//! The flat opcode dispatch: one `step` per instruction, delegating to the per-category handlers.
//! See ARCHITECTURE §7. The big `match` is the interpreter's single sanctioned long function.

// Local/global/function indexing is into wasmparser-validated index spaces (#33 carve-out).
#![allow(clippy::indexing_slicing)]

use super::call::{self, CallKind};
use super::{cell, Execution, StepOutcome};
use crate::instance::Instance;
use crate::module::op::{CmpKind, CompiledFunc, Op};
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::Result;

impl Execution {
    // Flat opcode dispatch; arms are short. `inline(always)` is deliberate: `step` has a single
    // call site (the `run` loop), and inlining it there removes the per-op call/return and the
    // `Result<StepOutcome>` stack round-trip, keeping `ip`/`base` in registers across ops —
    // measured ~1.8× on CoreMark. The op arrives already fetched (`run` gets it via `ops.get`, so
    // there is exactly one bounds check per op); `next` is the fall-through ip.
    #[allow(
        clippy::too_many_lines,
        clippy::inline_always,
        clippy::too_many_arguments
    )]
    #[inline(always)]
    pub(super) fn step(
        &mut self,
        inner: &mut StoreInner,
        code: &CompiledFunc,
        op: &Op,
        next: u32,
        base: u32,
        instance: Instance,
    ) -> Result<StepOutcome> {
        match op {
            Op::Nop => {}
            Op::Unreachable => return Err(Trap::UnreachableCodeReached.into()),
            Op::I32Const(v) => self.push_i32(*v),
            Op::I64Const(v) => self.push_i64(*v),
            Op::F32Const(v) => self.push_f32_bits(*v),
            Op::F64Const(v) => self.push_f64_bits(*v),
            Op::Drop => {
                self.pop();
            }
            Op::Select => {
                let cond = self.pop_i32();
                let b = self.pop_tagged();
                let a = self.pop_tagged();
                let (cell, tag) = if cond != 0 { a } else { b };
                self.push_cell(cell, tag);
            }
            Op::LocalGet(i) => {
                let (cell, tag) = self.cell_at((base + i) as usize);
                self.push_cell(cell, tag);
            }
            Op::LocalSet(i) => {
                let (cell, tag) = self.pop_tagged();
                self.set_cell((base + i) as usize, cell, tag);
            }
            Op::LocalTee(i) => {
                let (cell, tag) = self.top_cell();
                self.set_cell((base + i) as usize, cell, tag);
            }
            Op::GlobalGet(g) => {
                let handle = inner.instance(instance).globals[*g as usize];
                let v = inner.global(handle).value;
                self.push(v);
            }
            Op::GlobalSet(g) => {
                let handle = inner.instance(instance).globals[*g as usize];
                let v = cell::decode(self.pop(), inner.global(handle).ty.content());
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
            Op::BrIfCmp {
                kind,
                negate,
                target,
            } => {
                let n = self.values.len();
                let (a, b) = (
                    self.values[n - 2].unwrap_i32(),
                    self.values[n - 1].unwrap_i32(),
                );
                self.values.truncate(n - 2);
                self.shadow.truncate(n - 2);
                if cmp_i32(*kind, a, b) != *negate {
                    self.take_branch(target);
                    return Ok(StepOutcome::Advance(target.ip));
                }
            }
            Op::BrTable(range) => {
                // Targets live out-of-line in `code.br_tables`: `len` cases then the default.
                let cases = &code.br_tables[range.base as usize..(range.base + range.len) as usize];
                let default = &code.br_tables[(range.base + range.len) as usize];
                let i = self.pop_i32() as u32 as usize;
                let t = cases.get(i).unwrap_or(default);
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
                let (r, tag) = self.pop_tagged();
                if r.is_null() {
                    self.take_branch(t);
                    return Ok(StepOutcome::Advance(t.ip));
                }
                self.push_cell(r, tag); // non-null: keep it, fall through
            }
            Op::BrOnNonNull(t) => {
                let (r, tag) = self.pop_tagged();
                if !r.is_null() {
                    self.push_cell(r, tag); // non-null: keep it on the branch target
                    self.take_branch(t);
                    return Ok(StepOutcome::Advance(t.ip));
                }
                // null: reference dropped, fall through
            }
            Op::MemoryGrow(i) => {
                // Routed through the driver so the (T-generic) limiter is consulted.
                let memory = inner.instance(instance).memories[*i as usize];
                let delta = self.pop_index(inner.memory(memory).ty.is_64());
                return Ok(StepOutcome::DoGrow {
                    memory,
                    delta,
                    return_ip: next,
                });
            }
            Op::TableGrow(t) => {
                // Routed through the driver (limiter-consulted), like `memory.grow`.
                let table = inner.instance(instance).tables[*t as usize];
                let tt = &inner.table(table).ty;
                let (is_64, kind) = (tt.is_64(), cell::refkind_of_heap(tt.element().heap_type()));
                let delta = self.pop_index(is_64);
                let init = self.pop_ref(kind).to_ref();
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
            Op::Throw(tag) => return self.throw(inner, instance, *tag, next - 1),
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
            #[cfg(feature = "simd")]
            Op::Simd(s) => self.exec_simd(inner, s, instance)?,
            // GC aggregate allocation: ensure the limiter-granted reservation covers it first (with
            // operands still on the stack as roots); a reservation grow suspends and re-executes.
            // `array.new_data`/`array.new_elem` route through here too — their charge is clamped to
            // the source segment size (`seg_clamped_charge`), so a would-be-out-of-bounds count
            // reserves only a bounded amount and still traps with the correct error in its handler.
            alloc @ (Op::StructNew(_)
            | Op::StructNewDefault(_)
            | Op::ArrayNew(_)
            | Op::ArrayNewDefault(_)
            | Op::ArrayNewFixed { .. }
            | Op::ArrayNewData { .. }
            | Op::ArrayNewElem { .. }) => {
                if let Some(charge) = self.gc_alloc_charge(inner, instance, alloc)? {
                    if let Some(out) = self.gc_reserve(inner, charge, next - 1) {
                        return Ok(out);
                    }
                }
                self.exec_gc(inner, alloc, instance)?;
            }
            // Straight-line numerics + GC, hottest category first:
            // exec_numeric → exec_gc → exec_gc_array → exec_cast.
            other => self.exec_numeric(inner, other, instance)?,
        }
        Ok(StepOutcome::Advance(next))
    }
}

/// Evaluates a fused compare-and-branch comparison ([`Op::BrIfCmp`]).
#[allow(clippy::inline_always)] // one hot call site, inside the dispatch arm
#[inline(always)]
#[allow(clippy::cast_sign_loss)] // `_U` kinds reinterpret the operand bits as unsigned, per spec
fn cmp_i32(kind: CmpKind, a: i32, b: i32) -> bool {
    match kind {
        CmpKind::Eq => a == b,
        CmpKind::Ne => a != b,
        CmpKind::LtS => a < b,
        CmpKind::LtU => (a as u32) < (b as u32),
        CmpKind::GtS => a > b,
        CmpKind::GtU => (a as u32) > (b as u32),
        CmpKind::LeS => a <= b,
        CmpKind::LeU => (a as u32) <= (b as u32),
        CmpKind::GeS => a >= b,
        CmpKind::GeU => (a as u32) >= (b as u32),
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
