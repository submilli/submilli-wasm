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
mod memory;
mod numeric;
mod table;

use std::sync::Arc;

use crate::func::Func;
use crate::instance::Instance;
use crate::module::op::{BranchTarget, CompiledFunc, Op};
use crate::store::StoreInner;
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

enum StepOutcome {
    Advance(u32),
    DoCall(CallReq),
}

struct CallReq {
    return_ip: u32,
    instance: Instance,
    code: Arc<CompiledFunc>,
}

/// Runs `code` (a function of `instance`) with `args`, returning its results.
pub(crate) fn execute(
    inner: &mut StoreInner,
    instance: Instance,
    code: Arc<CompiledFunc>,
    args: Vec<Val>,
) -> Result<Vec<Val>> {
    let mut exec = Execution {
        values: args,
        frames: Vec::new(),
    };
    exec.push_call(instance, code);
    exec.run(inner)?;
    Ok(exec.values)
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

    fn run(&mut self, inner: &mut StoreInner) -> Result<()> {
        let stack_limit = inner.engine().max_wasm_stack();
        let frame = self.frames.last().expect("no initial frame");
        let mut code = frame.code.clone();
        let mut ip = frame.ip;
        let mut base = frame.locals_base;
        let mut instance = frame.instance;
        loop {
            if ip as usize >= code.ops.len() {
                if self.do_return(code.n_results) {
                    return Ok(());
                }
                let f = self.frames.last().expect("caller frame");
                code = f.code.clone();
                ip = f.ip;
                base = f.locals_base;
                instance = f.instance;
                continue;
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
                let (callee_instance, callee_code) = self.resolve_call(inner, instance, *f);
                return Ok(StepOutcome::DoCall(CallReq {
                    return_ip: next,
                    instance: callee_instance,
                    code: callee_code,
                }));
            }
            Op::CallIndirect { type_idx, table } => {
                return self.do_call_indirect(inner, instance, *type_idx, *table, next)
            }
            other => self.exec_numeric(inner, other, instance)?,
        }
        Ok(StepOutcome::Advance(next))
    }

    fn resolve_call(
        &self,
        inner: &StoreInner,
        instance: Instance,
        func_idx: u32,
    ) -> (Instance, Arc<CompiledFunc>) {
        let f = inner.instance(instance).funcs[func_idx as usize];
        resolve_func(inner, f)
    }
}

/// Follows a function handle to its defining instance and compiled body. Imported
/// functions resolve transparently: the handle already points at the defining
/// instance's `FuncEntity`.
pub(crate) fn resolve_func(inner: &StoreInner, f: Func) -> (Instance, Arc<CompiledFunc>) {
    let fe = inner.func(f);
    let def_inst = fe.instance;
    let func_index = fe.func_index;
    let module = inner.instance(def_inst).module.clone();
    (def_inst, module.inner().compiled(func_index))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
