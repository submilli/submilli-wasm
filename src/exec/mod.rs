//! The interpreter run loop and transient execution state.
//!
//! Each frame holds its code as `Arc<CompiledFunc>` so the loop reads ops while mutating the
//! value/frame stacks and the store. `step` returns an owned outcome. See ARCHITECTURE §7.

// Panic-safety gate for the exec hot path (#33). A validated guest must never panic the interpreter
// (a panic = whole-process DoS under multi-tenant). These lint levels cascade to every `src/exec/*`
// submodule, so accidental `panic!`/`todo!`/`unimplemented!` and unchecked indexing are caught here
// rather than in review. The per-op handler modules (`memory`/`table`/`gc*`/…) legitimately index
// *wasmparser-validated* module/instance index spaces and slices guarded by a just-checked bound, so
// each carries a documented file-level `#![allow(clippy::indexing_slicing)]`; the run-loop core
// (this file) and the numeric/conversion/host paths stay strict, so new unchecked indexing there is
// rejected. `clippy::unreachable` is intentionally *not* denied — `unreachable!()` is the sanctioned
// post-validation invariant assertion (same class as `expect`). `arithmetic_side_effects` is also not
// gated — the loop's `ip`/height/depth math is benign and the lint too noisy; numeric ops already use
// `wrapping_*`/`checked_*`, and the #35 fuzzer is the real net for arithmetic.
#![deny(
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]

mod arith;
mod call;
mod cast;
mod cell;
mod collect;
mod convert;
mod epoch;
mod exn;
mod frame;
mod gates;
mod gc;
mod gc_array;
pub(crate) mod guard;
pub(crate) mod host;
#[cfg(feature = "async")]
pub(crate) mod host_async;
mod memory;
mod numeric;
mod outcome;
mod ref_;
#[cfg(feature = "simd")]
mod simd;
mod stack;
mod step;
mod table;
pub(crate) mod trace;

use crate::error::InternalError;
use crate::instance::Instance;
use crate::module::code::Code;
use crate::store::Store;
use crate::trap::Trap;
use crate::value::{Val, ValType};
use crate::Result;

use self::frame::{Delimiter, Frame};
pub(crate) use outcome::Outcome;
use outcome::{CallReq, ResolvedCall, StepOutcome, CONTROL};

/// Transient interpreter state for one top-level call. Self-contained: it owns its
/// operand/frame stacks and holds no borrow into the `Store` across an [`Outcome`]
/// suspend, so it can be *parked* between resumptions — the basis for async,
/// where the driver awaits with this state at rest. See ARCHITECTURE §2.4.
#[derive(Debug)]
pub(crate) struct Execution {
    values: Vec<cell::Cell>,
    /// Per-operand-slot reference tag ([`cell::RefTag`]), parallel to `values` (same length, moved
    /// in lockstep). The cell stack is untyped, so this byte-shadow is how a tracing collector
    /// recovers operand/local roots (ARCHITECTURE §7/§14, #27g). Maintained unconditionally; cheap
    /// (1 B/slot) and far simpler to keep exhaustive than gating per-op.
    shadow: Vec<cell::RefTag>,
    frames: Vec<Frame>,
    /// Count of live [`Delimiter::HostReentry`] boundaries on `frames` — the host→wasm crossings
    /// nested on the *native* Rust stack. Each is charged [`HOST_REENTRY_RESERVE`] in `stack_bytes`
    /// so `max_wasm_stack` bounds re-entry depth below native exhaustion (#30). O(1): bumped in
    /// `enter_call`, dropped in `take_results`/`discard_to`.
    host_reentry_depth: usize,
    /// The top frame's `operand_base` (`usize::MAX` with no frame), re-synced on every frame
    /// push/pop so the per-op underflow check is one compare instead of a frame-stack walk.
    floor: usize,
    /// A run-loop request left by the op `step` just executed (see [`outcome::CONTROL`]); `None`
    /// everywhere outside the dispatch loop.
    control: Option<StepOutcome>,
    /// The current stack window's top: where a stack-window op (GC, SIMD, a host call's args)
    /// pops/pushes. Set by `enter_window`; slot-addressed ops never touch it.
    sp: usize,
    /// The engine's `max_wasm_stack` (bytes), cached at entry so every frame push is charged
    /// against it; `0` (fail closed) until an entry sets it.
    stack_limit: usize,
}

/// Guest calls between amortized GC-pressure mailbox checks (see `run`): pressure is
/// advisory, so µs-scale response latency is fine, and anchoring to calls keeps the per-op
/// path clean even for threshold-configured engines. Non-calling guests are covered by the
/// allocation/reservation path (allocators respond immediately via `GcGrow`) and by the
/// async-host-call safepoint; a non-calling, non-allocating spin adds no GC pressure.
const GC_CHECK_INTERVAL: u32 = 1024;

/// Native-stack bytes charged per host→wasm re-entry crossing against `max_wasm_stack` (#30). A
/// host fn that re-enters wasm (`Func::call`) nests interpreter + closure frames on the native Rust
/// stack — invisible to the heap-based wasm frame accounting (a wasm frame is ~24 B; the native
/// frame it begets is ~1 KB). Weighting each crossing with this reserve makes the single
/// `max_wasm_stack` budget bound crossings well below native exhaustion (~128 at the 512 KiB
/// default) without a separate knob, so unbounded host↔wasm ping-pong traps `StackOverflow` instead
/// of aborting the process.
const HOST_REENTRY_RESERVE: usize = 4096;

/// Bytes charged per operand slot against `max_wasm_stack`: a wasm value's native spill slot, as
/// wasmtime would spend — not our cell width (16 B under `simd`) plus shadow byte, which would cap
/// a single frame well below wasmparser's 50k-locals limit at the default budget. Real heap use is
/// thus at most ~2× the configured bytes: still a linear bound.
const SLOT_CHARGE: usize = 8;

// Parkability: keep `Execution` `Send` so async can await with it at rest. Compile-time check —
// a future non-`Send` field (e.g. an `Rc`) fails here rather than at an `.await`.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Execution>();
};

/// An empty execution — the starting state for a top-level call, and the placeholder left behind
/// (via `mem::take`) while the real execution is parked in the store across a host call.
impl Default for Execution {
    fn default() -> Self {
        Execution {
            values: Vec::new(),
            shadow: Vec::new(),
            frames: Vec::new(),
            host_reentry_depth: 0,
            floor: usize::MAX,
            control: None,
            sp: 0,
            stack_limit: 0,
        }
    }
}

impl Execution {
    pub(crate) fn is_empty(&self) -> bool {
        self.frames.is_empty() && self.values.is_empty() && self.shadow.is_empty()
    }

    /// Whether nothing at all survives from earlier calls — the condition for a store to reuse
    /// this execution's allocations ([`is_empty`](Self::is_empty) plus the transient fields).
    pub(crate) fn is_pristine(&self) -> bool {
        self.is_empty() && self.host_reentry_depth == 0 && self.control.is_none()
    }

    /// Heap bytes held by the stacks' capacity (bounds what a store keeps between calls).
    pub(crate) fn retained_bytes(&self) -> usize {
        let values = self
            .values
            .capacity()
            .saturating_mul(std::mem::size_of::<cell::Cell>());
        let frames = self
            .frames
            .capacity()
            .saturating_mul(std::mem::size_of::<Frame>());
        values
            .saturating_add(self.shadow.capacity())
            .saturating_add(frames)
    }

    /// Enters a (sub-)call on the shared stacks: a [`Delimiter`] boundary, the call's `args`, then
    /// the entry frame. The boundary sits at the current frame depth; the entered call runs with a
    /// `stop_depth` one above it (so the parked outer frames stay untouched).
    fn enter_call(
        &mut self,
        delim: Delimiter,
        instance: Instance,
        func_index: u32,
        code: Code,
        args: Vec<Val>,
    ) -> Result<()> {
        let value_base = self.values.len();
        let frame_base = self.frames.len();
        let old_reentry_depth = self.host_reentry_depth;
        if delim == Delimiter::HostReentry {
            self.host_reentry_depth = self
                .host_reentry_depth
                .checked_add(1)
                .ok_or(InternalError::FrameStack("host re-entry depth overflow"))?;
        }
        if let Err(error) = self.push_delimiter(delim, instance, code.clone()) {
            self.host_reentry_depth = old_reentry_depth;
            return Err(error);
        }
        self.shadow.extend(args.iter().map(cell::RefTag::of_val));
        self.values.extend(args.into_iter().map(cell::encode));
        if let Err(error) = self.push_call(instance, func_index, code, value_base) {
            self.values.truncate(value_base);
            self.shadow.truncate(value_base);
            self.frames.truncate(frame_base);
            self.sync_floor();
            self.host_reentry_depth = old_reentry_depth;
            return Err(error);
        }
        Ok(())
    }

    /// Drops this call's `HostReentry` reserve (if its boundary at `stop_depth` was one) as its
    /// frames are about to be truncated away. Mirrors the `enter_call` bump.
    fn release_reentry(&mut self, stop_depth: usize) -> Result<()> {
        match self
            .frames
            .get(stop_depth)
            .and_then(|frame| frame.delimiter)
        {
            Some(Delimiter::HostReentry) => {
                self.host_reentry_depth = self
                    .host_reentry_depth
                    .checked_sub(1)
                    .ok_or(InternalError::FrameStack("host re-entry depth underflow"))?;
            }
            Some(Delimiter::TopLevel) => {}
            None => return Err(InternalError::FrameStack("missing call boundary delimiter").into()),
        }
        Ok(())
    }

    /// On a finished (sub-)call: splits off its result cells (everything above `value_base`) and
    /// restores the shared stacks to the parked outer state (frames back to `stop_depth`).
    fn take_results(
        &mut self,
        value_base: usize,
        stop_depth: usize,
        result_tys: &[ValType],
    ) -> Result<Vec<cell::Cell>> {
        if self.values.len() != self.shadow.len()
            || value_base > self.values.len()
            || stop_depth > self.frames.len()
        {
            return Err(InternalError::FrameStack("invalid completed call boundary").into());
        }
        self.validate_result_tags(value_base, result_tys)?;
        self.release_reentry(stop_depth)?;
        let results = self.values.split_off(value_base);
        self.shadow.truncate(value_base);
        self.frames.truncate(stop_depth);
        self.sync_floor();
        Ok(results)
    }

    /// On a trap / uncaught exception in a (sub-)call: discards its frames and operands, restoring
    /// the shared stacks to the parked outer state so that call resumes pristine.
    pub(crate) fn discard_to(&mut self, value_base: usize, stop_depth: usize) -> Result<()> {
        if self.values.len() != self.shadow.len()
            || value_base > self.values.len()
            || value_base > self.shadow.len()
            || stop_depth > self.frames.len()
        {
            return Err(InternalError::FrameStack("invalid failed call boundary").into());
        }
        self.release_reentry(stop_depth)?;
        self.values.truncate(value_base);
        self.shadow.truncate(value_base);
        self.frames.truncate(stop_depth);
        self.sync_floor();
        Ok(())
    }

    /// Discards the innermost host-entered call after a contained host panic and reports its
    /// boundary so the parked outer execution can be restored.
    pub(crate) fn discard_current_call(&mut self) -> Result<(usize, usize)> {
        let (stop_depth, value_base) = self
            .frames
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, frame)| {
                frame
                    .delimiter
                    .is_some()
                    .then_some((depth, frame.locals_base as usize))
            })
            .ok_or(InternalError::FrameStack(
                "missing call boundary during panic cleanup",
            ))?;
        self.discard_to(value_base, stop_depth)?;
        Ok((value_base, stop_depth))
    }

    /// Estimated byte footprint of the wasm execution stacks, checked against
    /// `Config::max_wasm_stack` at each frame push to bound runaway recursion (slots charged at
    /// [`SLOT_CHARGE`]).
    #[inline]
    fn stack_bytes(&self) -> Result<usize> {
        self.stack_bytes_for(self.values.len(), self.frames.len())
    }

    /// [`stack_bytes`](Self::stack_bytes) for a prospective `values`-slot buffer and `frames`
    /// frames.
    #[inline]
    fn stack_bytes_for(&self, values: usize, frames: usize) -> Result<usize> {
        let values = values
            .checked_mul(SLOT_CHARGE)
            .ok_or(InternalError::FrameStack(
                "operand stack byte size overflow",
            ))?;
        let frames = frames
            .checked_mul(std::mem::size_of::<Frame>())
            .ok_or(InternalError::FrameStack("frame stack byte size overflow"))?;
        let reentry = self
            .host_reentry_depth
            .checked_mul(HOST_REENTRY_RESERVE)
            .ok_or(InternalError::FrameStack(
                "host re-entry byte size overflow",
            ))?;
        values
            .checked_add(frames)
            .and_then(|n| n.checked_add(reentry))
            .ok_or_else(|| InternalError::FrameStack("execution stack byte size overflow").into())
    }

    /// Runs frames until the stack falls back to `stop_depth` (the boundary this `run` is
    /// responsible for): `0` for a top-level call, or the parked outer depth for a host re-entry.
    /// Generic over the store's data type so *sync host calls run loop-resident* — the callback
    /// needs a `Caller<T>`; only async host fns still suspend out through an [`Outcome`].
    fn run<T>(&mut self, store: &mut Store<T>, stop_depth: usize) -> Result<Outcome> {
        // A `return_call` to a host fn from the boundary frame pops it, then the host pushes its
        // results; re-entering here at `stop_depth` means the call finished (#39).
        if self.frames.len() == stop_depth {
            return Ok(Outcome::Finished);
        }
        // Gate re-entry on the inherited budget: the per-`Call` check in the loop only bounds
        // wasm recursion *within* this segment, so a re-entered call whose body does no wasm call
        // (just a host call) would never be checked. With the host-crossing reserve folded into
        // `stack_bytes`, this entry check makes even pure host↔wasm ping-pong trap here rather
        // than abort the native stack (#30).
        if self.stack_bytes()? >= self.stack_limit {
            let ip = self.frames.last().map_or(0, |f| f.ip);
            return Err(self.attach_trap_backtrace(&store.inner, Trap::StackOverflow.into(), ip));
        }
        let fuel_enabled = store.inner.engine().consume_fuel();
        let epoch_enabled = store.inner.engine().epoch_interruption();
        // Monomorphize the dispatch loop on whether a *per-op* gate is live (fuel metering /
        // epoch deadlines), so other configurations run a loop with no gate branch at all. The
        // engine-wide GC-pressure mailbox is deliberately NOT a per-op gate: it is advisory
        // (microseconds of latency are fine), so the loop honors it every
        // [`GC_CHECK_INTERVAL`] guest calls instead — a threshold-configured deployment keeps
        // the fast loop. (Its predecessor armed per-op on `is_collecting()` alone, taxing every
        // default engine — see PERF-NOTES §14.)
        if fuel_enabled || epoch_enabled {
            self.dispatch::<T, true>(store, stop_depth, (fuel_enabled, epoch_enabled))
        } else {
            self.dispatch::<T, false>(store, stop_depth, (false, false))
        }
    }

    #[allow(clippy::too_many_lines)] // the resumable dispatch loop; arms are short
    fn dispatch<T, const GATED: bool>(
        &mut self,
        store: &mut Store<T>,
        stop_depth: usize,
        gates: (bool, bool),
    ) -> Result<Outcome> {
        // Amortized GC-pressure safepoint: armed only when the engine-wide axis exists, and
        // anchored to guest *calls* (zero per-op cost; call sites are already heavyweight).
        let gc_watch =
            store.inner.gc.is_collecting() && store.inner.engine().gc_memory_threshold().is_some();
        let mut gc_countdown: u32 = GC_CHECK_INTERVAL;
        let (mut code, mut func, mut ip, mut base, mut instance) = self.top()?;
        // Two-level loop: the outer level re-derives the current frame's `ops` slice whenever the
        // frame changes; the inner level then fetches each op with a single `.get` — no per-op
        // `Arc`→`Vec`→len pointer chase, and the `None` case doubles as the end-of-function return
        // (so there is no separate bounds check anywhere on the hot path).
        'frame: loop {
            let ops = code.ops_in(func.ops);
            loop {
                let Some(op) = ops.get(ip as usize) else {
                    if self.do_return(func.n_results, stop_depth)? {
                        return Ok(Outcome::Finished);
                    }
                    (func, ip, base, instance) = self.top_into(&mut code)?;
                    continue 'frame;
                };
                if GATED {
                    if let Some(out) = self.service_gates(&mut store.inner, gates, ip)? {
                        return Ok(out);
                    }
                }
                let next = match self.step(&mut store.inner, &code, op, ip + 1, base, instance) {
                    Ok(next) => next,
                    Err(e) => match self.unwind(&mut store.inner, e, ip, stop_depth) {
                        Ok(()) => {
                            (func, ip, base, instance) = self.top_into(&mut code)?;
                            continue 'frame;
                        }
                        Err(e) => return Err(self.attach_trap_backtrace(&store.inner, e, ip)),
                    },
                };
                if next != CONTROL {
                    ip = next;
                    continue;
                }
                // `CONTROL` without a pending outcome is an interpreter bug: fail closed.
                let outcome = self.control.take().ok_or(InternalError::FrameStack(
                    "control marker without a pending outcome",
                ))?;
                match outcome {
                    StepOutcome::Call(req) => {
                        let caller = self
                            .frames
                            .last_mut()
                            .ok_or(InternalError::FrameStack("missing caller frame"))?;
                        caller.ip = req.return_ip;
                        // The arguments become the callee's locals; the caller's live temporaries
                        // end below them.
                        caller.live_end = u32::try_from(req.args_at)
                            .map_err(|_| InternalError::FrameStack("operand index exceeds u32"))?;
                        // A frame push past `max_wasm_stack` traps (`StackOverflow`) at this call.
                        if let Err(e) =
                            self.push_call(req.instance, req.func_index, req.code, req.args_at)
                        {
                            return Err(self.attach_trap_backtrace(&store.inner, e, ip));
                        }
                        (func, ip, base, instance) = self.top_into(&mut code)?;
                        if gc_watch {
                            gc_countdown -= 1;
                            if gc_countdown == 0 {
                                gc_countdown = GC_CHECK_INTERVAL;
                                self.service_gc_pressure(&mut store.inner)?;
                            }
                        }
                        continue 'frame;
                    }
                    // Tail call (#39): replace the current frame — `return_from` moves the args to
                    // the frame's base and pops it, then `push_call` lays the callee there.
                    StepOutcome::TailCall(req) => {
                        let dst = self.frame_base()?;
                        let n_params = req.code.n_params() as usize;
                        self.return_from(req.args_at, n_params, stop_depth)?;
                        if let Err(e) = self.push_call(req.instance, req.func_index, req.code, dst)
                        {
                            return Err(self.attach_suspension_backtrace(&store.inner, e));
                        }
                        (func, ip, base, instance) = self.top_into(&mut code)?;
                        if gc_watch {
                            gc_countdown -= 1;
                            if gc_countdown == 0 {
                                gc_countdown = GC_CHECK_INTERVAL;
                                self.service_gc_pressure(&mut store.inner)?;
                            }
                        }
                        continue 'frame;
                    }
                    // Tail call to a host fn (#39): pop the current frame, run the host call
                    // loop-resident; if that emptied this segment, its results are the call's.
                    StepOutcome::TailHostCall {
                        func: host_fn,
                        instance: host_inst,
                        args_at,
                        n_params,
                    } => {
                        let dst = self.frame_base()?;
                        self.return_from(args_at, n_params, stop_depth)?;
                        self.enter_window(dst.saturating_add(n_params))?;
                        self.invoke_host(store, host_fn, host_inst, stop_depth)?;
                        if self.frames.len() == stop_depth {
                            return Ok(Outcome::Finished);
                        }
                        (func, ip, base, instance) = self.top_into(&mut code)?;
                        continue 'frame;
                    }
                    #[cfg(feature = "async")]
                    StepOutcome::TailHostAsyncCall {
                        func,
                        instance,
                        args_at,
                        n_params,
                    } => {
                        let dst = self.frame_base()?;
                        self.return_from(args_at, n_params, stop_depth)?;
                        self.enter_window(dst.saturating_add(n_params))?;
                        return Ok(Outcome::HostAsync { func, instance });
                    }
                    // Sync host call, loop-resident: the callback runs right here (the execution
                    // is swap-parked around it inside `invoke_host`, so re-entrant `Func::call`s
                    // still share these stacks). On return the frame stack is normally untouched
                    // (resume at `return_ip`); a host *throw* may have unwound to a handler, in
                    // which case the resume point moved — re-derive from the top frame.
                    StepOutcome::HostCall {
                        func: host_fn,
                        instance: host_inst,
                        top,
                        return_ip,
                    } => {
                        self.frames
                            .last_mut()
                            .ok_or(InternalError::FrameStack("missing caller frame"))?
                            .ip = return_ip;
                        self.enter_window(top)?; // the host pops its args from here
                        self.invoke_host(store, host_fn, host_inst, stop_depth)?;
                        if self.frames.len() == stop_depth {
                            return Ok(Outcome::Finished);
                        }
                        (func, ip, base, instance) = self.top_into(&mut code)?;
                        continue 'frame;
                    }
                    #[cfg(feature = "async")]
                    StepOutcome::HostAsyncCall {
                        func,
                        instance,
                        top,
                        return_ip,
                    } => {
                        self.frames
                            .last_mut()
                            .ok_or(InternalError::FrameStack("missing caller frame"))?
                            .ip = return_ip;
                        self.enter_window(top)?; // the host pops its args from here
                        return Ok(Outcome::HostAsync { func, instance });
                    }
                    StepOutcome::Grow {
                        memory,
                        delta,
                        at,
                        return_ip,
                    } => {
                        self.frames
                            .last_mut()
                            .ok_or(InternalError::FrameStack("missing caller frame"))?
                            .ip = return_ip;
                        return Ok(Outcome::Grow { memory, delta, at });
                    }
                    StepOutcome::TableGrow {
                        table,
                        delta,
                        init,
                        at,
                        return_ip,
                    } => {
                        self.frames
                            .last_mut()
                            .ok_or(InternalError::FrameStack("missing caller frame"))?
                            .ip = return_ip;
                        return Ok(Outcome::TableGrow {
                            table,
                            delta,
                            init,
                            at,
                        });
                    }
                    StepOutcome::GcGrow {
                        reserved_target,
                        bytes_needed,
                        return_ip,
                    } => {
                        self.frames
                            .last_mut()
                            .ok_or(InternalError::FrameStack("missing caller frame"))?
                            .ip = return_ip;
                        return Ok(Outcome::GcGrow {
                            reserved_target,
                            bytes_needed,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
