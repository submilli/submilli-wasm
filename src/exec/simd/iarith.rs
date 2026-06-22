//! Integer lanewise arithmetic (#37): add/sub/mul/neg/abs/min/max/avgr/popcnt, plus the
//! `all_true` / `bitmask` reductions. Wrapping semantics; no traps.

use super::lanes::{
    from_i16x8, from_i32x4, from_i64x2, from_i8x16, from_u16x8, from_u32x4, from_u8x16, i16x8,
    i32x4, i64x2, i8x16, u16x8, u32x4, u8x16,
};
use super::Execution;
use crate::module::op_simd::SimdOp;
use crate::value::Val;
use crate::Result;

impl Execution {
    #[allow(clippy::too_many_lines, clippy::semicolon_if_nothing_returned)]
    pub(super) fn exec_simd_iarith(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            // i8x16
            S::I8x16Add => self.v_binop(i8x16, from_i8x16, i8::wrapping_add),
            S::I8x16Sub => self.v_binop(i8x16, from_i8x16, i8::wrapping_sub),
            S::I8x16Neg => self.v_unop(i8x16, from_i8x16, i8::wrapping_neg),
            S::I8x16Abs => self.v_unop(i8x16, from_i8x16, i8::wrapping_abs),
            S::I8x16Popcnt => self.v_unop(u8x16, from_u8x16, |x| x.count_ones() as u8),
            S::I8x16MinS => self.v_binop(i8x16, from_i8x16, i8::min),
            S::I8x16MinU => self.v_binop(u8x16, from_u8x16, u8::min),
            S::I8x16MaxS => self.v_binop(i8x16, from_i8x16, i8::max),
            S::I8x16MaxU => self.v_binop(u8x16, from_u8x16, u8::max),
            S::I8x16AvgrU => self.v_binop(u8x16, from_u8x16, |a, b| avgr(a.into(), b.into()) as u8),
            S::I8x16AllTrue => self.v_all_true(i8x16),
            S::I8x16Bitmask => self.v_bitmask(i8x16),

            // i16x8
            S::I16x8Add => self.v_binop(i16x8, from_i16x8, i16::wrapping_add),
            S::I16x8Sub => self.v_binop(i16x8, from_i16x8, i16::wrapping_sub),
            S::I16x8Mul => self.v_binop(i16x8, from_i16x8, i16::wrapping_mul),
            S::I16x8Neg => self.v_unop(i16x8, from_i16x8, i16::wrapping_neg),
            S::I16x8Abs => self.v_unop(i16x8, from_i16x8, i16::wrapping_abs),
            S::I16x8MinS => self.v_binop(i16x8, from_i16x8, i16::min),
            S::I16x8MinU => self.v_binop(u16x8, from_u16x8, u16::min),
            S::I16x8MaxS => self.v_binop(i16x8, from_i16x8, i16::max),
            S::I16x8MaxU => self.v_binop(u16x8, from_u16x8, u16::max),
            S::I16x8AvgrU => {
                self.v_binop(u16x8, from_u16x8, |a, b| avgr(a.into(), b.into()) as u16)
            }
            S::I16x8AllTrue => self.v_all_true(i16x8),
            S::I16x8Bitmask => self.v_bitmask(i16x8),

            // i32x4
            S::I32x4Add => self.v_binop(i32x4, from_i32x4, i32::wrapping_add),
            S::I32x4Sub => self.v_binop(i32x4, from_i32x4, i32::wrapping_sub),
            S::I32x4Mul => self.v_binop(i32x4, from_i32x4, i32::wrapping_mul),
            S::I32x4Neg => self.v_unop(i32x4, from_i32x4, i32::wrapping_neg),
            S::I32x4Abs => self.v_unop(i32x4, from_i32x4, i32::wrapping_abs),
            S::I32x4MinS => self.v_binop(i32x4, from_i32x4, i32::min),
            S::I32x4MinU => self.v_binop(u32x4, from_u32x4, u32::min),
            S::I32x4MaxS => self.v_binop(i32x4, from_i32x4, i32::max),
            S::I32x4MaxU => self.v_binop(u32x4, from_u32x4, u32::max),
            S::I32x4AllTrue => self.v_all_true(i32x4),
            S::I32x4Bitmask => self.v_bitmask(i32x4),

            // i64x2
            S::I64x2Add => self.v_binop(i64x2, from_i64x2, i64::wrapping_add),
            S::I64x2Sub => self.v_binop(i64x2, from_i64x2, i64::wrapping_sub),
            S::I64x2Mul => self.v_binop(i64x2, from_i64x2, i64::wrapping_mul),
            S::I64x2Neg => self.v_unop(i64x2, from_i64x2, i64::wrapping_neg),
            S::I64x2Abs => self.v_unop(i64x2, from_i64x2, i64::wrapping_abs),
            S::I64x2AllTrue => self.v_all_true(i64x2),
            S::I64x2Bitmask => self.v_bitmask(i64x2),

            _ => return self.exec_simd_icmp(s),
        }
        Ok(())
    }

    /// `i32.from(lane < 0)` gathered into a bitmask, lane 0 → bit 0.
    fn v_bitmask<T: Copy + Default + PartialOrd, const N: usize>(
        &mut self,
        split: fn(u128) -> [T; N],
    ) {
        let l = split(self.pop_v128());
        let mut m = 0i32;
        for (i, &x) in l.iter().enumerate() {
            if x < T::default() {
                m |= 1 << i;
            }
        }
        self.push(Val::I32(m));
    }

    /// `1` if every lane is non-zero, else `0`.
    fn v_all_true<T: Copy + Default + PartialEq, const N: usize>(
        &mut self,
        split: fn(u128) -> [T; N],
    ) {
        let l = split(self.pop_v128());
        self.push(Val::I32(i32::from(l.iter().all(|&x| x != T::default()))));
    }
}

/// Unsigned rounding average in wider precision: `ceil((a + b) / 2)`.
#[inline]
fn avgr(a: u32, b: u32) -> u32 {
    (a + b).div_ceil(2)
}
