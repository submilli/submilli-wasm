//! Numeric/memory opcode dispatch. Arithmetic helpers live in [`super::arith`],
//! conversions in [`super::convert`], memory ops in [`super::memory`].

// `f*.eq`/`f*.ne` are exact IEEE comparisons by definition.
#![allow(clippy::float_cmp)]

use super::{arith, convert, Execution};
use crate::instance::Instance;
use crate::module::op::Op;
use crate::store::StoreInner;
use crate::value::Val;
use crate::{Error, Result};

impl Execution {
    #[allow(clippy::too_many_lines)] // flat opcode dispatch; arms are one-liners
    pub(super) fn exec_numeric(
        &mut self,
        inner: &mut StoreInner,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        use Op as O;
        match op {
            // --- i32 ---
            O::I32Eqz => self.i32_unop(|a| i32::from(a == 0)),
            O::I32Eq => self.i32_relop(|a, b| a == b),
            O::I32Ne => self.i32_relop(|a, b| a != b),
            O::I32LtS => self.i32_relop(|a, b| a < b),
            O::I32LtU => self.u32_relop(|a, b| a < b),
            O::I32GtS => self.i32_relop(|a, b| a > b),
            O::I32GtU => self.u32_relop(|a, b| a > b),
            O::I32LeS => self.i32_relop(|a, b| a <= b),
            O::I32LeU => self.u32_relop(|a, b| a <= b),
            O::I32GeS => self.i32_relop(|a, b| a >= b),
            O::I32GeU => self.u32_relop(|a, b| a >= b),
            O::I32Clz => self.i32_unop(|a| a.leading_zeros() as i32),
            O::I32Ctz => self.i32_unop(|a| a.trailing_zeros() as i32),
            O::I32Popcnt => self.i32_unop(|a| a.count_ones() as i32),
            O::I32Add => self.i32_binop(i32::wrapping_add),
            O::I32Sub => self.i32_binop(i32::wrapping_sub),
            O::I32Mul => self.i32_binop(i32::wrapping_mul),
            O::I32DivS => self.i32_try_binop(arith::div_s_i32)?,
            O::I32DivU => self.u32_try_binop(|a, b| arith::nz(b).map(|()| a / b))?,
            O::I32RemS => self.i32_try_binop(|a, b| arith::nz(b).map(|()| a.wrapping_rem(b)))?,
            O::I32RemU => self.u32_try_binop(|a, b| arith::nz(b).map(|()| a % b))?,
            O::I32And => self.i32_binop(|a, b| a & b),
            O::I32Or => self.i32_binop(|a, b| a | b),
            O::I32Xor => self.i32_binop(|a, b| a ^ b),
            O::I32Shl => self.i32_binop(|a, b| a.wrapping_shl(b as u32)),
            O::I32ShrS => self.i32_binop(|a, b| a.wrapping_shr(b as u32)),
            O::I32ShrU => self.u32_binop(u32::wrapping_shr),
            O::I32Rotl => self.u32_binop(|a, b| a.rotate_left(b % 32)),
            O::I32Rotr => self.u32_binop(|a, b| a.rotate_right(b % 32)),

            // --- i64 ---
            O::I64Eqz => {
                let a = self.pop().unwrap_i64();
                self.push(Val::I32(i32::from(a == 0)));
            }
            O::I64Eq => self.i64_relop(|a, b| a == b),
            O::I64Ne => self.i64_relop(|a, b| a != b),
            O::I64LtS => self.i64_relop(|a, b| a < b),
            O::I64LtU => self.u64_relop(|a, b| a < b),
            O::I64GtS => self.i64_relop(|a, b| a > b),
            O::I64GtU => self.u64_relop(|a, b| a > b),
            O::I64LeS => self.i64_relop(|a, b| a <= b),
            O::I64LeU => self.u64_relop(|a, b| a <= b),
            O::I64GeS => self.i64_relop(|a, b| a >= b),
            O::I64GeU => self.u64_relop(|a, b| a >= b),
            O::I64Clz => self.i64_unop(|a| i64::from(a.leading_zeros())),
            O::I64Ctz => self.i64_unop(|a| i64::from(a.trailing_zeros())),
            O::I64Popcnt => self.i64_unop(|a| i64::from(a.count_ones())),
            O::I64Add => self.i64_binop(i64::wrapping_add),
            O::I64Sub => self.i64_binop(i64::wrapping_sub),
            O::I64Mul => self.i64_binop(i64::wrapping_mul),
            O::I64DivS => self.i64_try_binop(arith::div_s_i64)?,
            O::I64DivU => self.u64_try_binop(|a, b| arith::nz(b).map(|()| a / b))?,
            O::I64RemS => self.i64_try_binop(|a, b| arith::nz(b).map(|()| a.wrapping_rem(b)))?,
            O::I64RemU => self.u64_try_binop(|a, b| arith::nz(b).map(|()| a % b))?,
            O::I64And => self.i64_binop(|a, b| a & b),
            O::I64Or => self.i64_binop(|a, b| a | b),
            O::I64Xor => self.i64_binop(|a, b| a ^ b),
            O::I64Shl => self.i64_binop(|a, b| a.wrapping_shl(b as u32)),
            O::I64ShrS => self.i64_binop(|a, b| a.wrapping_shr(b as u32)),
            O::I64ShrU => self.u64_binop(|a, b| a.wrapping_shr(b as u32)),
            O::I64Rotl => self.u64_binop(|a, b| a.rotate_left((b % 64) as u32)),
            O::I64Rotr => self.u64_binop(|a, b| a.rotate_right((b % 64) as u32)),

            // --- f32 ---
            O::F32Eq => self.f32_relop(|a, b| a == b),
            O::F32Ne => self.f32_relop(|a, b| a != b),
            O::F32Lt => self.f32_relop(|a, b| a < b),
            O::F32Gt => self.f32_relop(|a, b| a > b),
            O::F32Le => self.f32_relop(|a, b| a <= b),
            O::F32Ge => self.f32_relop(|a, b| a >= b),
            O::F32Abs => self.f32_unop(f32::abs),
            O::F32Neg => self.f32_unop(|a| -a),
            O::F32Ceil => self.f32_unop(f32::ceil),
            O::F32Floor => self.f32_unop(f32::floor),
            O::F32Trunc => self.f32_unop(f32::trunc),
            O::F32Nearest => self.f32_unop(f32::round_ties_even),
            O::F32Sqrt => self.f32_unop_canon(f32::sqrt),
            O::F32Add => self.f32_arith(|a, b| a + b),
            O::F32Sub => self.f32_arith(|a, b| a - b),
            O::F32Mul => self.f32_arith(|a, b| a * b),
            O::F32Div => self.f32_arith(|a, b| a / b),
            O::F32Min => self.f32_binop(arith::f32_min),
            O::F32Max => self.f32_binop(arith::f32_max),
            O::F32Copysign => self.f32_binop(f32::copysign),

            // --- f64 ---
            O::F64Eq => self.f64_relop(|a, b| a == b),
            O::F64Ne => self.f64_relop(|a, b| a != b),
            O::F64Lt => self.f64_relop(|a, b| a < b),
            O::F64Gt => self.f64_relop(|a, b| a > b),
            O::F64Le => self.f64_relop(|a, b| a <= b),
            O::F64Ge => self.f64_relop(|a, b| a >= b),
            O::F64Abs => self.f64_unop(f64::abs),
            O::F64Neg => self.f64_unop(|a| -a),
            O::F64Ceil => self.f64_unop(f64::ceil),
            O::F64Floor => self.f64_unop(f64::floor),
            O::F64Trunc => self.f64_unop(f64::trunc),
            O::F64Nearest => self.f64_unop(f64::round_ties_even),
            O::F64Sqrt => self.f64_unop_canon(f64::sqrt),
            O::F64Add => self.f64_arith(|a, b| a + b),
            O::F64Sub => self.f64_arith(|a, b| a - b),
            O::F64Mul => self.f64_arith(|a, b| a * b),
            O::F64Div => self.f64_arith(|a, b| a / b),
            O::F64Min => self.f64_binop(arith::f64_min),
            O::F64Max => self.f64_binop(arith::f64_max),
            O::F64Copysign => self.f64_binop(f64::copysign),

            // --- conversions ---
            O::I32WrapI64 => {
                let x = self.pop().unwrap_i64();
                self.push(Val::I32(convert::i32_wrap_i64(x)));
            }
            O::I64ExtendI32S => {
                let x = self.pop().unwrap_i32();
                self.push(Val::I64(convert::i64_extend_i32_s(x)));
            }
            O::I64ExtendI32U => {
                let x = self.pop().unwrap_i32();
                self.push(Val::I64(convert::i64_extend_i32_u(x)));
            }
            O::I32TruncF32S => self.trunc_to_i32(convert::i32_trunc_f32_s)?,
            O::I32TruncF32U => self.trunc_to_i32(convert::i32_trunc_f32_u)?,
            O::I32TruncF64S => self.trunc64_to_i32(convert::i32_trunc_f64_s)?,
            O::I32TruncF64U => self.trunc64_to_i32(convert::i32_trunc_f64_u)?,
            O::I64TruncF32S => self.trunc_to_i64(convert::i64_trunc_f32_s)?,
            O::I64TruncF32U => self.trunc_to_i64(convert::i64_trunc_f32_u)?,
            O::I64TruncF64S => self.trunc64_to_i64(convert::i64_trunc_f64_s)?,
            O::I64TruncF64U => self.trunc64_to_i64(convert::i64_trunc_f64_u)?,
            O::I32TruncSatF32S => self.map_f32_i32(convert::i32_trunc_sat_f32_s),
            O::I32TruncSatF32U => self.map_f32_i32(convert::i32_trunc_sat_f32_u),
            O::I32TruncSatF64S => self.map_f64_i32(convert::i32_trunc_sat_f64_s),
            O::I32TruncSatF64U => self.map_f64_i32(convert::i32_trunc_sat_f64_u),
            O::I64TruncSatF32S => self.map_f32_i64(convert::i64_trunc_sat_f32_s),
            O::I64TruncSatF32U => self.map_f32_i64(convert::i64_trunc_sat_f32_u),
            O::I64TruncSatF64S => self.map_f64_i64(convert::i64_trunc_sat_f64_s),
            O::I64TruncSatF64U => self.map_f64_i64(convert::i64_trunc_sat_f64_u),
            O::F32ConvertI32S => self.map_i32_f32(convert::f32_convert_i32_s),
            O::F32ConvertI32U => self.map_i32_f32(convert::f32_convert_i32_u),
            O::F32ConvertI64S => self.map_i64_f32(convert::f32_convert_i64_s),
            O::F32ConvertI64U => self.map_i64_f32(convert::f32_convert_i64_u),
            O::F32DemoteF64 => self.map_f64_f32(convert::f32_demote_f64),
            O::F64ConvertI32S => self.map_i32_f64(convert::f64_convert_i32_s),
            O::F64ConvertI32U => self.map_i32_f64(convert::f64_convert_i32_u),
            O::F64ConvertI64S => self.map_i64_f64(convert::f64_convert_i64_s),
            O::F64ConvertI64U => self.map_i64_f64(convert::f64_convert_i64_u),
            O::F64PromoteF32 => self.map_f32_f64(convert::f64_promote_f32),
            O::I32ReinterpretF32 => self.map_f32_i32(|x| x.to_bits() as i32),
            O::I64ReinterpretF64 => self.map_f64_i64(|x| x.to_bits() as i64),
            O::F32ReinterpretI32 => {
                let x = self.pop().unwrap_i32();
                self.push(Val::F32(x as u32));
            }
            O::F64ReinterpretI64 => {
                let x = self.pop().unwrap_i64();
                self.push(Val::F64(x as u64));
            }

            // --- sign-extension ---
            O::I32Extend8S => self.i32_unop(|a| i32::from(a as i8)),
            O::I32Extend16S => self.i32_unop(|a| i32::from(a as i16)),
            O::I64Extend8S => self.i64_unop(|a| i64::from(a as i8)),
            O::I64Extend16S => self.i64_unop(|a| i64::from(a as i16)),
            O::I64Extend32S => self.i64_unop(|a| i64::from(a as i32)),

            // --- memory ---
            O::I32Load(_)
            | O::I64Load(_)
            | O::F32Load(_)
            | O::F64Load(_)
            | O::I32Load8S(_)
            | O::I32Load8U(_)
            | O::I32Load16S(_)
            | O::I32Load16U(_)
            | O::I64Load8S(_)
            | O::I64Load8U(_)
            | O::I64Load16S(_)
            | O::I64Load16U(_)
            | O::I64Load32S(_)
            | O::I64Load32U(_)
            | O::I32Store(_)
            | O::I64Store(_)
            | O::F32Store(_)
            | O::F64Store(_)
            | O::I32Store8(_)
            | O::I32Store16(_)
            | O::I64Store8(_)
            | O::I64Store16(_)
            | O::I64Store32(_)
            | O::MemorySize
            | O::MemoryGrow
            | O::MemoryCopy
            | O::MemoryFill
            | O::MemoryInit(_)
            | O::DataDrop(_) => return self.exec_memory(inner, op, instance),

            O::TableInit { .. } | O::TableCopy { .. } | O::ElemDrop(_) => {
                return self.exec_table(inner, op, instance)
            }

            other => {
                return Err(Error::msg(format!(
                    "unexpected op in exec_numeric: {other:?}"
                )))
            }
        }
        Ok(())
    }

    // Conversion plumbing (pop one operand, push the converted result).
    fn trunc_to_i32(&mut self, f: impl Fn(f32) -> Result<i32>) -> Result<()> {
        let x = self.pop().unwrap_f32();
        self.push(Val::I32(f(x)?));
        Ok(())
    }
    fn trunc64_to_i32(&mut self, f: impl Fn(f64) -> Result<i32>) -> Result<()> {
        let x = self.pop().unwrap_f64();
        self.push(Val::I32(f(x)?));
        Ok(())
    }
    fn trunc_to_i64(&mut self, f: impl Fn(f32) -> Result<i64>) -> Result<()> {
        let x = self.pop().unwrap_f32();
        self.push(Val::I64(f(x)?));
        Ok(())
    }
    fn trunc64_to_i64(&mut self, f: impl Fn(f64) -> Result<i64>) -> Result<()> {
        let x = self.pop().unwrap_f64();
        self.push(Val::I64(f(x)?));
        Ok(())
    }
    fn map_f32_i32(&mut self, f: impl Fn(f32) -> i32) {
        let x = self.pop().unwrap_f32();
        self.push(Val::I32(f(x)));
    }
    fn map_f64_i32(&mut self, f: impl Fn(f64) -> i32) {
        let x = self.pop().unwrap_f64();
        self.push(Val::I32(f(x)));
    }
    fn map_f32_i64(&mut self, f: impl Fn(f32) -> i64) {
        let x = self.pop().unwrap_f32();
        self.push(Val::I64(f(x)));
    }
    fn map_f64_i64(&mut self, f: impl Fn(f64) -> i64) {
        let x = self.pop().unwrap_f64();
        self.push(Val::I64(f(x)));
    }
    fn map_i32_f32(&mut self, f: impl Fn(i32) -> f32) {
        let x = self.pop().unwrap_i32();
        self.push(Val::F32(f(x).to_bits()));
    }
    fn map_i64_f32(&mut self, f: impl Fn(i64) -> f32) {
        let x = self.pop().unwrap_i64();
        self.push(Val::F32(f(x).to_bits()));
    }
    fn map_f64_f32(&mut self, f: impl Fn(f64) -> f32) {
        let x = self.pop().unwrap_f64();
        self.push(Val::F32(f(x).to_bits()));
    }
    fn map_i32_f64(&mut self, f: impl Fn(i32) -> f64) {
        let x = self.pop().unwrap_i32();
        self.push(Val::F64(f(x).to_bits()));
    }
    fn map_i64_f64(&mut self, f: impl Fn(i64) -> f64) {
        let x = self.pop().unwrap_i64();
        self.push(Val::F64(f(x).to_bits()));
    }
    fn map_f32_f64(&mut self, f: impl Fn(f32) -> f64) {
        let x = self.pop().unwrap_f32();
        self.push(Val::F64(f(x).to_bits()));
    }
}
