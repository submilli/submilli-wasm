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
mod gc;
mod gc_array;
pub(crate) mod guard;
pub(crate) mod host;
mod memory;
mod numeric;
mod outcome;
mod ref_;
#[cfg(feature = "simd")]
mod simd;
mod step;
mod table;
pub(crate) mod trace;

use std::sync::Arc;

use crate::instance::Instance;
use crate::module::op::{BranchTarget, CompiledFunc};
use crate::store::{FuelStep, StoreInner};
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

use self::frame::{Delimiter, Frame};
pub(crate) use outcome::Outcome;
use outcome::{CallReq, ResolvedCall, StepOutcome};

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
}

/// Native-stack bytes charged per host→wasm re-entry crossing against `max_wasm_stack` (#30). A
/// host fn that re-enters wasm (`Func::call`) nests interpreter + closure frames on the native Rust
/// stack — invisible to the heap-based wasm frame accounting (a wasm frame is ~24 B; the native
/// frame it begets is ~1 KB). Weighting each crossing with this reserve makes the single
/// `max_wasm_stack` budget bound crossings well below native exhaustion (~128 at the 512 KiB
/// default) without a separate knob, so unbounded host↔wasm ping-pong traps `StackOverflow` instead
/// of aborting the process.
const HOST_REENTRY_RESERVE: usize = 4096;

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
        }
    }
}

impl Execution {
    /// Enters a (sub-)call on the shared stacks: a [`Delimiter`] boundary, the call's `args`, then
    /// the entry frame. The boundary sits at the current frame depth; the entered call runs with a
    /// `stop_depth` one above it (so the parked outer frames stay untouched).
    fn enter_call(
        &mut self,
        delim: Delimiter,
        instance: Instance,
        func_index: u32,
        code: Arc<CompiledFunc>,
        args: Vec<Val>,
    ) {
        if delim == Delimiter::HostReentry {
            self.host_reentry_depth += 1;
        }
        self.push_delimiter(delim, instance, code.clone());
        self.shadow.extend(args.iter().map(cell::RefTag::of_val));
        self.values.extend(args.into_iter().map(cell::encode));
        self.push_call(instance, func_index, code);
    }

    /// Drops this call's `HostReentry` reserve (if its boundary at `stop_depth` was one) as its
    /// frames are about to be truncated away. Mirrors the `enter_call` bump.
    fn release_reentry(&mut self, stop_depth: usize) {
        if self.frames.get(stop_depth).and_then(|f| f.delimiter) == Some(Delimiter::HostReentry) {
            self.host_reentry_depth -= 1;
        }
    }

    /// On a finished (sub-)call: splits off its result cells (everything above `value_base`) and
    /// restores the shared stacks to the parked outer state (frames back to `stop_depth`).
    fn take_results(&mut self, value_base: usize, stop_depth: usize) -> Vec<cell::Cell> {
        let results = self.values.split_off(value_base);
        self.shadow.truncate(value_base);
        self.release_reentry(stop_depth);
        self.frames.truncate(stop_depth);
        results
    }

    /// On a trap / uncaught exception in a (sub-)call: discards its frames and operands, restoring
    /// the shared stacks to the parked outer state so that call resumes pristine.
    fn discard_to(&mut self, value_base: usize, stop_depth: usize) {
        self.values.truncate(value_base);
        self.shadow.truncate(value_base);
        self.release_reentry(stop_depth);
        self.frames.truncate(stop_depth);
    }

    /// Estimated byte footprint of the wasm execution stacks, checked against
    /// `Config::max_wasm_stack` at each call to bound runaway recursion. An operand slot is now a
    /// fixed-width untyped [`cell::Cell`] (8 or 16 bytes; see `cell`), not the ~32-byte `Val`.
    fn stack_bytes(&self) -> usize {
        self.values.len() * std::mem::size_of::<cell::Cell>()
            + self.shadow.len() // 1 byte per operand slot (the GC root shadow)
            + self.frames.len() * std::mem::size_of::<Frame>()
            // Charge each host→wasm crossing its native-stack cost so re-entrancy is bounded by the
            // same budget as wasm recursion (#30) — the parked outer frames are already counted above.
            + self.host_reentry_depth * HOST_REENTRY_RESERVE
    }

    fn push_call(&mut self, instance: Instance, func_index: u32, code: Arc<CompiledFunc>) {
        let locals_base = self.values.len() as u32 - code.n_params;
        for ty in &code.local_types {
            self.push(Val::default_for(ty));
        }
        self.frames.push(Frame {
            code,
            ip: 0,
            locals_base,
            instance,
            func_index,
            delimiter: None,
        });
    }

    /// Pushes a [`Delimiter`] boundary marker (no operands, inert `code`/`instance` filler). The
    /// next `push_call` lays the entered function's frame directly above it; `run`/`unwind` stop at
    /// this frame's depth so the call below it stays parked and untouched.
    fn push_delimiter(&mut self, kind: Delimiter, instance: Instance, code: Arc<CompiledFunc>) {
        let locals_base = self.values.len() as u32;
        self.frames.push(Frame {
            code,
            ip: 0,
            locals_base,
            instance,
            func_index: 0,
            delimiter: Some(kind),
        });
    }

    /// Moves the top `keep` operands down over `pop` discarded ones, then jumps.
    fn take_branch(&mut self, t: &BranchTarget) {
        let len = self.values.len();
        let keep = t.keep as usize;
        let src = len - keep;
        let dst = src - t.pop as usize;
        self.values.copy_within(src..len, dst);
        self.values.truncate(dst + keep);
        // The root shadow moves in lockstep with the cell stack (same offsets/length).
        self.shadow.copy_within(src..len, dst);
        self.shadow.truncate(dst + keep);
    }

    fn top(&self) -> (Arc<CompiledFunc>, u32, u32, Instance) {
        let f = self.frames.last().expect("current frame");
        (f.code.clone(), f.ip, f.locals_base, f.instance)
    }

    /// Pops the current frame, moving its top `n_results` operands down to the
    /// frame base. Returns true if the frame stack has fallen back to `stop_depth`
    /// (this call's boundary) — i.e. the call this `run` was driving has finished.
    fn do_return(&mut self, n_results: u32, stop_depth: usize) -> bool {
        let frame = self.frames.pop().expect("frame stack underflow");
        let n = n_results as usize;
        let len = self.values.len();
        let dst = frame.locals_base as usize;
        self.values.copy_within(len - n..len, dst);
        self.values.truncate(dst + n);
        self.shadow.copy_within(len - n..len, dst);
        self.shadow.truncate(dst + n);
        self.frames.len() == stop_depth
    }

    /// Runs frames until the stack falls back to `stop_depth` (the boundary this `run` is
    /// responsible for): `0` for a top-level call, or the parked outer depth for a host re-entry.
    #[allow(clippy::too_many_lines)] // the resumable dispatch loop; arms are short
    fn run(&mut self, inner: &mut StoreInner, stop_depth: usize) -> Result<Outcome> {
        // A `return_call` to a host fn from the boundary frame pops it, then the host pushes its
        // results; re-entering here at `stop_depth` means the call finished (#39).
        if self.frames.len() == stop_depth {
            return Ok(Outcome::Finished);
        }
        let stack_limit = inner.engine().max_wasm_stack();
        let (mut code, mut ip, mut base, mut instance) = self.top();
        // Gate re-entry on the inherited budget: the per-`DoCall` check below only bounds wasm
        // recursion *within* this segment, so a re-entered call whose body does no wasm call (just
        // a host call) would never be checked. With the host-crossing reserve folded into
        // `stack_bytes`, this entry check makes even pure host↔wasm ping-pong trap here rather than
        // abort the native stack (#30).
        if self.stack_bytes() >= stack_limit {
            return Err(self.attach_trap_backtrace(inner, Trap::StackOverflow.into(), ip));
        }
        let fuel_enabled = inner.engine().consume_fuel();
        let epoch_enabled = inner.engine().epoch_interruption();
        // The engine-wide GC-pressure axis is only live under a tracing collector (else the
        // mailbox is never posted to — no hot-path cost by default).
        let gc_pressure_watch = inner.gc.is_collecting();
        loop {
            if ip as usize >= code.ops.len() {
                if self.do_return(code.n_results, stop_depth) {
                    return Ok(Outcome::Finished);
                }
                (code, ip, base, instance) = self.top();
                continue;
            }
            if fuel_enabled {
                match inner.consume_fuel_step() {
                    FuelStep::Ran => {}
                    FuelStep::Exhausted => {
                        return Err(self.attach_trap_backtrace(inner, Trap::OutOfFuel.into(), ip))
                    }
                    FuelStep::NeedYield => {
                        #[cfg(feature = "async")]
                        {
                            self.frames.last_mut().expect("current frame").ip = ip;
                            return Ok(Outcome::FuelYield);
                        }
                        // Unreachable without async (an interval needs an async store); stays total.
                        #[cfg(not(feature = "async"))]
                        return Err(self.attach_trap_backtrace(inner, Trap::OutOfFuel.into(), ip));
                    }
                }
            }
            if epoch_enabled && inner.epoch_deadline_reached() {
                self.frames.last_mut().expect("current frame").ip = ip;
                return Ok(Outcome::EpochDeadline);
            }
            // Honor an engine-wide GC-pressure request posted to this store's mailbox at this safe
            // point (operands are roots via the shadow). Read-and-clear, so we collect once per
            // posted request (not over and over); only large-footprint stores bother (no thundering
            // herd); request, not force — a finishing store simply never reaches here.
            if gc_pressure_watch && inner.gc.footprint_over_floor() && inner.take_gc_request() {
                self.gc_collect_now(inner);
            }
            match self.step(inner, &code, ip, base, instance) {
                Err(e) => match self.unwind(inner, e, ip, stop_depth) {
                    Ok(()) => (code, ip, base, instance) = self.top(),
                    Err(e) => return Err(self.attach_trap_backtrace(inner, e, ip)),
                },
                Ok(StepOutcome::Advance(next)) => ip = next,
                Ok(StepOutcome::DoCall(req)) => {
                    if self.stack_bytes() >= stack_limit {
                        return Err(self.attach_trap_backtrace(
                            inner,
                            Trap::StackOverflow.into(),
                            ip,
                        ));
                    }
                    self.frames.last_mut().expect("caller frame").ip = req.return_ip;
                    self.push_call(req.instance, req.func_index, req.code.clone());
                    code = req.code;
                    ip = 0;
                    base = self.frames.last().expect("callee frame").locals_base;
                    instance = req.instance;
                }
                // Tail call (#39): replace the current frame — `do_return(n_params)` repositions the
                // args to the frame's base and pops it, then `push_call` lays the callee there.
                Ok(StepOutcome::DoTailCall(req)) => {
                    self.do_return(req.code.n_params, stop_depth);
                    self.push_call(req.instance, req.func_index, req.code.clone());
                    code = req.code;
                    ip = 0;
                    base = self.frames.last().expect("callee frame").locals_base;
                    instance = req.instance;
                }
                // Tail call to a host fn: pop the current frame; the host's results return to the
                // caller (or, if the outermost frame is gone, to the embedder via the guard above).
                Ok(StepOutcome::DoTailHostCall {
                    func,
                    instance,
                    n_params,
                }) => {
                    self.do_return(n_params, stop_depth);
                    return Ok(Outcome::HostCall { func, instance });
                }
                #[cfg(feature = "async")]
                Ok(StepOutcome::DoTailHostAsyncCall {
                    func,
                    instance,
                    n_params,
                }) => {
                    self.do_return(n_params, stop_depth);
                    return Ok(Outcome::HostAsync { func, instance });
                }
                Ok(StepOutcome::DoHostCall {
                    func,
                    instance,
                    return_ip,
                }) => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::HostCall { func, instance });
                }
                #[cfg(feature = "async")]
                Ok(StepOutcome::DoHostAsyncCall {
                    func,
                    instance,
                    return_ip,
                }) => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::HostAsync { func, instance });
                }
                Ok(StepOutcome::DoGrow {
                    memory,
                    delta,
                    return_ip,
                }) => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::Grow { memory, delta });
                }
                Ok(StepOutcome::DoTableGrow {
                    table,
                    delta,
                    init,
                    return_ip,
                }) => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::TableGrow { table, delta, init });
                }
                Ok(StepOutcome::DoGcGrow {
                    reserved_target,
                    bytes_needed,
                    return_ip,
                }) => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::GcGrow {
                        reserved_target,
                        bytes_needed,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
