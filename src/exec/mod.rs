//! The interpreter run loop and transient execution state.
//!
//! Each frame holds its code as `Arc<CompiledFunc>` so the loop can read ops
//! from the (immutable) `Arc` while freely mutating the value/frame stacks and
//! the store. `step` returns an owned outcome so we never reassign the active
//! `code` while a borrow of it is live. See ARCHITECTURE §7.

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
mod table;
pub(crate) mod trace;

use std::sync::Arc;

use crate::instance::Instance;
use crate::module::op::{BranchTarget, CompiledFunc, Op};
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

// Parkability: keep `Execution` `Send` so async can await with it at rest and the
// executor can move it between tasks. Compile-time check — if a future field adds a
// non-`Send` member (e.g. an `Rc`), this fails here rather than at an `.await`.
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

    /// Estimated byte footprint of the wasm execution stacks, checked against
    /// `Config::max_wasm_stack` at each call to bound runaway recursion.
    fn stack_bytes(&self) -> usize {
        self.values.len() * std::mem::size_of::<Val>()
            + self.frames.len() * std::mem::size_of::<Frame>()
    }

    fn push_call(&mut self, instance: Instance, code: Arc<CompiledFunc>) {
        let locals_base = self.values.len() as u32 - code.n_params;
        for ty in &code.local_types {
            self.values.push(Val::default_for(ty));
        }
        self.frames.push(Frame {
            code,
            ip: 0,
            locals_base,
            instance,
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
                    self.push_call(req.instance, req.code.clone());
                    code = req.code;
                    ip = 0;
                    base = self.frames.last().expect("callee frame").locals_base;
                    instance = req.instance;
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

    #[allow(clippy::too_many_lines)] // flat opcode dispatch; arms are short
    fn step(
        &mut self,
        inner: &mut StoreInner,
        code: &CompiledFunc,
        ip: u32,
        base: u32,
        instance: Instance,
    ) -> Result<StepOutcome> {
        let next = ip + 1;
        match &code.ops[ip as usize] {
            Op::Nop => {}
            Op::Unreachable => return Err(Trap::UnreachableCodeReached.into()),
            Op::I32Const(v) => self.push(Val::I32(*v)),
            Op::I64Const(v) => self.push(Val::I64(*v)),
            Op::F32Const(v) => self.push(Val::F32(*v)),
            Op::F64Const(v) => self.push(Val::F64(*v)),
            Op::Drop => {
                self.pop();
            }
            Op::Select => {
                let cond = self.pop_i32();
                let b = self.pop();
                let a = self.pop();
                self.push(if cond != 0 { a } else { b });
            }
            Op::LocalGet(i) => {
                let v = self.values[(base + i) as usize];
                self.push(v);
            }
            Op::LocalSet(i) => {
                let v = self.pop();
                self.values[(base + i) as usize] = v;
            }
            Op::LocalTee(i) => {
                let v = *self.values.last().expect("operand stack underflow");
                self.values[(base + i) as usize] = v;
            }
            Op::GlobalGet(g) => {
                let handle = inner.instance(instance).globals[*g as usize];
                let v = inner.global(handle).value;
                self.push(v);
            }
            Op::GlobalSet(g) => {
                let v = self.pop();
                let handle = inner.instance(instance).globals[*g as usize];
                inner.global_mut(handle).value = v;
            }
            Op::Br(t) => {
                self.take_branch(t);
                return Ok(StepOutcome::Advance(t.ip));
            }
            Op::BrIf(t) => {
                if self.pop_i32() != 0 {
                    self.take_branch(t);
                    return Ok(StepOutcome::Advance(t.ip));
                }
            }
            Op::BrIfNot(t) => {
                if self.pop_i32() == 0 {
                    self.take_branch(t);
                    return Ok(StepOutcome::Advance(t.ip));
                }
            }
            Op::BrTable { targets, default } => {
                let i = self.pop_i32() as u32 as usize;
                let t = targets.get(i).unwrap_or(default);
                self.take_branch(t);
                return Ok(StepOutcome::Advance(t.ip));
            }
            Op::Call(f) => {
                let callee = inner.instance(instance).funcs[*f as usize];
                return Ok(match call::resolve(inner, callee) {
                    ResolvedCall::Wasm(callee_instance, code) => StepOutcome::DoCall(CallReq {
                        return_ip: next,
                        instance: callee_instance,
                        code,
                    }),
                    ResolvedCall::Host(func) => StepOutcome::DoHostCall {
                        func,
                        instance,
                        return_ip: next,
                    },
                    #[cfg(feature = "async")]
                    ResolvedCall::HostAsync(func) => StepOutcome::DoHostAsyncCall {
                        func,
                        instance,
                        return_ip: next,
                    },
                });
            }
            Op::CallIndirect { type_idx, table } => {
                return self.do_call_indirect(inner, instance, *type_idx, *table, next)
            }
            Op::CallRef(_) => return self.do_call_ref(inner, instance, next),
            Op::BrOnNull(t) => {
                let r = self.pop();
                if r.is_null_ref() {
                    self.take_branch(t);
                    return Ok(StepOutcome::Advance(t.ip));
                }
                self.push(r); // non-null: keep it, fall through
            }
            Op::BrOnNonNull(t) => {
                let r = self.pop();
                if !r.is_null_ref() {
                    self.push(r); // non-null: keep it on the branch target
                    self.take_branch(t);
                    return Ok(StepOutcome::Advance(t.ip));
                }
                // null: reference dropped, fall through
            }
            Op::MemoryGrow => {
                // Routed through the driver so the (T-generic) limiter is consulted.
                let memory = inner.instance(instance).memories[0];
                let delta = u64::from(self.pop_i32() as u32);
                return Ok(StepOutcome::DoGrow {
                    memory,
                    delta,
                    return_ip: next,
                });
            }
            Op::TableGrow(t) => {
                // Routed through the driver (limiter-consulted), like `memory.grow`.
                let table = inner.instance(instance).tables[*t as usize];
                let delta = u64::from(self.pop_i32() as u32);
                let init = self.pop().to_ref();
                return Ok(StepOutcome::DoTableGrow {
                    table,
                    delta,
                    init,
                    return_ip: next,
                });
            }
            // Straight-line ops route by category to their dedicated handler.
            op @ (Op::I32Load(_)
            | Op::I64Load(_)
            | Op::F32Load(_)
            | Op::F64Load(_)
            | Op::I32Load8S(_)
            | Op::I32Load8U(_)
            | Op::I32Load16S(_)
            | Op::I32Load16U(_)
            | Op::I64Load8S(_)
            | Op::I64Load8U(_)
            | Op::I64Load16S(_)
            | Op::I64Load16U(_)
            | Op::I64Load32S(_)
            | Op::I64Load32U(_)
            | Op::I32Store(_)
            | Op::I64Store(_)
            | Op::F32Store(_)
            | Op::F64Store(_)
            | Op::I32Store8(_)
            | Op::I32Store16(_)
            | Op::I64Store8(_)
            | Op::I64Store16(_)
            | Op::I64Store32(_)
            | Op::MemorySize
            | Op::MemoryCopy
            | Op::MemoryFill
            | Op::MemoryInit(_)
            | Op::DataDrop(_)) => {
                self.exec_memory(inner, op, instance)?;
            }
            op @ (Op::RefNull(_) | Op::RefFunc(_) | Op::RefIsNull | Op::RefAsNonNull) => {
                self.exec_ref(inner, op, instance)?;
            }
            op @ (Op::TableInit { .. }
            | Op::TableCopy { .. }
            | Op::ElemDrop(_)
            | Op::TableGet(_)
            | Op::TableSet(_)
            | Op::TableSize(_)
            | Op::TableFill(_)) => self.exec_table(inner, op, instance)?,
            Op::Throw(tag) => return self.throw(inner, instance, *tag, ip),
            Op::ThrowRef => return self.throw_ref(),
            op @ Op::BrOnCast { .. } => {
                if let Some(ip) = self.br_on_cast(inner, instance, op, false) {
                    return Ok(StepOutcome::Advance(ip));
                }
            }
            op @ Op::BrOnCastFail { .. } => {
                if let Some(ip) = self.br_on_cast(inner, instance, op, true) {
                    return Ok(StepOutcome::Advance(ip));
                }
            }
            // Straight-line GC + numerics fall through a chain: `exec_gc` (struct/i31) →
            // `exec_gc_array` (arrays) → `exec_cast` (test/cast/eq/convert) → `exec_numeric`.
            other => self.exec_gc(inner, other, instance)?,
        }
        Ok(StepOutcome::Advance(next))
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
