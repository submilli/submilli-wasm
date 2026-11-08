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
pub(crate) mod epoch;
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
pub(crate) mod native;
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
use crate::func::Func;
use crate::instance::Instance;
use crate::module::code::Code;
use crate::module::op::Op;
use crate::store::{Store, StoreInner};
use crate::trap::Trap;
use crate::value::{Val, ValType};
use crate::Result;

use self::frame::{Delimiter, Frame};
pub(crate) use outcome::Outcome;
use outcome::{CallReq, ResolvedCall, StepOutcome, CALL_INTERNAL, CONTROL};

/// Transient interpreter state for one top-level call. Self-contained: it owns its
/// operand/frame stacks and holds no borrow into the `Store` across an [`Outcome`]
/// suspend, so it can be *parked* between resumptions — the basis for async,
/// where the driver awaits with this state at rest. See ARCHITECTURE §2.4.
#[derive(Debug)]
pub(crate) struct Execution {
    /// The operand buffer: every frame's locals and operands, each slot its untyped cell plus its
    /// reference tag ([`cell::RefTag`]). The cells are untyped, so the tag (the root shadow) is
    /// how a tracing collector recovers operand/local roots (ARCHITECTURE §7/§14, #27g); keeping
    /// them in one record makes every slot access a single bounds check.
    slots: Vec<cell::TaggedCell>,
    frames: Vec<Frame>,
    /// Native-stack bytes charged for the innermost live call boundary: what the nested entries
    /// below it used on the *native* Rust stack (measured, see [`native`]) plus
    /// [`HOST_REENTRY_RESERVE`]; `0` for an outermost call. Charged in `stack_bytes` so
    /// `max_wasm_stack` bounds host↔wasm recursion below native exhaustion (#30). Set in
    /// `enter_call`; each delimiter frame saves the value it replaced, restored when its call ends.
    native_charge: usize,
    /// The top frame's `operand_base` (`usize::MAX` with no frame), re-synced on every frame
    /// push/pop so the per-op underflow check is one compare instead of a frame-stack walk.
    floor: usize,
    /// A run-loop request left by the op `step` just executed (see [`outcome::CONTROL`]); `None`
    /// everywhere outside the dispatch loop.
    control: Option<StepOutcome>,
    /// The current stack window's top: where a stack-window op (GC, SIMD, a host call's args)
    /// pops/pushes. Set by `enter_window`; slot-addressed ops never touch it.
    sp: usize,
    /// The last memory-0 resolution, `(instance index, arena index)`: loads and stores of the same
    /// instance's memory 0 reuse it instead of looking the memory up through the instance (see
    /// `memory_index`). It can't go stale: an execution never leaves its store (so a hit may skip
    /// the store check), instance indices are never reused, and an instance's memories are fixed
    /// at instantiation. Reset per top-level call all the same (`host::enter`).
    mem0: (u32, u32),
    /// The run loop's [`Code`] handle, kept while it is suspended so resuming needs no store
    /// lookup — a hint only: the loop re-points it at the actual top frame (`top_into`). Cleared
    /// when a call completes, so a reused execution doesn't keep a module alive.
    code: Option<Code>,
    /// The engine's `max_wasm_stack` (bytes), cached at entry so every frame push is charged
    /// against it; `0` (fail closed) until an entry sets it.
    stack_limit: usize,
    /// Set when execution resumes after a serviced epoch deadline: the op at the resume point
    /// already passed its gates (fuel charged, deadline handled), so it runs before the next gate
    /// check — otherwise a callback returning `Continue(0)` would refire at the same op forever.
    gates_passed: bool,
}

/// Guest calls between amortized GC-pressure mailbox checks (see `run`): pressure is
/// advisory, so µs-scale response latency is fine, and anchoring to calls keeps the per-op
/// path clean even for threshold-configured engines. Non-calling guests are covered by the
/// allocation/reservation path (allocators respond immediately via `GcGrow`) and by the
/// async-host-call safepoint; a non-calling, non-allocating spin adds no GC pressure.
const GC_CHECK_INTERVAL: u32 = 1024;

/// A `Store<T>` with its data type erased, for the dispatch loop (see [`Execution::run`]): the
/// loop runs on the untyped [`StoreInner`]; a sync host call is the one thing that needs `T`.
trait HostStore {
    fn inner(&mut self) -> &mut StoreInner;
    /// Runs sync host function `func` (its arguments in `exec`'s current window) loop-resident.
    fn call_host(
        &mut self,
        exec: &mut Execution,
        func: Func,
        instance: Instance,
        stop_depth: usize,
    ) -> Result<()>;
}

impl<T> HostStore for Store<T> {
    fn inner(&mut self) -> &mut StoreInner {
        &mut self.inner
    }

    fn call_host(
        &mut self,
        exec: &mut Execution,
        func: Func,
        instance: Instance,
        stop_depth: usize,
    ) -> Result<()> {
        exec.invoke_host(self, func, instance, stop_depth)
    }
}

/// Headroom charged on top of a nested entry's measured native-stack use (see `native_charge`):
/// the measurement is taken at entry, before the entered call pushes its own interpreter frames —
/// about one host crossing's worth (~2.5 KB optimized; ~11 KB unoptimized, where only the
/// innermost entry's frames go uncounted, never a multiple).
const HOST_REENTRY_RESERVE: usize = 4 << 10;

/// Bytes charged per operand slot against `max_wasm_stack`: a wasm value's native spill slot, as
/// wasmtime would spend — not our cell width (16 B under `simd`) plus shadow byte, which would cap
/// a single frame well below wasmparser's 50k-locals limit at the default budget. Real heap use is
/// thus at most ~2× the configured bytes: still a linear bound.
const SLOT_CHARGE: usize = 8;

/// `Execution::mem0` with nothing cached (no instance has index `u32::MAX`).
const NO_MEM0: (u32, u32) = (u32::MAX, u32::MAX);

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
            slots: Vec::new(),
            frames: Vec::new(),
            native_charge: 0,
            floor: usize::MAX,
            control: None,
            sp: 0,
            mem0: NO_MEM0,
            code: None,
            stack_limit: 0,
            gates_passed: false,
        }
    }
}

impl Execution {
    pub(crate) fn is_empty(&self) -> bool {
        self.frames.is_empty() && self.slots.is_empty()
    }

    /// Whether nothing at all survives from earlier calls — the condition for a store to reuse
    /// this execution's allocations ([`is_empty`](Self::is_empty) plus the transient fields).
    pub(crate) fn is_pristine(&self) -> bool {
        self.is_empty() && self.native_charge == 0 && self.control.is_none() && self.code.is_none()
    }

    /// Swaps everything but `control` with `other` — the per-host-call park/unpark
    /// (`StoreInner::swap_exec`). `control` is `None` on both sides there (the run loop takes it
    /// before acting on it), so leaving it in place changes nothing, but skips 72 of the struct's
    /// 184 bytes on the four copies a host call makes (~5% of a host call). The destructuring is
    /// exhaustive: a new field fails to compile until it is swapped here too.
    pub(crate) fn swap_parked(&mut self, other: &mut Execution) {
        debug_assert!(self.control.is_none() && other.control.is_none());
        let Execution {
            slots,
            frames,
            native_charge,
            floor,
            control: _,
            sp,
            mem0,
            code,
            stack_limit,
            gates_passed,
        } = self;
        std::mem::swap(slots, &mut other.slots);
        std::mem::swap(frames, &mut other.frames);
        std::mem::swap(native_charge, &mut other.native_charge);
        std::mem::swap(floor, &mut other.floor);
        std::mem::swap(sp, &mut other.sp);
        std::mem::swap(mem0, &mut other.mem0);
        std::mem::swap(code, &mut other.code);
        std::mem::swap(stack_limit, &mut other.stack_limit);
        std::mem::swap(gates_passed, &mut other.gates_passed);
    }

    /// Heap bytes held by the stacks' capacity (bounds what a store keeps between calls).
    pub(crate) fn retained_bytes(&self) -> usize {
        let slots = self
            .slots
            .capacity()
            .saturating_mul(std::mem::size_of::<cell::TaggedCell>());
        let frames = self
            .frames
            .capacity()
            .saturating_mul(std::mem::size_of::<Frame>());
        slots.saturating_add(frames)
    }

    /// Enters a (sub-)call on the shared stacks: a [`Delimiter`] boundary, the call's `args`, then
    /// the entry frame. The boundary sits at the current frame depth; the entered call runs with a
    /// `stop_depth` one above it (so the parked outer frames stay untouched). `native` is the
    /// entry's native-stack charge (see `native_charge`).
    fn enter_call(
        &mut self,
        (delim, native): (Delimiter, usize),
        instance: Instance,
        func_index: u32,
        code: Code,
        args: &[Val],
    ) -> Result<()> {
        let value_base = self.slots.len();
        let frame_base = self.frames.len();
        let saved_charge = self.native_charge;
        self.native_charge = saved_charge.max(native);
        if let Err(error) = self.push_delimiter(delim, instance, &code, saved_charge) {
            self.native_charge = saved_charge;
            return Err(error);
        }
        self.slots.extend(args.iter().map(|&v| cell::TaggedCell {
            cell: stack::encode_val(v),
            tag: cell::RefTag::of_val(&v),
        }));
        if let Err(error) = self.push_call(instance, func_index, &code, value_base) {
            self.slots.truncate(value_base);
            self.frames.truncate(frame_base);
            self.sync_floor();
            self.native_charge = saved_charge;
            return Err(error);
        }
        self.code = Some(code);
        Ok(())
    }

    /// Restores the native charge the call whose boundary sits at `stop_depth` replaced, as its
    /// frames are about to be truncated away. Mirrors `enter_call`.
    fn release_reentry(&mut self, stop_depth: usize) -> Result<()> {
        match self.frames.get(stop_depth) {
            Some(frame) if frame.delimiter.is_some() => {
                self.native_charge = frame.native_saved as usize;
                Ok(())
            }
            _ => Err(InternalError::FrameStack("missing call boundary delimiter").into()),
        }
    }

    /// On a finished (sub-)call: decodes its results (everything above `value_base`, against the
    /// entry function's result types — the cells are untyped) into `out` and restores the shared
    /// stacks to the parked outer state (frames back to `stop_depth`).
    fn take_results(
        &mut self,
        value_base: usize,
        stop_depth: usize,
        result_tys: &[ValType],
        out: &mut [Val],
    ) -> Result<()> {
        if out.len() != result_tys.len() {
            return Err(
                InternalError::ResultShape("results buffer does not match the call").into(),
            );
        }
        if value_base > self.slots.len() || stop_depth > self.frames.len() {
            return Err(InternalError::FrameStack("invalid completed call boundary").into());
        }
        self.validate_result_tags(value_base, result_tys)?;
        self.code = None;
        self.release_reentry(stop_depth)?;
        let cells = self.slots.get(value_base..).unwrap_or_default();
        for ((slot, ty), out) in cells.iter().zip(result_tys).zip(out) {
            *out = stack::decode_val(slot.cell, ty);
        }
        self.slots.truncate(value_base);
        self.frames.truncate(stop_depth);
        self.sync_floor();
        Ok(())
    }

    /// On a trap / uncaught exception in a (sub-)call: discards its frames and operands, restoring
    /// the shared stacks to the parked outer state so that call resumes pristine.
    pub(crate) fn discard_to(&mut self, value_base: usize, stop_depth: usize) -> Result<()> {
        if value_base > self.slots.len() || stop_depth > self.frames.len() {
            return Err(InternalError::FrameStack("invalid failed call boundary").into());
        }
        self.release_reentry(stop_depth)?;
        self.code = None;
        self.slots.truncate(value_base);
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
    fn stack_bytes(&self) -> usize {
        self.stack_bytes_for(self.live_top(), self.frames.len())
    }

    /// [`stack_bytes`](Self::stack_bytes) for a prospective `slots`-slot buffer and `frames`
    /// frames. Saturating: a footprint too large to count is over any limit (branch-free on the
    /// call path).
    #[inline]
    fn stack_bytes_for(&self, slots: usize, frames: usize) -> usize {
        Self::heap_bytes_for(slots, frames).saturating_add(self.native_charge)
    }

    /// The heap stacks' share of [`stack_bytes`](Self::stack_bytes) (no native charge).
    #[inline]
    fn heap_bytes_for(slots: usize, frames: usize) -> usize {
        slots
            .saturating_mul(SLOT_CHARGE)
            .saturating_add(frames.saturating_mul(std::mem::size_of::<Frame>()))
    }

    /// The heap stacks' current footprint — what a parked execution adds to a nested direct host
    /// call's native use (see `native`).
    pub(crate) fn heap_stack_bytes(&self) -> usize {
        Self::heap_bytes_for(self.live_top(), self.frames.len())
    }

    /// Runs frames until the stack falls back to `stop_depth` (the boundary this `run` is
    /// responsible for): `0` for a top-level call, or the parked outer depth for a host re-entry.
    ///
    /// The dispatch loop behind it is *not* generic over the store's data type: it takes the store
    /// as a [`HostStore`] trait object, needed only to run a sync host call (which wants a
    /// `Caller<T>`) loop-resident. So the loop is compiled once, in this crate, at this crate's
    /// opt level — not in each embedder's crate at theirs, where a mixed profile (this crate
    /// optimized, the embedder at 0) once put the forced inlines into an unoptimized loop: ~50 KB
    /// per host re-entry (~100 KB with the arithmetic wrappers forced too).
    fn run<T>(&mut self, store: &mut Store<T>, stop_depth: usize) -> Result<Outcome> {
        self.run_erased(store, stop_depth)
    }

    /// [`run`](Self::run) on the type-erased store. Not `#[inline]`, and only `const`-generic, so
    /// it and the loop are codegened here (see `run`).
    fn run_erased(&mut self, store: &mut dyn HostStore, stop_depth: usize) -> Result<Outcome> {
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
        if self.stack_bytes() >= self.stack_limit {
            let ip = self.frames.last().map_or(0, |f| f.ip);
            return Err(self.attach_trap_backtrace(store.inner(), Trap::StackOverflow.into(), ip));
        }
        let engine = store.inner().engine();
        let (fuel_enabled, epoch_enabled) = (engine.consume_fuel(), engine.epoch_interruption());
        // Monomorphize the dispatch loop on whether a *per-op* gate is live (fuel metering /
        // epoch deadlines), so other configurations run a loop with no gate branch at all. The
        // engine-wide GC-pressure mailbox is deliberately NOT a per-op gate: it is advisory
        // (microseconds of latency are fine), so the loop honors it every
        // [`GC_CHECK_INTERVAL`] guest calls instead — a threshold-configured deployment keeps
        // the fast loop. (Its predecessor armed per-op on `is_collecting()` alone, taxing every
        // default engine — see PERF-NOTES §14.)
        if fuel_enabled || epoch_enabled {
            self.dispatch::<true>(store, stop_depth, (fuel_enabled, epoch_enabled))
        } else {
            self.dispatch::<false>(store, stop_depth, (false, false))
        }
    }

    /// Runs the dispatch loop with the running frame's [`Code`] handle: the one stashed when the
    /// loop last suspended (re-pointed at the top frame), else looked up from its instance.
    fn dispatch<const GATED: bool>(
        &mut self,
        store: &mut dyn HostStore,
        stop_depth: usize,
        gates: (bool, bool),
    ) -> Result<Outcome> {
        let mut code = match self.code.take() {
            Some(code) => code,
            None => self.top_frame()?.code_in(store.inner())?,
        };
        let outcome = self.dispatch_loop::<GATED>(store, stop_depth, gates, &mut code);
        self.code = Some(code);
        outcome
    }

    #[allow(clippy::too_many_lines)] // the resumable dispatch loop; arms are short
    fn dispatch_loop<const GATED: bool>(
        &mut self,
        store: &mut dyn HostStore,
        stop_depth: usize,
        gates: (bool, bool),
        code: &mut Code,
    ) -> Result<Outcome> {
        // The loop runs on the untyped store; only a sync host call needs `store` itself, after
        // which `inner` is re-derived.
        let mut inner = store.inner();
        // Amortized GC-pressure safepoint: armed only when the engine-wide axis exists, and
        // anchored to guest *calls* (zero per-op cost; call sites are already heavyweight).
        let gc_watch = inner.gc.is_collecting() && inner.engine().gc_memory_threshold().is_some();
        let mut gc_countdown: u32 = GC_CHECK_INTERVAL;
        let (mut func, mut ip, mut base, mut instance) = self.top_into(inner, code)?;
        // Two-level loop: the outer level re-derives the current frame's `ops` slice whenever the
        // frame changes; the inner level then fetches each op with a single `.get` — no per-op
        // `Arc`→`Vec`→len pointer chase, and the `None` case doubles as the end-of-function return
        // (so there is no separate bounds check anywhere on the hot path).
        'frame: loop {
            let ops = code.ops_in(func.ops)?;
            loop {
                let Some(op) = ops.get(ip as usize) else {
                    if self.do_return(func.n_results, stop_depth)? {
                        return Ok(Outcome::Finished);
                    }
                    (func, ip, base, instance) = self.top_into(inner, code)?;
                    continue 'frame;
                };
                if GATED {
                    if let Some(out) = self.service_gates(inner, gates, ip)? {
                        return Ok(out);
                    }
                }
                let next = match self.step(inner, code, op, ip + 1, base, instance) {
                    Ok(next) => next,
                    Err(e) => match self.unwind(inner, e, ip, stop_depth) {
                        Ok(()) => {
                            (func, ip, base, instance) = self.top_into(inner, code)?;
                            continue 'frame;
                        }
                        Err(e) => return Err(self.attach_trap_backtrace(inner, e, ip)),
                    },
                };
                if next < CALL_INTERNAL {
                    ip = next;
                    continue;
                }
                if next == CALL_INTERNAL {
                    // The callee is read from the op itself: a stray marker (a corrupt branch
                    // target) from any other op fails closed.
                    let Op::CallInternal(index, s) = *op else {
                        return Err(InternalError::FrameStack("call marker without a call").into());
                    };
                    match self.call_internal(code, instance, index, step::slot(base, s), ip + 1) {
                        Ok((f, b)) => (func, ip, base) = (f, 0, b),
                        Err(e) => return Err(self.attach_trap_backtrace(inner, e, ip)),
                    }
                    if gc_watch {
                        gc_countdown -= 1;
                        if gc_countdown == 0 {
                            gc_countdown = GC_CHECK_INTERVAL;
                            self.service_gc_pressure(inner)?;
                        }
                    }
                    continue 'frame;
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
                        // The callee's frame is known: switch to it directly (no `top_into`). A
                        // frame push past `max_wasm_stack` traps (`StackOverflow`) at this call.
                        let callee = req.callee;
                        frame::switch_code(inner, code, &callee);
                        match self.push_call(callee.instance, callee.func_index, code, req.args_at)
                        {
                            Ok((f, b)) => (func, ip, base, instance) = (f, 0, b, callee.instance),
                            Err(e) => return Err(self.attach_trap_backtrace(inner, e, ip)),
                        }
                        if gc_watch {
                            gc_countdown -= 1;
                            if gc_countdown == 0 {
                                gc_countdown = GC_CHECK_INTERVAL;
                                self.service_gc_pressure(inner)?;
                            }
                        }
                        continue 'frame;
                    }
                    // Tail call (#39): replace the current frame — `return_from` moves the args to
                    // the frame's base and pops it, then `push_call` lays the callee there.
                    StepOutcome::TailCall(req) => {
                        let dst = self.frame_base()?;
                        let callee = req.callee;
                        self.return_from(req.args_at, callee.n_params as usize, stop_depth)?;
                        frame::switch_code(inner, code, &callee);
                        match self.push_call(callee.instance, callee.func_index, code, dst) {
                            Ok((f, b)) => (func, ip, base, instance) = (f, 0, b, callee.instance),
                            Err(e) => return Err(self.attach_suspension_backtrace(inner, e)),
                        }
                        if gc_watch {
                            gc_countdown -= 1;
                            if gc_countdown == 0 {
                                gc_countdown = GC_CHECK_INTERVAL;
                                self.service_gc_pressure(inner)?;
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
                        store.call_host(self, host_fn, host_inst, stop_depth)?;
                        inner = store.inner();
                        if self.frames.len() == stop_depth {
                            return Ok(Outcome::Finished);
                        }
                        (func, ip, base, instance) = self.top_into(inner, code)?;
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
                        store.call_host(self, host_fn, host_inst, stop_depth)?;
                        inner = store.inner();
                        if self.frames.len() == stop_depth {
                            return Ok(Outcome::Finished);
                        }
                        (func, ip, base, instance) = self.top_into(inner, code)?;
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
