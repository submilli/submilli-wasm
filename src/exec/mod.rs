//! The interpreter run loop and transient execution state.
//!
//! Each frame holds its code as `Arc<CompiledFunc>` so the loop can read ops
//! from the (immutable) `Arc` while freely mutating the value/frame stacks and
//! the store. `step` returns an owned outcome so we never reassign the active
//! `code` while a borrow of it is live. See ARCHITECTURE §7.

mod arith;
mod call;
mod convert;
mod frame;
pub(crate) mod host;
mod memory;
mod numeric;
mod table;

use std::sync::Arc;

use crate::func::Func;
use crate::instance::Instance;
use crate::module::op::{BranchTarget, CompiledFunc, Op};
use crate::store::{FuncEntity, StoreInner};
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

use self::frame::Frame;

/// Transient interpreter state for one top-level call. (Resumable state held in
/// the `Store` arrives in Phase 3.)
struct Execution {
    values: Vec<Val>,
    frames: Vec<Frame>,
}

/// Why [`Execution::run`] returned: either the call finished or it suspended on a
/// host function the (generic) driver in [`host`] must invoke.
pub(crate) enum Outcome {
    Finished,
    HostCall {
        func: Func,
        instance: Instance,
    },
    EpochDeadline,
    Grow {
        memory: crate::extern_::Memory,
        delta: u64,
    },
}

enum StepOutcome {
    Advance(u32),
    DoCall(CallReq),
    DoHostCall {
        func: Func,
        instance: Instance,
        return_ip: u32,
    },
    DoGrow {
        memory: crate::extern_::Memory,
        delta: u64,
        return_ip: u32,
    },
}

/// A resolved callee: a wasm body to push a frame for, or a host func to suspend on.
enum ResolvedCall {
    Wasm(Instance, Arc<CompiledFunc>),
    Host(Func),
}

struct CallReq {
    return_ip: u32,
    instance: Instance,
    code: Arc<CompiledFunc>,
}

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
        let frame = self.frames.last().expect("no initial frame");
        let mut code = frame.code.clone();
        let mut ip = frame.ip;
        let mut base = frame.locals_base;
        let mut instance = frame.instance;
        loop {
            if ip as usize >= code.ops.len() {
                if self.do_return(code.n_results) {
                    return Ok(Outcome::Finished);
                }
                let f = self.frames.last().expect("caller frame");
                code = f.code.clone();
                ip = f.ip;
                base = f.locals_base;
                instance = f.instance;
                continue;
            }
            if fuel_enabled && !inner.try_consume_fuel() {
                return Err(Trap::OutOfFuel.into());
            }
            if epoch_enabled && inner.epoch_deadline_reached() {
                self.frames.last_mut().expect("current frame").ip = ip;
                return Ok(Outcome::EpochDeadline);
            }
            match self.step(inner, &code, ip, base, instance)? {
                StepOutcome::Advance(next) => ip = next,
                StepOutcome::DoCall(req) => {
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
                StepOutcome::DoHostCall {
                    func,
                    instance,
                    return_ip,
                } => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::HostCall { func, instance });
                }
                StepOutcome::DoGrow {
                    memory,
                    delta,
                    return_ip,
                } => {
                    self.frames.last_mut().expect("caller frame").ip = return_ip;
                    return Ok(Outcome::Grow { memory, delta });
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
                return Ok(match resolve(inner, callee) {
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
                });
            }
            Op::CallIndirect { type_idx, table } => {
                return self.do_call_indirect(inner, instance, *type_idx, *table, next)
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
            other => self.exec_numeric(inner, other, instance)?,
        }
        Ok(StepOutcome::Advance(next))
    }
}

/// Resolves a function handle to a wasm body (defining instance + compiled code)
/// or a host func. Imported functions resolve transparently — the handle already
/// points at the defining instance's `FuncEntity`.
fn resolve(inner: &StoreInner, f: Func) -> ResolvedCall {
    match inner.func(f) {
        FuncEntity::Wasm {
            instance,
            func_index,
        } => {
            let def_inst = *instance;
            let module = inner.instance(def_inst).module.clone();
            ResolvedCall::Wasm(def_inst, module.inner().compiled(*func_index))
        }
        FuncEntity::Host { .. } => ResolvedCall::Host(f),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
