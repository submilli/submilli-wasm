//! The flat opcode dispatch: one `step` per instruction, delegating to the per-category handlers.
//! See ARCHITECTURE §7. The big `match` is the interpreter's single sanctioned long function.

use super::call::{self, CallKind};
use super::{Execution, StepOutcome};
use crate::instance::Instance;
use crate::module::op::{CompiledFunc, Op};
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

impl Execution {
    #[allow(clippy::too_many_lines)] // flat opcode dispatch; arms are short
    pub(super) fn step(
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
            call_op @ (Op::Call(f) | Op::ReturnCall(f)) => {
                let callee = inner.instance(instance).funcs[*f as usize];
                let kind = call_kind(matches!(call_op, Op::ReturnCall(_)), next);
                return Ok(call::call_outcome(
                    inner,
                    call::resolve(inner, callee),
                    instance,
                    kind,
                ));
            }
            Op::CallIndirect { type_idx, table } => {
                return self.do_call_indirect(
                    inner,
                    instance,
                    *type_idx,
                    *table,
                    CallKind::Nested(next),
                )
            }
            Op::ReturnCallIndirect { type_idx, table } => {
                return self.do_call_indirect(inner, instance, *type_idx, *table, CallKind::Tail)
            }
            Op::CallRef(_) => return self.do_call_ref(inner, instance, CallKind::Nested(next)),
            Op::ReturnCallRef(_) => return self.do_call_ref(inner, instance, CallKind::Tail),
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
            Op::MemoryGrow(i) => {
                // Routed through the driver so the (T-generic) limiter is consulted.
                let memory = inner.instance(instance).memories[*i as usize];
                let delta = self.pop_index();
                return Ok(StepOutcome::DoGrow {
                    memory,
                    delta,
                    return_ip: next,
                });
            }
            Op::TableGrow(t) => {
                // Routed through the driver (limiter-consulted), like `memory.grow`.
                let table = inner.instance(instance).tables[*t as usize];
                let delta = self.pop_index();
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
            | Op::MemorySize(_)
            | Op::MemoryCopy(..)
            | Op::MemoryFill(_)
            | Op::MemoryInit(..)
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
            // Straight-line GC + numerics: exec_gc → exec_gc_array → exec_cast → exec_numeric.
            other => self.exec_gc(inner, other, instance)?,
        }
        Ok(StepOutcome::Advance(next))
    }
}

/// A tail (`return_call*`) site replaces the current frame; a normal call keeps `next` as the
/// caller's resume ip (#39).
fn call_kind(tail: bool, next: u32) -> CallKind {
    if tail {
        CallKind::Tail
    } else {
        CallKind::Nested(next)
    }
}
