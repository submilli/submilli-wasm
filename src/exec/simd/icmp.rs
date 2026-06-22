//! Integer lanewise comparisons (→ all-ones/zero lane masks) and shifts (count masked to the lane
//! width) (#37).

use super::lanes::{
    from_i16x8, from_i32x4, from_i64x2, from_i8x16, from_u16x8, from_u32x4, from_u64x2, from_u8x16,
    i16x8, i32x4, i64x2, i8x16, u16x8, u32x4, u64x2, u8x16,
};
use super::Execution;
use crate::module::op_simd::SimdOp;
use crate::Result;

impl Execution {
    #[allow(clippy::too_many_lines)] // flat lanewise dispatch; arms are one-liners
    pub(super) fn exec_simd_icmp(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            // i8x16 comparisons
            S::I8x16Eq => self.v_cmp(i8x16, |a, b| a == b),
            S::I8x16Ne => self.v_cmp(i8x16, |a, b| a != b),
            S::I8x16LtS => self.v_cmp(i8x16, |a, b| a < b),
            S::I8x16LtU => self.v_cmp(u8x16, |a, b| a < b),
            S::I8x16GtS => self.v_cmp(i8x16, |a, b| a > b),
            S::I8x16GtU => self.v_cmp(u8x16, |a, b| a > b),
            S::I8x16LeS => self.v_cmp(i8x16, |a, b| a <= b),
            S::I8x16LeU => self.v_cmp(u8x16, |a, b| a <= b),
            S::I8x16GeS => self.v_cmp(i8x16, |a, b| a >= b),
            S::I8x16GeU => self.v_cmp(u8x16, |a, b| a >= b),

            // i16x8 comparisons
            S::I16x8Eq => self.v_cmp(i16x8, |a, b| a == b),
            S::I16x8Ne => self.v_cmp(i16x8, |a, b| a != b),
            S::I16x8LtS => self.v_cmp(i16x8, |a, b| a < b),
            S::I16x8LtU => self.v_cmp(u16x8, |a, b| a < b),
            S::I16x8GtS => self.v_cmp(i16x8, |a, b| a > b),
            S::I16x8GtU => self.v_cmp(u16x8, |a, b| a > b),
            S::I16x8LeS => self.v_cmp(i16x8, |a, b| a <= b),
            S::I16x8LeU => self.v_cmp(u16x8, |a, b| a <= b),
            S::I16x8GeS => self.v_cmp(i16x8, |a, b| a >= b),
            S::I16x8GeU => self.v_cmp(u16x8, |a, b| a >= b),

            // i32x4 comparisons
            S::I32x4Eq => self.v_cmp(i32x4, |a, b| a == b),
            S::I32x4Ne => self.v_cmp(i32x4, |a, b| a != b),
            S::I32x4LtS => self.v_cmp(i32x4, |a, b| a < b),
            S::I32x4LtU => self.v_cmp(u32x4, |a, b| a < b),
            S::I32x4GtS => self.v_cmp(i32x4, |a, b| a > b),
            S::I32x4GtU => self.v_cmp(u32x4, |a, b| a > b),
            S::I32x4LeS => self.v_cmp(i32x4, |a, b| a <= b),
            S::I32x4LeU => self.v_cmp(u32x4, |a, b| a <= b),
            S::I32x4GeS => self.v_cmp(i32x4, |a, b| a >= b),
            S::I32x4GeU => self.v_cmp(u32x4, |a, b| a >= b),

            // i64x2 comparisons (signed only)
            S::I64x2Eq => self.v_cmp(i64x2, |a, b| a == b),
            S::I64x2Ne => self.v_cmp(i64x2, |a, b| a != b),
            S::I64x2LtS => self.v_cmp(i64x2, |a, b| a < b),
            S::I64x2GtS => self.v_cmp(i64x2, |a, b| a > b),
            S::I64x2LeS => self.v_cmp(i64x2, |a, b| a <= b),
            S::I64x2GeS => self.v_cmp(i64x2, |a, b| a >= b),

            // shifts (count masked to lane width in v_shift)
            S::I8x16Shl => self.v_shift(i8x16, from_i8x16, i8::wrapping_shl),
            S::I8x16ShrS => self.v_shift(i8x16, from_i8x16, i8::wrapping_shr),
            S::I8x16ShrU => self.v_shift(u8x16, from_u8x16, u8::wrapping_shr),
            S::I16x8Shl => self.v_shift(i16x8, from_i16x8, i16::wrapping_shl),
            S::I16x8ShrS => self.v_shift(i16x8, from_i16x8, i16::wrapping_shr),
            S::I16x8ShrU => self.v_shift(u16x8, from_u16x8, u16::wrapping_shr),
            S::I32x4Shl => self.v_shift(i32x4, from_i32x4, i32::wrapping_shl),
            S::I32x4ShrS => self.v_shift(i32x4, from_i32x4, i32::wrapping_shr),
            S::I32x4ShrU => self.v_shift(u32x4, from_u32x4, u32::wrapping_shr),
            S::I64x2Shl => self.v_shift(i64x2, from_i64x2, i64::wrapping_shl),
            S::I64x2ShrS => self.v_shift(i64x2, from_i64x2, i64::wrapping_shr),
            S::I64x2ShrU => self.v_shift(u64x2, from_u64x2, u64::wrapping_shr),

            _ => return self.exec_simd_iarith2(s),
        }
        Ok(())
    }
}
