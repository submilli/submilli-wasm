//! The flat opcode dispatch: one `step` per instruction, delegating to the per-category handlers.
//! See ARCHITECTURE §7. The big `match` is the interpreter's single sanctioned long function.

// `f*.eq`/`f*.ne` are exact IEEE comparisons by definition.
#![allow(clippy::float_cmp)]

use super::call::{self, CallKind};
use super::{arith, cell, convert, Execution, StepOutcome, CONTROL};
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
            Op::I32Const(v) => self.push_i32(*v),
            Op::I64Const(v) => self.push_i64(*v),
            Op::F32Const(v) => self.push_f32_bits(*v),
            Op::F64Const(v) => self.push_f64_bits(*v),
            Op::Drop => {
                self.pop()?;
            }
            Op::Select => {
                let (cell, tag) = self.pop_select()?;
                self.push_cell(cell, tag);
            }
            Op::LocalGet(i) => {
                let local = base
                    .checked_add(*i)
                    .ok_or(crate::error::InternalError::FrameStack(
                        "local index overflow",
                    ))?;
                let (cell, tag) = self.cell_at(local as usize)?;
                self.push_cell(cell, tag);
            }
            Op::LocalSet(i) => {
                let (cell, tag) = self.pop_tagged()?;
                let local = base
                    .checked_add(*i)
                    .ok_or(crate::error::InternalError::FrameStack(
                        "local index overflow",
                    ))?;
                self.set_cell(local as usize, cell, tag)?;
            }
            Op::LocalTee(i) => {
                let (cell, tag) = self.top_cell()?;
                let local = base
                    .checked_add(*i)
                    .ok_or(crate::error::InternalError::FrameStack(
                        "local index overflow",
                    ))?;
                self.set_cell(local as usize, cell, tag)?;
            }
            Op::GlobalGet(g) => {
                let handle = index_space::global(inner, instance, *g);
                let v = inner.global(handle).value;
                self.push(v);
            }
            Op::GlobalSet(g) => {
                let handle = index_space::global(inner, instance, *g);
                let v = self.pop_typed(inner.global(handle).ty.content())?;
                inner.global_mut(handle).value = v;
            }
            Op::Br(t) => {
                self.take_branch(*t)?;
                return Ok(t.ip);
            }
            Op::BrIf(t) => {
                if self.pop_i32()? != 0 {
                    self.take_branch(*t)?;
                    return Ok(t.ip);
                }
            }
            Op::BrIfNot(t) => {
                if self.pop_i32()? == 0 {
                    self.take_branch(*t)?;
                    return Ok(t.ip);
                }
            }
            Op::BrIfCmp {
                kind,
                negate,
                target,
            } => {
                let (a, b) = self.pop_i32_pair()?;
                if cmp_i32(*kind, a, b) != *negate {
                    self.take_branch(*target)?;
                    return Ok(target.ip);
                }
            }
            Op::BrTable(range) => {
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
                let i = self.pop_i32()? as u32 as usize;
                let t = cases.get(i).unwrap_or(default);
                self.take_branch(*t)?;
                return Ok(t.ip);
            }
            call_op @ (Op::Call(f) | Op::ReturnCall(f)) => {
                let callee = index_space::func(inner, instance, *f);
                let kind = call_kind(matches!(call_op, Op::ReturnCall(_)), next);
                let outcome =
                    call::call_outcome(inner, call::resolve(inner, callee), instance, kind);
                return Ok(self.control(outcome));
            }
            Op::CallIndirect { type_idx, table } => {
                let kind = CallKind::Nested(next);
                let outcome = self.do_call_indirect(inner, instance, *type_idx, *table, kind)?;
                return Ok(self.control(outcome));
            }
            Op::ReturnCallIndirect { type_idx, table } => {
                let kind = CallKind::Tail;
                let outcome = self.do_call_indirect(inner, instance, *type_idx, *table, kind)?;
                return Ok(self.control(outcome));
            }
            Op::CallRef(_) => {
                let outcome = self.do_call_ref(inner, instance, CallKind::Nested(next))?;
                return Ok(self.control(outcome));
            }
            Op::ReturnCallRef(_) => {
                let outcome = self.do_call_ref(inner, instance, CallKind::Tail)?;
                return Ok(self.control(outcome));
            }
            Op::BrOnNull(t) => {
                let (r, tag) = self.pop_tagged_ref()?;
                if r.is_null() {
                    self.take_branch(*t)?;
                    return Ok(t.ip);
                }
                self.push_cell(r, tag); // non-null: keep it, fall through
            }
            Op::BrOnNonNull(t) => {
                let (r, tag) = self.pop_tagged_ref()?;
                if !r.is_null() {
                    self.push_cell(r, tag); // non-null: keep it on the branch target
                    self.take_branch(*t)?;
                    return Ok(t.ip);
                }
                // null: reference dropped, fall through
            }
            Op::MemoryGrow(i) => {
                // Routed through the driver so the (T-generic) limiter is consulted.
                let memory = index_space::memory(inner, instance, *i);
                let delta = self.pop_index(inner.memory(memory).ty.is_64())?;
                return Ok(self.control(StepOutcome::Grow {
                    memory,
                    delta,
                    return_ip: next,
                }));
            }
            Op::TableGrow(t) => {
                // Routed through the driver (limiter-consulted), like `memory.grow`.
                let table = index_space::table(inner, instance, *t);
                let tt = &inner.table(table).ty;
                let (is_64, kind) = (tt.is_64(), cell::refkind_of_heap(tt.element().heap_type()));
                let delta = self.pop_index(is_64)?;
                let init = self.pop_ref(kind)?.to_ref();
                return Ok(self.control(StepOutcome::TableGrow {
                    table,
                    delta,
                    init,
                    return_ip: next,
                }));
            }
            // Loads/stores: flat here (a second-level `match` per category cost a second
            // indirect branch per op).
            Op::I32Load(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_i32(i32::from_le_bytes(b));
            }
            Op::I64Load(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_i64(i64::from_le_bytes(b));
            }
            Op::F32Load(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_f32_bits(u32::from_le_bytes(b));
            }
            Op::F64Load(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_f64_bits(u64::from_le_bytes(b));
            }
            Op::I32Load8S(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i32(i32::from(b[0] as i8));
            }
            Op::I32Load8U(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i32(i32::from(b[0]));
            }
            Op::I32Load16S(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i32(i32::from(i16::from_le_bytes(b)));
            }
            Op::I32Load16U(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i32(i32::from(u16::from_le_bytes(b)));
            }
            Op::I64Load8S(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i64(i64::from(b[0] as i8));
            }
            Op::I64Load8U(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_i64(i64::from(b[0]));
            }
            Op::I64Load16S(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i64(i64::from(i16::from_le_bytes(b)));
            }
            Op::I64Load16U(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_i64(i64::from(u16::from_le_bytes(b)));
            }
            Op::I64Load32S(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_i64(i64::from(i32::from_le_bytes(b)));
            }
            Op::I64Load32U(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_i64(i64::from(u32::from_le_bytes(b)));
            }
            Op::I32Store(m) => {
                let v = self.pop_i32()?;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I64Store(m) => {
                let v = self.pop_i64()?;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::F32Store(m) => {
                let v = self.pop_f32()?.to_bits();
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::F64Store(m) => {
                let v = self.pop_f64()?.to_bits();
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I32Store8(m) => {
                let v = self.pop_i32()? as u8;
                self.store_n(inner, code, instance, m, [v])?;
            }
            Op::I32Store16(m) => {
                let v = self.pop_i32()? as u16;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I64Store8(m) => {
                let v = self.pop_i64()? as u8;
                self.store_n(inner, code, instance, m, [v])?;
            }
            Op::I64Store16(m) => {
                let v = self.pop_i64()? as u16;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            Op::I64Store32(m) => {
                let v = self.pop_i64()? as u32;
                self.store_n(inner, code, instance, m, v.to_le_bytes())?;
            }
            // The cold memory ops route to their category handler.
            op @ (Op::MemorySize(_)
            | Op::MemoryCopy(..)
            | Op::MemoryFill(_)
            | Op::MemoryInit(..)
            | Op::DataDrop(_)) => {
                self.exec_memory(inner, code, op, instance)?;
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
            Op::Throw(tag) => {
                let outcome = self.throw(inner, instance, *tag, next - 1)?;
                return Ok(self.control(outcome));
            }
            Op::ThrowRef => {
                let outcome = self.throw_ref()?;
                return Ok(self.control(outcome));
            }
            op @ Op::BrOnCast { .. } => {
                if let Some(ip) = self.br_on_cast(inner, code, instance, op, false)? {
                    return Ok(ip);
                }
            }
            op @ Op::BrOnCastFail { .. } => {
                if let Some(ip) = self.br_on_cast(inner, code, instance, op, true)? {
                    return Ok(ip);
                }
            }
            #[cfg(feature = "simd")]
            Op::Simd(s) => self.exec_simd(inner, code, s, instance)?,
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
                    if let Some(out) = self.gc_reserve(inner, charge, next - 1)? {
                        return Ok(self.control(out));
                    }
                }
                self.exec_gc(inner, alloc, instance)?;
            }
            // Numerics: flat for the same reason as loads/stores.
            // --- i32 ---
            Op::I32Eqz => self.i32_unop(|a| i32::from(a == 0))?,
            Op::I32Eq => self.i32_relop(|a, b| a == b)?,
            Op::I32Ne => self.i32_relop(|a, b| a != b)?,
            Op::I32LtS => self.i32_relop(|a, b| a < b)?,
            Op::I32LtU => self.u32_relop(|a, b| a < b)?,
            Op::I32GtS => self.i32_relop(|a, b| a > b)?,
            Op::I32GtU => self.u32_relop(|a, b| a > b)?,
            Op::I32LeS => self.i32_relop(|a, b| a <= b)?,
            Op::I32LeU => self.u32_relop(|a, b| a <= b)?,
            Op::I32GeS => self.i32_relop(|a, b| a >= b)?,
            Op::I32GeU => self.u32_relop(|a, b| a >= b)?,
            Op::I32Clz => self.i32_unop(|a| a.leading_zeros() as i32)?,
            Op::I32Ctz => self.i32_unop(|a| a.trailing_zeros() as i32)?,
            Op::I32Popcnt => self.i32_unop(|a| a.count_ones() as i32)?,
            Op::I32Add => self.i32_binop(i32::wrapping_add)?,
            Op::I32Sub => self.i32_binop(i32::wrapping_sub)?,
            Op::I32Mul => self.i32_binop(i32::wrapping_mul)?,
            Op::I32DivS => self.i32_try_binop(arith::div_s_i32)?,
            Op::I32DivU => self.u32_try_binop(|a, b| arith::nz(b).map(|()| a / b))?,
            Op::I32RemS => self.i32_try_binop(|a, b| arith::nz(b).map(|()| a.wrapping_rem(b)))?,
            Op::I32RemU => self.u32_try_binop(|a, b| arith::nz(b).map(|()| a % b))?,
            Op::I32And => self.i32_binop(|a, b| a & b)?,
            Op::I32Or => self.i32_binop(|a, b| a | b)?,
            Op::I32Xor => self.i32_binop(|a, b| a ^ b)?,
            Op::I32Shl => self.i32_binop(|a, b| a.wrapping_shl(b as u32))?,
            Op::I32ShrS => self.i32_binop(|a, b| a.wrapping_shr(b as u32))?,
            Op::I32ShrU => self.u32_binop(u32::wrapping_shr)?,
            Op::I32Rotl => self.u32_binop(|a, b| a.rotate_left(b % 32))?,
            Op::I32Rotr => self.u32_binop(|a, b| a.rotate_right(b % 32))?,

            // --- i64 ---
            Op::I64Eqz => {
                let a = self.pop_i64()?;
                self.push_i32(i32::from(a == 0));
            }
            Op::I64Eq => self.i64_relop(|a, b| a == b)?,
            Op::I64Ne => self.i64_relop(|a, b| a != b)?,
            Op::I64LtS => self.i64_relop(|a, b| a < b)?,
            Op::I64LtU => self.u64_relop(|a, b| a < b)?,
            Op::I64GtS => self.i64_relop(|a, b| a > b)?,
            Op::I64GtU => self.u64_relop(|a, b| a > b)?,
            Op::I64LeS => self.i64_relop(|a, b| a <= b)?,
            Op::I64LeU => self.u64_relop(|a, b| a <= b)?,
            Op::I64GeS => self.i64_relop(|a, b| a >= b)?,
            Op::I64GeU => self.u64_relop(|a, b| a >= b)?,
            Op::I64Clz => self.i64_unop(|a| i64::from(a.leading_zeros()))?,
            Op::I64Ctz => self.i64_unop(|a| i64::from(a.trailing_zeros()))?,
            Op::I64Popcnt => self.i64_unop(|a| i64::from(a.count_ones()))?,
            Op::I64Add => self.i64_binop(i64::wrapping_add)?,
            Op::I64Sub => self.i64_binop(i64::wrapping_sub)?,
            Op::I64Mul => self.i64_binop(i64::wrapping_mul)?,
            Op::I64DivS => self.i64_try_binop(arith::div_s_i64)?,
            Op::I64DivU => self.u64_try_binop(|a, b| arith::nz(b).map(|()| a / b))?,
            Op::I64RemS => self.i64_try_binop(|a, b| arith::nz(b).map(|()| a.wrapping_rem(b)))?,
            Op::I64RemU => self.u64_try_binop(|a, b| arith::nz(b).map(|()| a % b))?,
            Op::I64And => self.i64_binop(|a, b| a & b)?,
            Op::I64Or => self.i64_binop(|a, b| a | b)?,
            Op::I64Xor => self.i64_binop(|a, b| a ^ b)?,
            Op::I64Shl => self.i64_binop(|a, b| a.wrapping_shl(b as u32))?,
            Op::I64ShrS => self.i64_binop(|a, b| a.wrapping_shr(b as u32))?,
            Op::I64ShrU => self.u64_binop(|a, b| a.wrapping_shr(b as u32))?,
            Op::I64Rotl => self.u64_binop(|a, b| a.rotate_left((b % 64) as u32))?,
            Op::I64Rotr => self.u64_binop(|a, b| a.rotate_right((b % 64) as u32))?,

            // --- f32 ---
            Op::F32Eq => self.f32_relop(|a, b| a == b)?,
            Op::F32Ne => self.f32_relop(|a, b| a != b)?,
            Op::F32Lt => self.f32_relop(|a, b| a < b)?,
            Op::F32Gt => self.f32_relop(|a, b| a > b)?,
            Op::F32Le => self.f32_relop(|a, b| a <= b)?,
            Op::F32Ge => self.f32_relop(|a, b| a >= b)?,
            Op::F32Abs => self.f32_unop(f32::abs)?,
            Op::F32Neg => self.f32_unop(|a| -a)?,
            Op::F32Ceil => self.f32_unop_canon(f32::ceil)?,
            Op::F32Floor => self.f32_unop_canon(f32::floor)?,
            Op::F32Trunc => self.f32_unop_canon(f32::trunc)?,
            Op::F32Nearest => self.f32_unop_canon(f32::round_ties_even)?,
            Op::F32Sqrt => self.f32_unop_canon(f32::sqrt)?,
            Op::F32Add => self.f32_arith(|a, b| a + b)?,
            Op::F32Sub => self.f32_arith(|a, b| a - b)?,
            Op::F32Mul => self.f32_arith(|a, b| a * b)?,
            Op::F32Div => self.f32_arith(|a, b| a / b)?,
            Op::F32Min => self.f32_binop(arith::f32_min)?,
            Op::F32Max => self.f32_binop(arith::f32_max)?,
            Op::F32Copysign => self.f32_binop(f32::copysign)?,

            // --- f64 ---
            Op::F64Eq => self.f64_relop(|a, b| a == b)?,
            Op::F64Ne => self.f64_relop(|a, b| a != b)?,
            Op::F64Lt => self.f64_relop(|a, b| a < b)?,
            Op::F64Gt => self.f64_relop(|a, b| a > b)?,
            Op::F64Le => self.f64_relop(|a, b| a <= b)?,
            Op::F64Ge => self.f64_relop(|a, b| a >= b)?,
            Op::F64Abs => self.f64_unop(f64::abs)?,
            Op::F64Neg => self.f64_unop(|a| -a)?,
            Op::F64Ceil => self.f64_unop_canon(f64::ceil)?,
            Op::F64Floor => self.f64_unop_canon(f64::floor)?,
            Op::F64Trunc => self.f64_unop_canon(f64::trunc)?,
            Op::F64Nearest => self.f64_unop_canon(f64::round_ties_even)?,
            Op::F64Sqrt => self.f64_unop_canon(f64::sqrt)?,
            Op::F64Add => self.f64_arith(|a, b| a + b)?,
            Op::F64Sub => self.f64_arith(|a, b| a - b)?,
            Op::F64Mul => self.f64_arith(|a, b| a * b)?,
            Op::F64Div => self.f64_arith(|a, b| a / b)?,
            Op::F64Min => self.f64_binop(arith::f64_min)?,
            Op::F64Max => self.f64_binop(arith::f64_max)?,
            Op::F64Copysign => self.f64_binop(f64::copysign)?,

            // --- conversions ---
            Op::I32WrapI64 => {
                let x = self.pop_i64()?;
                self.push_i32(convert::i32_wrap_i64(x));
            }
            Op::I64ExtendI32S => {
                let x = self.pop_i32()?;
                self.push_i64(convert::i64_extend_i32_s(x));
            }
            Op::I64ExtendI32U => {
                let x = self.pop_i32()?;
                self.push_i64(convert::i64_extend_i32_u(x));
            }
            Op::I32TruncF32S => self.trunc_to_i32(convert::i32_trunc_f32_s)?,
            Op::I32TruncF32U => self.trunc_to_i32(convert::i32_trunc_f32_u)?,
            Op::I32TruncF64S => self.trunc64_to_i32(convert::i32_trunc_f64_s)?,
            Op::I32TruncF64U => self.trunc64_to_i32(convert::i32_trunc_f64_u)?,
            Op::I64TruncF32S => self.trunc_to_i64(convert::i64_trunc_f32_s)?,
            Op::I64TruncF32U => self.trunc_to_i64(convert::i64_trunc_f32_u)?,
            Op::I64TruncF64S => self.trunc64_to_i64(convert::i64_trunc_f64_s)?,
            Op::I64TruncF64U => self.trunc64_to_i64(convert::i64_trunc_f64_u)?,
            Op::I32TruncSatF32S => self.map_f32_i32(convert::i32_trunc_sat_f32_s)?,
            Op::I32TruncSatF32U => self.map_f32_i32(convert::i32_trunc_sat_f32_u)?,
            Op::I32TruncSatF64S => self.map_f64_i32(convert::i32_trunc_sat_f64_s)?,
            Op::I32TruncSatF64U => self.map_f64_i32(convert::i32_trunc_sat_f64_u)?,
            Op::I64TruncSatF32S => self.map_f32_i64(convert::i64_trunc_sat_f32_s)?,
            Op::I64TruncSatF32U => self.map_f32_i64(convert::i64_trunc_sat_f32_u)?,
            Op::I64TruncSatF64S => self.map_f64_i64(convert::i64_trunc_sat_f64_s)?,
            Op::I64TruncSatF64U => self.map_f64_i64(convert::i64_trunc_sat_f64_u)?,
            Op::F32ConvertI32S => self.map_i32_f32(convert::f32_convert_i32_s)?,
            Op::F32ConvertI32U => self.map_i32_f32(convert::f32_convert_i32_u)?,
            Op::F32ConvertI64S => self.map_i64_f32(convert::f32_convert_i64_s)?,
            Op::F32ConvertI64U => self.map_i64_f32(convert::f32_convert_i64_u)?,
            Op::F32DemoteF64 => self.map_f64_f32(convert::f32_demote_f64)?,
            Op::F64ConvertI32S => self.map_i32_f64(convert::f64_convert_i32_s)?,
            Op::F64ConvertI32U => self.map_i32_f64(convert::f64_convert_i32_u)?,
            Op::F64ConvertI64S => self.map_i64_f64(convert::f64_convert_i64_s)?,
            Op::F64ConvertI64U => self.map_i64_f64(convert::f64_convert_i64_u)?,
            Op::F64PromoteF32 => self.map_f32_f64(convert::f64_promote_f32)?,
            Op::I32ReinterpretF32 => self.map_f32_i32(|x| x.to_bits() as i32)?,
            Op::I64ReinterpretF64 => self.map_f64_i64(|x| x.to_bits() as i64)?,
            Op::F32ReinterpretI32 => {
                let x = self.pop_i32()?;
                self.push_f32_bits(x as u32);
            }
            Op::F64ReinterpretI64 => {
                let x = self.pop_i64()?;
                self.push_f64_bits(x as u64);
            }

            // --- sign-extension ---
            Op::I32Extend8S => self.i32_unop(|a| i32::from(a as i8))?,
            Op::I32Extend16S => self.i32_unop(|a| i32::from(a as i16))?,
            Op::I64Extend8S => self.i64_unop(|a| i64::from(a as i8))?,
            Op::I64Extend16S => self.i64_unop(|a| i64::from(a as i16))?,
            Op::I64Extend32S => self.i64_unop(|a| i64::from(a as i32))?,
            // The colder GC/cast categories fall through: exec_gc → exec_gc_array → exec_cast.
            other => self.exec_gc(inner, other, instance)?,
        }
        Ok(next)
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
