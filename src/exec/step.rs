//! The flat opcode dispatch: one `step` per instruction, delegating to the per-category handlers.
//! See ARCHITECTURE §7. The big `match` is the interpreter's single sanctioned long function.

// `f*.eq`/`f*.ne` are exact IEEE comparisons by definition.
#![allow(clippy::float_cmp)]

use super::call::{self, CallKind};
use super::cell::Cell;
use super::{arith, cell, convert, memory, Execution, StepOutcome, CONTROL};
use crate::instance::Instance;
use crate::module::code::Code;
use crate::module::op::{CmpKind, Op};
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
    #[allow(clippy::cognitive_complexity)] // flat bytecode dispatch; arms delegate by category
    #[inline(always)]
    pub(super) fn step(
        &mut self,
        inner: &mut StoreInner,
        code: &Code,
        op: &Op,
        next: u32,
        base: u32,
        instance: Instance,
    ) -> Result<u32> {
        match op {
            Op::Nop => {}
            Op::Unreachable => return Err(Trap::UnreachableCodeReached.into()),
            Op::I32Const(s, v) => self.set_scalar(slot(base, *s), Cell::from_i32(*v))?,
            Op::I64Const(s, v) => self.set_scalar(slot(base, *s), Cell::from_i64(*v))?,
            Op::F32Const(s, v) => {
                self.set_scalar(slot(base, *s), Cell::of_bytes(v.to_le_bytes()))?;
            }
            Op::F64Const(s, v) => {
                self.set_scalar(slot(base, *s), Cell::of_bytes(v.to_le_bytes()))?;
            }
            Op::Select(s) => self.select_at(slot(base, *s))?,
            Op::LocalGet(s, i) => {
                let (cell, tag) = self.cell_at(slot(base, *i))?;
                self.set_cell(slot(base, *s), cell, tag)?;
            }
            Op::LocalSet(s, i) => {
                let (cell, tag) = self.cell_at(slot(base, *s))?;
                self.set_cell(slot(base, *i), cell, tag)?;
            }
            Op::GlobalGet(s, g) => {
                let v = inner.global(index_space::global(inner, instance, *g)).value;
                self.set_val(slot(base, *s), v)?;
            }
            Op::GlobalSet(s, g) => {
                let handle = index_space::global(inner, instance, *g);
                let v = self.take_typed(slot(base, *s), inner.global(handle).ty.content())?;
                inner.global_mut(handle).value = v;
            }
            Op::Br(t, s) => {
                self.take_branch(*t, slot(base, *s))?;
                return Ok(t.ip);
            }
            Op::BrIf(t, s) => {
                let at = slot(base, *s);
                if self.cond_at(at)? != 0 {
                    self.take_branch(*t, at)?;
                    return Ok(t.ip);
                }
            }
            Op::BrIfNot(t, s) => {
                let at = slot(base, *s);
                if self.cond_at(at)? == 0 {
                    self.take_branch(*t, at)?;
                    return Ok(t.ip);
                }
            }
            Op::BrIfCmp {
                kind,
                negate,
                target,
                s,
            } => {
                let at = slot(base, *s);
                let (a, b) = self.i32_pair_at(at)?;
                if cmp_i32(*kind, a, b) != *negate {
                    self.take_branch(*target, at)?;
                    return Ok(target.ip);
                }
            }
            Op::BrTable(range, s) => {
                // Targets live out-of-line in `code.br_tables`: `len` cases then the default.
                let pool = code.br_tables();
                let end = range.base.checked_add(range.len).ok_or(
                    crate::error::InternalError::Compiler("branch table range overflow"),
                )? as usize;
                let cases = pool.get(range.base as usize..end).ok_or(
                    crate::error::InternalError::Compiler("branch table range is invalid"),
                )?;
                let default = pool.get(end).ok_or(crate::error::InternalError::Compiler(
                    "branch table default is missing",
                ))?;
                let at = slot(base, *s);
                let i = self.cond_at(at)? as u32 as usize;
                let t = cases.get(i).unwrap_or(default);
                self.take_branch(*t, at)?;
                return Ok(t.ip);
            }
            call_op @ (Op::Call(f, s) | Op::ReturnCall(f, s)) => {
                let callee = index_space::func(inner, instance, *f);
                let kind = call_kind(matches!(call_op, Op::ReturnCall(..)), next);
                let resolved = call::resolve(inner, callee);
                let outcome = call::call_outcome(inner, resolved, instance, kind, slot(base, *s));
                return Ok(self.control(outcome));
            }
            Op::CallIndirect { type_idx, table, s } => {
                let (kind, at) = (CallKind::Nested(next), slot(base, *s));
                let outcome =
                    self.do_call_indirect(inner, instance, (*type_idx, *table), kind, at)?;
                return Ok(self.control(outcome));
            }
            Op::ReturnCallIndirect { type_idx, table, s } => {
                let (kind, at) = (CallKind::Tail, slot(base, *s));
                let outcome =
                    self.do_call_indirect(inner, instance, (*type_idx, *table), kind, at)?;
                return Ok(self.control(outcome));
            }
            Op::CallRef(_, s) => {
                let at = slot(base, *s);
                let outcome = self.do_call_ref(inner, instance, CallKind::Nested(next), at)?;
                return Ok(self.control(outcome));
            }
            Op::ReturnCallRef(_, s) => {
                let at = slot(base, *s);
                let outcome = self.do_call_ref(inner, instance, CallKind::Tail, at)?;
                return Ok(self.control(outcome));
            }
            Op::BrOnNull(t, s) => {
                let at = slot(base, *s);
                if self.ref_at(at)?.0.is_null() {
                    self.take_branch(*t, at)?; // the reference is consumed
                    return Ok(t.ip);
                }
                // non-null: the reference stays in its slot; fall through
            }
            Op::BrOnNonNull(t, s) => {
                let at = slot(base, *s);
                if !self.ref_at(at)?.0.is_null() {
                    self.take_branch(*t, at + 1)?; // the reference is kept for the target
                    return Ok(t.ip);
                } // null: dropped; fall through
            }
            Op::MemoryGrow(i, s) => {
                // Routed through the driver so the (T-generic) limiter is consulted; it writes the
                // old size (or -1) back over the delta's slot.
                let at = slot(base, *s);
                let memory = index_space::memory(inner, instance, *i);
                let delta = self.index_at(at, inner.memory(memory).ty.is_64())?;
                return Ok(self.control(StepOutcome::Grow {
                    memory,
                    delta,
                    at,
                    return_ip: next,
                }));
            }
            Op::TableGrow(t, s) => {
                // Routed through the driver (limiter-consulted), like `memory.grow`; operands
                // `init, delta`, and the old size (or -1) replaces them.
                let at = slot(base, *s);
                let table = index_space::table(inner, instance, *t);
                let tt = &inner.table(table).ty;
                let (is_64, kind) = (tt.is_64(), cell::refkind_of_heap(tt.element().heap_type()));
                let init = self.take_ref(at, kind)?.to_ref();
                let delta = self.index_at(at + 1, is_64)?;
                return Ok(self.control(StepOutcome::TableGrow {
                    table,
                    delta,
                    init,
                    at,
                    return_ip: next,
                }));
            }
            // Loads/stores: flat here (a second-level `match` per category cost a second
            // indirect branch per op). The address slot is the op's slot; a load's value replaces it.
            Op::I32Load(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<4>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i32(i32::from_le_bytes(b)))?;
            }
            Op::I64Load(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<8>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i64(i64::from_le_bytes(b)))?;
            }
            Op::F32Load(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<4>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::of_bytes(b))?;
            }
            Op::F64Load(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<8>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::of_bytes(b))?;
            }
            Op::I32Load8S(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<1>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i32(i32::from(b[0] as i8)))?;
            }
            Op::I32Load8U(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<1>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i32(i32::from(b[0])))?;
            }
            Op::I32Load16S(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<2>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i32(i32::from(i16::from_le_bytes(b))))?;
            }
            Op::I32Load16U(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<2>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i32(i32::from(u16::from_le_bytes(b))))?;
            }
            Op::I64Load8S(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<1>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i64(i64::from(b[0] as i8)))?;
            }
            Op::I64Load8U(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<1>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i64(i64::from(b[0])))?;
            }
            Op::I64Load16S(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<2>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i64(i64::from(i16::from_le_bytes(b))))?;
            }
            Op::I64Load16U(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<2>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i64(i64::from(u16::from_le_bytes(b))))?;
            }
            Op::I64Load32S(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<4>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i64(i64::from(i32::from_le_bytes(b))))?;
            }
            Op::I64Load32U(m, s) => {
                let at = slot(base, *s);
                let b = self.load_at::<4>(inner, instance, memory::resolve(code, m)?, at)?;
                self.set_scalar(at, Cell::from_i64(i64::from(u32::from_le_bytes(b))))?;
            }
            Op::I32Store(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_i32();
                self.store_at(
                    inner,
                    instance,
                    memory::resolve(code, m)?,
                    at,
                    v.to_le_bytes(),
                )?;
            }
            Op::I64Store(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_i64();
                self.store_at(
                    inner,
                    instance,
                    memory::resolve(code, m)?,
                    at,
                    v.to_le_bytes(),
                )?;
            }
            Op::F32Store(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_f32().to_bits();
                self.store_at(
                    inner,
                    instance,
                    memory::resolve(code, m)?,
                    at,
                    v.to_le_bytes(),
                )?;
            }
            Op::F64Store(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_f64().to_bits();
                self.store_at(
                    inner,
                    instance,
                    memory::resolve(code, m)?,
                    at,
                    v.to_le_bytes(),
                )?;
            }
            Op::I32Store8(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_i32() as u8;
                self.store_at(inner, instance, memory::resolve(code, m)?, at, [v])?;
            }
            Op::I32Store16(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_i32() as u16;
                self.store_at(
                    inner,
                    instance,
                    memory::resolve(code, m)?,
                    at,
                    v.to_le_bytes(),
                )?;
            }
            Op::I64Store8(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_i64() as u8;
                self.store_at(inner, instance, memory::resolve(code, m)?, at, [v])?;
            }
            Op::I64Store16(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_i64() as u16;
                self.store_at(
                    inner,
                    instance,
                    memory::resolve(code, m)?,
                    at,
                    v.to_le_bytes(),
                )?;
            }
            Op::I64Store32(m, s) => {
                let at = slot(base, *s);
                let v = self.scalar_at(at.saturating_add(1))?.unwrap_i64() as u32;
                self.store_at(
                    inner,
                    instance,
                    memory::resolve(code, m)?,
                    at,
                    v.to_le_bytes(),
                )?;
            }
            // The cold memory ops route to their category handler.
            op @ (Op::MemorySize(..)
            | Op::MemoryCopy(..)
            | Op::MemoryFill(..)
            | Op::MemoryInit(..)
            | Op::DataDrop(_)) => {
                self.exec_memory(inner, op, instance, base)?;
            }
            op @ (Op::RefNull(..) | Op::RefFunc(..) | Op::RefIsNull(_) | Op::RefAsNonNull(_)) => {
                self.exec_ref(inner, op, instance, base)?;
            }
            op @ (Op::TableInit { .. }
            | Op::TableCopy { .. }
            | Op::ElemDrop(_)
            | Op::TableGet(..)
            | Op::TableSet(..)
            | Op::TableSize(..)
            | Op::TableFill(..)) => self.exec_table(inner, op, instance, base)?,
            Op::Throw(tag, s) => {
                let outcome = self.throw(inner, instance, *tag, next - 1, slot(base, *s))?;
                return Ok(self.control(outcome));
            }
            Op::ThrowRef(s) => {
                let outcome = self.throw_ref(slot(base, *s))?;
                return Ok(self.control(outcome));
            }
            op @ Op::BrOnCast { .. } => {
                self.enter_window_of(base, op)?;
                if let Some(ip) = self.br_on_cast(inner, code, instance, op, false)? {
                    return Ok(ip);
                }
            }
            op @ Op::BrOnCastFail { .. } => {
                self.enter_window_of(base, op)?;
                if let Some(ip) = self.br_on_cast(inner, code, instance, op, true)? {
                    return Ok(ip);
                }
            }
            #[cfg(feature = "simd")]
            Op::Simd(s, top) => {
                self.enter_window(slot(base, *top))?;
                self.exec_simd(inner, code, s, instance)?;
            }
            // GC aggregate allocation: ensure the limiter-granted reservation covers it first (with
            // operands still on the stack as roots); a reservation grow suspends and re-executes.
            // `array.new_data`/`array.new_elem` route through here too — their charge is clamped to
            // the source segment size (`seg_clamped_charge`), so a would-be-out-of-bounds count
            // reserves only a bounded amount and still traps with the correct error in its handler.
            alloc @ (Op::StructNew(..)
            | Op::StructNewDefault(..)
            | Op::ArrayNew(..)
            | Op::ArrayNewDefault(..)
            | Op::ArrayNewFixed { .. }
            | Op::ArrayNewData { .. }
            | Op::ArrayNewElem { .. }) => {
                self.enter_window_of(base, alloc)?;
                if let Some(charge) = self.gc_alloc_charge(inner, instance, alloc)? {
                    if let Some(out) = self.gc_reserve(inner, charge, next - 1)? {
                        return Ok(self.control(out));
                    }
                }
                self.exec_gc(inner, alloc, instance)?;
            }
            // Numerics: flat for the same reason as loads/stores.
            // --- i32 ---
            Op::I32Eqz(s) => self.i32_unop(slot(base, *s), |a| i32::from(a == 0))?,
            Op::I32Eq(s) => self.i32_relop(slot(base, *s), |a, b| a == b)?,
            Op::I32Ne(s) => self.i32_relop(slot(base, *s), |a, b| a != b)?,
            Op::I32LtS(s) => self.i32_relop(slot(base, *s), |a, b| a < b)?,
            Op::I32LtU(s) => self.u32_relop(slot(base, *s), |a, b| a < b)?,
            Op::I32GtS(s) => self.i32_relop(slot(base, *s), |a, b| a > b)?,
            Op::I32GtU(s) => self.u32_relop(slot(base, *s), |a, b| a > b)?,
            Op::I32LeS(s) => self.i32_relop(slot(base, *s), |a, b| a <= b)?,
            Op::I32LeU(s) => self.u32_relop(slot(base, *s), |a, b| a <= b)?,
            Op::I32GeS(s) => self.i32_relop(slot(base, *s), |a, b| a >= b)?,
            Op::I32GeU(s) => self.u32_relop(slot(base, *s), |a, b| a >= b)?,
            Op::I32Clz(s) => self.i32_unop(slot(base, *s), |a| a.leading_zeros() as i32)?,
            Op::I32Ctz(s) => self.i32_unop(slot(base, *s), |a| a.trailing_zeros() as i32)?,
            Op::I32Popcnt(s) => self.i32_unop(slot(base, *s), |a| a.count_ones() as i32)?,
            Op::I32Add(s) => self.i32_binop(slot(base, *s), i32::wrapping_add)?,
            Op::I32Sub(s) => self.i32_binop(slot(base, *s), i32::wrapping_sub)?,
            Op::I32Mul(s) => self.i32_binop(slot(base, *s), i32::wrapping_mul)?,
            Op::I32DivS(s) => self.i32_try_binop(slot(base, *s), arith::div_s_i32)?,
            Op::I32DivU(s) => {
                self.u32_try_binop(slot(base, *s), |a, b| arith::nz(b).map(|()| a / b))?;
            }
            Op::I32RemS(s) => self.i32_try_binop(slot(base, *s), |a, b| {
                arith::nz(b).map(|()| a.wrapping_rem(b))
            })?,
            Op::I32RemU(s) => {
                self.u32_try_binop(slot(base, *s), |a, b| arith::nz(b).map(|()| a % b))?;
            }
            Op::I32And(s) => self.i32_binop(slot(base, *s), |a, b| a & b)?,
            Op::I32Or(s) => self.i32_binop(slot(base, *s), |a, b| a | b)?,
            Op::I32Xor(s) => self.i32_binop(slot(base, *s), |a, b| a ^ b)?,
            Op::I32Shl(s) => self.i32_binop(slot(base, *s), |a, b| a.wrapping_shl(b as u32))?,
            Op::I32ShrS(s) => self.i32_binop(slot(base, *s), |a, b| a.wrapping_shr(b as u32))?,
            Op::I32ShrU(s) => self.u32_binop(slot(base, *s), u32::wrapping_shr)?,
            Op::I32Rotl(s) => self.u32_binop(slot(base, *s), |a, b| a.rotate_left(b % 32))?,
            Op::I32Rotr(s) => self.u32_binop(slot(base, *s), |a, b| a.rotate_right(b % 32))?,

            // --- i64 ---
            Op::I64Eqz(s) => self.map_i64_i32(slot(base, *s), |a| i32::from(a == 0))?,
            Op::I64Eq(s) => self.i64_relop(slot(base, *s), |a, b| a == b)?,
            Op::I64Ne(s) => self.i64_relop(slot(base, *s), |a, b| a != b)?,
            Op::I64LtS(s) => self.i64_relop(slot(base, *s), |a, b| a < b)?,
            Op::I64LtU(s) => self.u64_relop(slot(base, *s), |a, b| a < b)?,
            Op::I64GtS(s) => self.i64_relop(slot(base, *s), |a, b| a > b)?,
            Op::I64GtU(s) => self.u64_relop(slot(base, *s), |a, b| a > b)?,
            Op::I64LeS(s) => self.i64_relop(slot(base, *s), |a, b| a <= b)?,
            Op::I64LeU(s) => self.u64_relop(slot(base, *s), |a, b| a <= b)?,
            Op::I64GeS(s) => self.i64_relop(slot(base, *s), |a, b| a >= b)?,
            Op::I64GeU(s) => self.u64_relop(slot(base, *s), |a, b| a >= b)?,
            Op::I64Clz(s) => self.i64_unop(slot(base, *s), |a| i64::from(a.leading_zeros()))?,
            Op::I64Ctz(s) => self.i64_unop(slot(base, *s), |a| i64::from(a.trailing_zeros()))?,
            Op::I64Popcnt(s) => self.i64_unop(slot(base, *s), |a| i64::from(a.count_ones()))?,
            Op::I64Add(s) => self.i64_binop(slot(base, *s), i64::wrapping_add)?,
            Op::I64Sub(s) => self.i64_binop(slot(base, *s), i64::wrapping_sub)?,
            Op::I64Mul(s) => self.i64_binop(slot(base, *s), i64::wrapping_mul)?,
            Op::I64DivS(s) => self.i64_try_binop(slot(base, *s), arith::div_s_i64)?,
            Op::I64DivU(s) => {
                self.u64_try_binop(slot(base, *s), |a, b| arith::nz(b).map(|()| a / b))?;
            }
            Op::I64RemS(s) => self.i64_try_binop(slot(base, *s), |a, b| {
                arith::nz(b).map(|()| a.wrapping_rem(b))
            })?,
            Op::I64RemU(s) => {
                self.u64_try_binop(slot(base, *s), |a, b| arith::nz(b).map(|()| a % b))?;
            }
            Op::I64And(s) => self.i64_binop(slot(base, *s), |a, b| a & b)?,
            Op::I64Or(s) => self.i64_binop(slot(base, *s), |a, b| a | b)?,
            Op::I64Xor(s) => self.i64_binop(slot(base, *s), |a, b| a ^ b)?,
            Op::I64Shl(s) => self.i64_binop(slot(base, *s), |a, b| a.wrapping_shl(b as u32))?,
            Op::I64ShrS(s) => self.i64_binop(slot(base, *s), |a, b| a.wrapping_shr(b as u32))?,
            Op::I64ShrU(s) => self.u64_binop(slot(base, *s), |a, b| a.wrapping_shr(b as u32))?,
            Op::I64Rotl(s) => {
                self.u64_binop(slot(base, *s), |a, b| a.rotate_left((b % 64) as u32))?;
            }
            Op::I64Rotr(s) => {
                self.u64_binop(slot(base, *s), |a, b| a.rotate_right((b % 64) as u32))?;
            }

            // --- f32 ---
            Op::F32Eq(s) => self.f32_relop(slot(base, *s), |a, b| a == b)?,
            Op::F32Ne(s) => self.f32_relop(slot(base, *s), |a, b| a != b)?,
            Op::F32Lt(s) => self.f32_relop(slot(base, *s), |a, b| a < b)?,
            Op::F32Gt(s) => self.f32_relop(slot(base, *s), |a, b| a > b)?,
            Op::F32Le(s) => self.f32_relop(slot(base, *s), |a, b| a <= b)?,
            Op::F32Ge(s) => self.f32_relop(slot(base, *s), |a, b| a >= b)?,
            Op::F32Abs(s) => self.f32_unop(slot(base, *s), f32::abs)?,
            Op::F32Neg(s) => self.f32_unop(slot(base, *s), |a| -a)?,
            Op::F32Ceil(s) => self.f32_unop_canon(slot(base, *s), f32::ceil)?,
            Op::F32Floor(s) => self.f32_unop_canon(slot(base, *s), f32::floor)?,
            Op::F32Trunc(s) => self.f32_unop_canon(slot(base, *s), f32::trunc)?,
            Op::F32Nearest(s) => self.f32_unop_canon(slot(base, *s), f32::round_ties_even)?,
            Op::F32Sqrt(s) => self.f32_unop_canon(slot(base, *s), f32::sqrt)?,
            Op::F32Add(s) => self.f32_arith(slot(base, *s), |a, b| a + b)?,
            Op::F32Sub(s) => self.f32_arith(slot(base, *s), |a, b| a - b)?,
            Op::F32Mul(s) => self.f32_arith(slot(base, *s), |a, b| a * b)?,
            Op::F32Div(s) => self.f32_arith(slot(base, *s), |a, b| a / b)?,
            Op::F32Min(s) => self.f32_binop(slot(base, *s), arith::f32_min)?,
            Op::F32Max(s) => self.f32_binop(slot(base, *s), arith::f32_max)?,
            Op::F32Copysign(s) => self.f32_binop(slot(base, *s), f32::copysign)?,

            // --- f64 ---
            Op::F64Eq(s) => self.f64_relop(slot(base, *s), |a, b| a == b)?,
            Op::F64Ne(s) => self.f64_relop(slot(base, *s), |a, b| a != b)?,
            Op::F64Lt(s) => self.f64_relop(slot(base, *s), |a, b| a < b)?,
            Op::F64Gt(s) => self.f64_relop(slot(base, *s), |a, b| a > b)?,
            Op::F64Le(s) => self.f64_relop(slot(base, *s), |a, b| a <= b)?,
            Op::F64Ge(s) => self.f64_relop(slot(base, *s), |a, b| a >= b)?,
            Op::F64Abs(s) => self.f64_unop(slot(base, *s), f64::abs)?,
            Op::F64Neg(s) => self.f64_unop(slot(base, *s), |a| -a)?,
            Op::F64Ceil(s) => self.f64_unop_canon(slot(base, *s), f64::ceil)?,
            Op::F64Floor(s) => self.f64_unop_canon(slot(base, *s), f64::floor)?,
            Op::F64Trunc(s) => self.f64_unop_canon(slot(base, *s), f64::trunc)?,
            Op::F64Nearest(s) => self.f64_unop_canon(slot(base, *s), f64::round_ties_even)?,
            Op::F64Sqrt(s) => self.f64_unop_canon(slot(base, *s), f64::sqrt)?,
            Op::F64Add(s) => self.f64_arith(slot(base, *s), |a, b| a + b)?,
            Op::F64Sub(s) => self.f64_arith(slot(base, *s), |a, b| a - b)?,
            Op::F64Mul(s) => self.f64_arith(slot(base, *s), |a, b| a * b)?,
            Op::F64Div(s) => self.f64_arith(slot(base, *s), |a, b| a / b)?,
            Op::F64Min(s) => self.f64_binop(slot(base, *s), arith::f64_min)?,
            Op::F64Max(s) => self.f64_binop(slot(base, *s), arith::f64_max)?,
            Op::F64Copysign(s) => self.f64_binop(slot(base, *s), f64::copysign)?,

            // --- conversions ---
            Op::I32WrapI64(s) => self.map_i64_i32(slot(base, *s), convert::i32_wrap_i64)?,
            Op::I64ExtendI32S(s) => self.map_i32_i64(slot(base, *s), convert::i64_extend_i32_s)?,
            Op::I64ExtendI32U(s) => self.map_i32_i64(slot(base, *s), convert::i64_extend_i32_u)?,
            Op::I32TruncF32S(s) => self.trunc_to_i32(slot(base, *s), convert::i32_trunc_f32_s)?,
            Op::I32TruncF32U(s) => self.trunc_to_i32(slot(base, *s), convert::i32_trunc_f32_u)?,
            Op::I32TruncF64S(s) => self.trunc64_to_i32(slot(base, *s), convert::i32_trunc_f64_s)?,
            Op::I32TruncF64U(s) => self.trunc64_to_i32(slot(base, *s), convert::i32_trunc_f64_u)?,
            Op::I64TruncF32S(s) => self.trunc_to_i64(slot(base, *s), convert::i64_trunc_f32_s)?,
            Op::I64TruncF32U(s) => self.trunc_to_i64(slot(base, *s), convert::i64_trunc_f32_u)?,
            Op::I64TruncF64S(s) => self.trunc64_to_i64(slot(base, *s), convert::i64_trunc_f64_s)?,
            Op::I64TruncF64U(s) => self.trunc64_to_i64(slot(base, *s), convert::i64_trunc_f64_u)?,
            Op::I32TruncSatF32S(s) => {
                self.map_f32_i32(slot(base, *s), convert::i32_trunc_sat_f32_s)?;
            }
            Op::I32TruncSatF32U(s) => {
                self.map_f32_i32(slot(base, *s), convert::i32_trunc_sat_f32_u)?;
            }
            Op::I32TruncSatF64S(s) => {
                self.map_f64_i32(slot(base, *s), convert::i32_trunc_sat_f64_s)?;
            }
            Op::I32TruncSatF64U(s) => {
                self.map_f64_i32(slot(base, *s), convert::i32_trunc_sat_f64_u)?;
            }
            Op::I64TruncSatF32S(s) => {
                self.map_f32_i64(slot(base, *s), convert::i64_trunc_sat_f32_s)?;
            }
            Op::I64TruncSatF32U(s) => {
                self.map_f32_i64(slot(base, *s), convert::i64_trunc_sat_f32_u)?;
            }
            Op::I64TruncSatF64S(s) => {
                self.map_f64_i64(slot(base, *s), convert::i64_trunc_sat_f64_s)?;
            }
            Op::I64TruncSatF64U(s) => {
                self.map_f64_i64(slot(base, *s), convert::i64_trunc_sat_f64_u)?;
            }
            Op::F32ConvertI32S(s) => {
                self.map_i32_f32(slot(base, *s), convert::f32_convert_i32_s)?;
            }
            Op::F32ConvertI32U(s) => {
                self.map_i32_f32(slot(base, *s), convert::f32_convert_i32_u)?;
            }
            Op::F32ConvertI64S(s) => {
                self.map_i64_f32(slot(base, *s), convert::f32_convert_i64_s)?;
            }
            Op::F32ConvertI64U(s) => {
                self.map_i64_f32(slot(base, *s), convert::f32_convert_i64_u)?;
            }
            Op::F32DemoteF64(s) => self.map_f64_f32(slot(base, *s), convert::f32_demote_f64)?,
            Op::F64ConvertI32S(s) => {
                self.map_i32_f64(slot(base, *s), convert::f64_convert_i32_s)?;
            }
            Op::F64ConvertI32U(s) => {
                self.map_i32_f64(slot(base, *s), convert::f64_convert_i32_u)?;
            }
            Op::F64ConvertI64S(s) => {
                self.map_i64_f64(slot(base, *s), convert::f64_convert_i64_s)?;
            }
            Op::F64ConvertI64U(s) => {
                self.map_i64_f64(slot(base, *s), convert::f64_convert_i64_u)?;
            }
            Op::F64PromoteF32(s) => self.map_f32_f64(slot(base, *s), convert::f64_promote_f32)?,
            Op::I32ReinterpretF32(s) => self.map_f32_i32(slot(base, *s), |x| x.to_bits() as i32)?,
            Op::I64ReinterpretF64(s) => self.map_f64_i64(slot(base, *s), |x| x.to_bits() as i64)?,
            Op::F32ReinterpretI32(s) => {
                self.map_i32_f32(slot(base, *s), |x| f32::from_bits(x as u32))?;
            }
            Op::F64ReinterpretI64(s) => {
                self.map_i64_f64(slot(base, *s), |x| f64::from_bits(x as u64))?;
            }

            // --- sign-extension ---
            Op::I32Extend8S(s) => self.i32_unop(slot(base, *s), |a| i32::from(a as i8))?,
            Op::I32Extend16S(s) => self.i32_unop(slot(base, *s), |a| i32::from(a as i16))?,
            Op::I64Extend8S(s) => self.i64_unop(slot(base, *s), |a| i64::from(a as i8))?,
            Op::I64Extend16S(s) => self.i64_unop(slot(base, *s), |a| i64::from(a as i16))?,
            Op::I64Extend32S(s) => self.i64_unop(slot(base, *s), |a| i64::from(a as i32))?,
            // The colder GC/cast categories fall through: exec_gc → exec_gc_array → exec_cast.
            other => {
                self.enter_window_of(base, other)?;
                self.exec_gc(inner, other, instance)?;
            }
        }
        Ok(next)
    }

    /// Enters a stack-window op's operand window (its handler pops/pushes relative to `top`).
    #[inline]
    fn enter_window_of(&mut self, base: u32, op: &Op) -> Result<()> {
        let top = op
            .window_top()
            .ok_or(crate::error::InternalError::Compiler(
                "op has no operand window",
            ))?;
        self.enter_window(slot(base, top))
    }

    /// Leaves `outcome` for the run loop and returns the [`CONTROL`] marker.
    #[inline]
    fn control(&mut self, outcome: StepOutcome) -> u32 {
        self.control = Some(outcome);
        CONTROL
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
#[inline]
fn call_kind(tail: bool, next: u32) -> CallKind {
    if tail {
        CallKind::Tail
    } else {
        CallKind::Nested(next)
    }
}

/// Lookups into the executing instance's index spaces. Their indices are wasmparser-validated for
/// that instance (#33 carve-out); scoping the indexing allowance here keeps the dispatch arms
/// themselves under `clippy::indexing_slicing`.
#[allow(clippy::indexing_slicing)]
mod index_space {
    use crate::extern_::{Global, Memory, Table};
    use crate::func::Func;
    use crate::instance::Instance;
    use crate::store::StoreInner;

    #[inline]
    pub(super) fn global(inner: &StoreInner, instance: Instance, index: u32) -> Global {
        inner.instance(instance).globals[index as usize]
    }

    #[inline]
    pub(super) fn func(inner: &StoreInner, instance: Instance, index: u32) -> Func {
        inner.instance(instance).funcs[index as usize]
    }

    #[inline]
    pub(super) fn memory(inner: &StoreInner, instance: Instance, index: u32) -> Memory {
        inner.instance(instance).memories[index as usize]
    }

    #[inline]
    pub(super) fn table(inner: &StoreInner, instance: Instance, index: u32) -> Table {
        inner.instance(instance).tables[index as usize]
    }
}

/// The absolute operand-stack index of frame slot `s` (`base` = the frame's first local).
/// Saturating: an out-of-range slot fails the access's bounds check instead of wrapping.
#[allow(clippy::inline_always)]
#[inline(always)]
pub(super) fn slot(base: u32, s: crate::module::op::Slot) -> usize {
    (base as usize).saturating_add(s as usize)
}
