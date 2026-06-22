//! Saturating + widening/narrowing integer ops (#37): add/sub-sat, q15mulr-sat, narrow, extend
//! low/high, ext-add-pairwise, ext-mul low/high, and the i32x4 dot product. The widen/narrow
//! helpers are generic over the input/output lane counts (`NI`/`NO`).

use super::lanes::{
    from_i16x8, from_i32x4, from_i64x2, from_i8x16, from_u16x8, from_u32x4, from_u64x2, from_u8x16,
    i16x8, i32x4, i8x16, u16x8, u32x4, u8x16,
};
use super::Execution;
use crate::module::op_simd::SimdOp;
use crate::Result;

impl Execution {
    #[allow(clippy::too_many_lines, clippy::semicolon_if_nothing_returned)]
    pub(super) fn exec_simd_iarith2(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            // saturating add/sub
            S::I8x16AddSatS => self.v_binop(i8x16, from_i8x16, i8::saturating_add),
            S::I8x16AddSatU => self.v_binop(u8x16, from_u8x16, u8::saturating_add),
            S::I8x16SubSatS => self.v_binop(i8x16, from_i8x16, i8::saturating_sub),
            S::I8x16SubSatU => self.v_binop(u8x16, from_u8x16, u8::saturating_sub),
            S::I16x8AddSatS => self.v_binop(i16x8, from_i16x8, i16::saturating_add),
            S::I16x8AddSatU => self.v_binop(u16x8, from_u16x8, u16::saturating_add),
            S::I16x8SubSatS => self.v_binop(i16x8, from_i16x8, i16::saturating_sub),
            S::I16x8SubSatU => self.v_binop(u16x8, from_u16x8, u16::saturating_sub),
            S::I16x8Q15MulrSatS => self.v_binop(i16x8, from_i16x8, |a, b| {
                (((i32::from(a) * i32::from(b)) + 0x4000) >> 15).clamp(-32768, 32767) as i16
            }),

            // narrow (signed source; signed- or unsigned-saturated)
            S::I8x16NarrowI16x8S => self.v_narrow(i16x8, from_i8x16, |x| x.clamp(-128, 127) as i8),
            S::I8x16NarrowI16x8U => self.v_narrow(i16x8, from_u8x16, |x| x.clamp(0, 255) as u8),
            S::I16x8NarrowI32x4S => {
                self.v_narrow(i32x4, from_i16x8, |x| x.clamp(-32768, 32767) as i16);
            }
            S::I16x8NarrowI32x4U => {
                self.v_narrow(i32x4, from_u16x8, |x| x.clamp(0, 65535) as u16);
            }

            // extend low/high
            S::I16x8ExtendLowI8x16S => self.v_extend(i8x16, from_i16x8, false, i16::from),
            S::I16x8ExtendHighI8x16S => self.v_extend(i8x16, from_i16x8, true, i16::from),
            S::I16x8ExtendLowI8x16U => self.v_extend(u8x16, from_u16x8, false, u16::from),
            S::I16x8ExtendHighI8x16U => self.v_extend(u8x16, from_u16x8, true, u16::from),
            S::I32x4ExtendLowI16x8S => self.v_extend(i16x8, from_i32x4, false, i32::from),
            S::I32x4ExtendHighI16x8S => self.v_extend(i16x8, from_i32x4, true, i32::from),
            S::I32x4ExtendLowI16x8U => self.v_extend(u16x8, from_u32x4, false, u32::from),
            S::I32x4ExtendHighI16x8U => self.v_extend(u16x8, from_u32x4, true, u32::from),
            S::I64x2ExtendLowI32x4S => self.v_extend(i32x4, from_i64x2, false, i64::from),
            S::I64x2ExtendHighI32x4S => self.v_extend(i32x4, from_i64x2, true, i64::from),
            S::I64x2ExtendLowI32x4U => self.v_extend(u32x4, from_u64x2, false, u64::from),
            S::I64x2ExtendHighI32x4U => self.v_extend(u32x4, from_u64x2, true, u64::from),

            // ext-add pairwise
            S::I16x8ExtAddPairwiseI8x16S => {
                self.v_extadd(i8x16, from_i16x8, |a, b| i16::from(a) + i16::from(b));
            }
            S::I16x8ExtAddPairwiseI8x16U => {
                self.v_extadd(u8x16, from_u16x8, |a, b| u16::from(a) + u16::from(b));
            }
            S::I32x4ExtAddPairwiseI16x8S => {
                self.v_extadd(i16x8, from_i32x4, |a, b| i32::from(a) + i32::from(b));
            }
            S::I32x4ExtAddPairwiseI16x8U => {
                self.v_extadd(u16x8, from_u32x4, |a, b| u32::from(a) + u32::from(b));
            }

            // ext-mul low/high
            S::I16x8ExtMulLowI8x16S => {
                self.v_extmul(i8x16, from_i16x8, false, |a, b| i16::from(a) * i16::from(b))
            }
            S::I16x8ExtMulHighI8x16S => {
                self.v_extmul(i8x16, from_i16x8, true, |a, b| i16::from(a) * i16::from(b))
            }
            S::I16x8ExtMulLowI8x16U => {
                self.v_extmul(u8x16, from_u16x8, false, |a, b| u16::from(a) * u16::from(b))
            }
            S::I16x8ExtMulHighI8x16U => {
                self.v_extmul(u8x16, from_u16x8, true, |a, b| u16::from(a) * u16::from(b))
            }
            S::I32x4ExtMulLowI16x8S => {
                self.v_extmul(i16x8, from_i32x4, false, |a, b| i32::from(a) * i32::from(b))
            }
            S::I32x4ExtMulHighI16x8S => {
                self.v_extmul(i16x8, from_i32x4, true, |a, b| i32::from(a) * i32::from(b))
            }
            S::I32x4ExtMulLowI16x8U => {
                self.v_extmul(u16x8, from_u32x4, false, |a, b| u32::from(a) * u32::from(b))
            }
            S::I32x4ExtMulHighI16x8U => {
                self.v_extmul(u16x8, from_u32x4, true, |a, b| u32::from(a) * u32::from(b))
            }
            S::I64x2ExtMulLowI32x4S => {
                self.v_extmul(i32x4, from_i64x2, false, |a, b| i64::from(a) * i64::from(b))
            }
            S::I64x2ExtMulHighI32x4S => {
                self.v_extmul(i32x4, from_i64x2, true, |a, b| i64::from(a) * i64::from(b))
            }
            S::I64x2ExtMulLowI32x4U => {
                self.v_extmul(u32x4, from_u64x2, false, |a, b| u64::from(a) * u64::from(b))
            }
            S::I64x2ExtMulHighI32x4U => {
                self.v_extmul(u32x4, from_u64x2, true, |a, b| u64::from(a) * u64::from(b))
            }

            S::I32x4DotI16x8S => {
                let b = i16x8(self.pop_v128());
                let a = i16x8(self.pop_v128());
                self.push_v128(from_i32x4(core::array::from_fn(|i| {
                    (i32::from(a[2 * i]) * i32::from(b[2 * i]))
                        .wrapping_add(i32::from(a[2 * i + 1]) * i32::from(b[2 * i + 1]))
                })));
            }

            _ => return self.exec_simd_farith(s),
        }
        Ok(())
    }

    /// Widen `NO` input lanes (low or high half of `NI = 2·NO`) to `NO` output lanes via `f`.
    fn v_extend<I: Copy, O, const NI: usize, const NO: usize>(
        &mut self,
        split: fn(u128) -> [I; NI],
        join: fn([O; NO]) -> u128,
        high: bool,
        f: impl Fn(I) -> O,
    ) {
        let a = split(self.pop_v128());
        let off = if high { NO } else { 0 };
        self.push_v128(join(core::array::from_fn(|i| f(a[off + i]))));
    }

    /// Multiply the low/high `NO` lanes of two operands into `NO` wider output lanes.
    fn v_extmul<I: Copy, O, const NI: usize, const NO: usize>(
        &mut self,
        split: fn(u128) -> [I; NI],
        join: fn([O; NO]) -> u128,
        high: bool,
        f: impl Fn(I, I) -> O,
    ) {
        let b = split(self.pop_v128());
        let a = split(self.pop_v128());
        let off = if high { NO } else { 0 };
        self.push_v128(join(core::array::from_fn(|i| f(a[off + i], b[off + i]))));
    }

    /// Sum adjacent lane pairs, widening: output lane `i` = `f(in[2i], in[2i+1])`.
    fn v_extadd<I: Copy, O, const NI: usize, const NO: usize>(
        &mut self,
        split: fn(u128) -> [I; NI],
        join: fn([O; NO]) -> u128,
        f: impl Fn(I, I) -> O,
    ) {
        let a = split(self.pop_v128());
        self.push_v128(join(core::array::from_fn(|i| f(a[2 * i], a[2 * i + 1]))));
    }

    /// Narrow two operands' `NI` lanes each into `NO = 2·NI` output lanes (a then b) via `f`.
    fn v_narrow<I: Copy, O, const NI: usize, const NO: usize>(
        &mut self,
        split: fn(u128) -> [I; NI],
        join: fn([O; NO]) -> u128,
        f: impl Fn(I) -> O,
    ) {
        let b = split(self.pop_v128());
        let a = split(self.pop_v128());
        self.push_v128(join(core::array::from_fn(|i| {
            if i < NI {
                f(a[i])
            } else {
                f(b[i - NI])
            }
        })));
    }
}
