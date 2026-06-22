//! The interpreter run loop and transient execution state.
//!
//! Each frame holds its code as `Arc<CompiledFunc>` so the loop reads ops while mutating the
//! value/frame stacks and the store. `step` returns an owned outcome. See ARCHITECTURE §7.

mod arith;
mod call;
mod cast;
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
    values: Vec<Val>,
    frames: Vec<Frame>,
}

// Parkability: keep `Execution` `Send` so async can await with it at rest. Compile-time check —
// a future non-`Send` field (e.g. an `Rc`) fails here rather than at an `.await`.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Execution>();
};

impl Execution {
    fn push(&mut self, v: Val) {
        self.values.push(v);
    }

    fn pop(&mut self) -> Val {
        self.values.pop().expect("operand stack underflow")
    }

    fn pop_i32(&mut self) -> i32 {
        self.pop().unwrap_i32()
    }

    /// Pops an index/length/address operand that is i32 (32-bit memory/table) or i64 (memory64/
    /// table64, #42), widening to u64. Validation fixes the variant, so reading whichever int is
    /// present is correct — the executor needn't know the entity's index width here.
    fn pop_index(&mut self) -> u64 {
        match self.pop() {
            Val::I32(v) => u64::from(v as u32),
            Val::I64(v) => v as u64,
            _ => unreachable!("validated index operand is i32/i64"),
        }
    }

    /// Pushes a size/grow result as i64 for a 64-bit memory/table, else i32 (#42).
    fn push_index(&mut self, is_64: bool, v: u64) {
        self.push(if is_64 {
            Val::I64(v as i64)
        } else {
            Val::I32(v as u32 as i32)
        });
    }

    /// Estimated byte footprint of the wasm execution stacks, checked against
    /// `Config::max_wasm_stack` at each call to bound runaway recursion.
    fn stack_bytes(&self) -> usize {
        self.values.len() * std::mem::size_of::<Val>()
            + self.frames.len() * std::mem::size_of::<Frame>()
    }

    fn push_call(&mut self, instance: Instance, func_index: u32, code: Arc<CompiledFunc>) {
        let locals_base = self.values.len() as u32 - code.n_params;
        for ty in &code.local_types {
            self.values.push(Val::default_for(ty));
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
                    FuelStep::Exhausted => return Err(Trap::OutOfFuel.into()),
                    FuelStep::NeedYield => {
                        #[cfg(feature = "async")]
                        {
                            self.frames.last_mut().expect("current frame").ip = ip;
                            return Ok(Outcome::FuelYield);
                        }
                        // Unreachable without async (an interval needs an async store); stays total.
                        #[cfg(not(feature = "async"))]
                        return Err(Trap::OutOfFuel.into());
                    }
                }
            }
            if epoch_enabled && inner.epoch_deadline_reached() {
                self.frames.last_mut().expect("current frame").ip = ip;
                return Ok(Outcome::EpochDeadline);
            }
            match self.step(inner, &code, ip, base, instance) {
                Err(e) => match self.unwind(inner, e, ip) {
                    Ok(()) => (code, ip, base, instance) = self.top(),
                    Err(e) => return Err(self.attach_trap_backtrace(inner, e, ip)),
                },
                Ok(StepOutcome::Advance(next)) => ip = next,
                Ok(StepOutcome::DoCall(req)) => {
                    if self.stack_bytes() >= stack_limit {
                        return Err(Trap::StackOverflow.into());
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
            }
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
