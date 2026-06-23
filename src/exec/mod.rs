//! The interpreter run loop and transient execution state.
//!
//! Each frame holds its code as `Arc<CompiledFunc>` so the loop reads ops while mutating the
//! value/frame stacks and the store. `step` returns an owned outcome. See ARCHITECTURE §7.

mod arith;
mod call;
mod cast;
mod cell;
mod collect;
mod convert;
mod exn;
mod frame;
mod gc;
mod gc_array;
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

use self::frame::Frame;
pub(crate) use outcome::Outcome;
use outcome::{CallReq, ResolvedCall, StepOutcome};

/// Transient interpreter state for one top-level call. Self-contained: it owns its
/// operand/frame stacks and holds no borrow into the `Store` across an [`Outcome`]
/// suspend, so it can be *parked* between resumptions — the basis for async,
/// where the driver awaits with this state at rest. See ARCHITECTURE §2.4.
struct Execution {
    values: Vec<cell::Cell>,
    /// Per-operand-slot reference tag ([`cell::RefTag`]), parallel to `values` (same length, moved
    /// in lockstep). The cell stack is untyped, so this byte-shadow is how a tracing collector
    /// recovers operand/local roots (ARCHITECTURE §7/§14, #27g). Maintained unconditionally; cheap
    /// (1 B/slot) and far simpler to keep exhaustive than gating per-op.
    shadow: Vec<cell::RefTag>,
    frames: Vec<Frame>,
}

// Parkability: keep `Execution` `Send` so async can await with it at rest. Compile-time check —
// a future non-`Send` field (e.g. an `Rc`) fails here rather than at an `.await`.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Execution>();
};

impl Execution {
    /// Estimated byte footprint of the wasm execution stacks, checked against
    /// `Config::max_wasm_stack` at each call to bound runaway recursion. An operand slot is now a
    /// fixed-width untyped [`cell::Cell`] (8 or 16 bytes; see `cell`), not the ~32-byte `Val`.
    fn stack_bytes(&self) -> usize {
        self.values.len() * std::mem::size_of::<cell::Cell>()
            + self.shadow.len() // 1 byte per operand slot (the GC root shadow)
            + self.frames.len() * std::mem::size_of::<Frame>()
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
    /// frame base. Returns true if no frames remain (execution finished).
    fn do_return(&mut self, n_results: u32) -> bool {
        let frame = self.frames.pop().expect("frame stack underflow");
        let n = n_results as usize;
        let len = self.values.len();
        let dst = frame.locals_base as usize;
        self.values.copy_within(len - n..len, dst);
        self.values.truncate(dst + n);
        self.shadow.copy_within(len - n..len, dst);
        self.shadow.truncate(dst + n);
        self.frames.is_empty()
    }

    #[allow(clippy::too_many_lines)] // the resumable dispatch loop; arms are short
    fn run(&mut self, inner: &mut StoreInner) -> Result<Outcome> {
        // A `return_call` to a host fn from the outermost frame pops the only frame, then the host
        // pushes its results; re-entering here with no frames means the call finished (#39).
        if self.frames.is_empty() {
            return Ok(Outcome::Finished);
        }
        let stack_limit = inner.engine().max_wasm_stack();
        let fuel_enabled = inner.engine().consume_fuel();
        let epoch_enabled = inner.engine().epoch_interruption();
        // The engine-wide GC-pressure axis is only live under a tracing collector (else the
        // mailbox is never posted to — no hot-path cost by default).
        let gc_pressure_watch = inner.gc.is_collecting();
        let (mut code, mut ip, mut base, mut instance) = self.top();
        loop {
            if ip as usize >= code.ops.len() {
                if self.do_return(code.n_results) {
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
                Err(e) => match self.unwind(inner, e, ip) {
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
                    self.do_return(req.code.n_params);
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
                    self.do_return(n_params);
                    return Ok(Outcome::HostCall { func, instance });
                }
                #[cfg(feature = "async")]
                Ok(StepOutcome::DoTailHostAsyncCall {
                    func,
                    instance,
                    n_params,
                }) => {
                    self.do_return(n_params);
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
                    return_ip,
                }) => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::GcGrow { reserved_target });
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
