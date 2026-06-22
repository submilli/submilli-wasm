//! Relaxed SIMD (#38). Each op has implementation-defined results; we pick one fixed deterministic
//! lowering that reuses the #37 machinery and lands inside the spec's `(either …)` allowed set:
//! swizzle/trunc/q15 = their non-relaxed counterparts, min/max = the canonical-NaN `f*_min/max`,
//! laneselect = bitwise `(a&m)|(b&~m)`, madd/nmadd = fused `mul_add`, dots = signed-`i8` wrapping.

use super::lanes::{
    f32x4, f64x2, from_f32x4, from_f64x2, from_i16x8, from_i32x4, from_u8x16, i16x8, i32x4, i8x16,
    u8x16,
};
use super::Execution;
use crate::exec::arith::{canon_f32, canon_f64, f32_max, f32_min, f64_max, f64_min};
use crate::exec::convert::{
    i32_trunc_sat_f32_s, i32_trunc_sat_f32_u, i32_trunc_sat_f64_s, i32_trunc_sat_f64_u,
};
use crate::module::op_simd::SimdOp;
use crate::{Error, Result};

impl Execution {
    #[allow(clippy::too_many_lines, clippy::many_single_char_names)]
    pub(super) fn exec_simd_relaxed(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            // swizzle: index ≥ 16 → 0 (the ARM / deterministic choice).
            S::I8x16RelaxedSwizzle => {
                let sel = u8x16(self.pop_v128());
                let a = u8x16(self.pop_v128());
                let r: [u8; 16] = core::array::from_fn(|i| {
                    let j = sel[i] as usize;
                    if j < 16 {
                        a[j]
                    } else {
                        0
                    }
                });
                self.push_v128(from_u8x16(r));
            }

            // truncation = saturating (NaN → 0).
            S::I32x4RelaxedTruncF32x4S => self.v_unop(f32x4, from_i32x4, i32_trunc_sat_f32_s),
            S::I32x4RelaxedTruncF32x4U => self.v_unop(f32x4, from_i32x4, i32_trunc_sat_f32_u),
            S::I32x4RelaxedTruncF64x2SZero => {
                let l = f64x2(self.pop_v128());
                self.push_v128(from_i32x4([
                    i32_trunc_sat_f64_s(l[0]),
                    i32_trunc_sat_f64_s(l[1]),
                    0,
                    0,
                ]));
            }
            S::I32x4RelaxedTruncF64x2UZero => {
                let l = f64x2(self.pop_v128());
                self.push_v128(from_i32x4([
                    i32_trunc_sat_f64_u(l[0]),
                    i32_trunc_sat_f64_u(l[1]),
                    0,
                    0,
                ]));
            }

            // min/max = the canonical-NaN / ±0 wasm semantics.
            S::F32x4RelaxedMin => self.v_binop(f32x4, from_f32x4, f32_min),
            S::F32x4RelaxedMax => self.v_binop(f32x4, from_f32x4, f32_max),
            S::F64x2RelaxedMin => self.v_binop(f64x2, from_f64x2, f64_min),
            S::F64x2RelaxedMax => self.v_binop(f64x2, from_f64x2, f64_max),

            // laneselect (all widths identical): bitwise blend, control `m` on top of stack.
            S::I8x16RelaxedLaneselect
            | S::I16x8RelaxedLaneselect
            | S::I32x4RelaxedLaneselect
            | S::I64x2RelaxedLaneselect => {
                let m = self.pop_v128();
                let b = self.pop_v128();
                let a = self.pop_v128();
                self.push_v128((a & m) | (b & !m));
            }

            // fused multiply-add (canonicalized).
            S::F32x4RelaxedMadd => self.fma_f32x4(f32::mul_add),
            S::F32x4RelaxedNmadd => self.fma_f32x4(|a, b, c| (-a).mul_add(b, c)),
            S::F64x2RelaxedMadd => self.fma_f64x2(f64::mul_add),
            S::F64x2RelaxedNmadd => self.fma_f64x2(|a, b, c| (-a).mul_add(b, c)),

            // q15mulr = saturating.
            S::I16x8RelaxedQ15mulrS => self.v_binop(i16x8, from_i16x8, |a, b| {
                (((i32::from(a) * i32::from(b)) + 0x4000) >> 15).clamp(-32768, 32767) as i16
            }),

            // i8×i7 dot products (second operand read as signed i8; sums wrap).
            S::I16x8RelaxedDotI8x16I7x16S => {
                let b = i8x16(self.pop_v128());
                let a = i8x16(self.pop_v128());
                self.push_v128(from_i16x8(relaxed_dot(&a, &b)));
            }
            S::I32x4RelaxedDotI8x16I7x16AddS => {
                let c = i32x4(self.pop_v128());
                let b = i8x16(self.pop_v128());
                let a = i8x16(self.pop_v128());
                let dot = relaxed_dot(&a, &b);
                self.push_v128(from_i32x4(core::array::from_fn(|j| {
                    i32::from(dot[2 * j])
                        .wrapping_add(i32::from(dot[2 * j + 1]))
                        .wrapping_add(c[j])
                })));
            }

            _ => return Err(Error::msg(format!("unimplemented simd op: {s:?}"))),
        }
        Ok(())
    }

    #[allow(clippy::many_single_char_names)]
    fn fma_f32x4(&mut self, f: impl Fn(f32, f32, f32) -> f32) {
        let c = f32x4(self.pop_v128());
        let b = f32x4(self.pop_v128());
        let a = f32x4(self.pop_v128());
        let r: [f32; 4] = core::array::from_fn(|i| {
            canon_f32(
                f(a[i], b[i], c[i]),
                a[i].is_nan() || b[i].is_nan() || c[i].is_nan(),
            )
        });
        self.push_v128(from_f32x4(r));
    }

    #[allow(clippy::many_single_char_names)]
    fn fma_f64x2(&mut self, f: impl Fn(f64, f64, f64) -> f64) {
        let c = f64x2(self.pop_v128());
        let b = f64x2(self.pop_v128());
        let a = f64x2(self.pop_v128());
        let r: [f64; 2] = core::array::from_fn(|i| {
            canon_f64(
                f(a[i], b[i], c[i]),
                a[i].is_nan() || b[i].is_nan() || c[i].is_nan(),
            )
        });
        self.push_v128(from_f64x2(r));
    }
}

/// The i16x8 relaxed dot: lane `i` = `a[2i]·b[2i] + a[2i+1]·b[2i+1]` (signed `i8`; products fit
/// i16, the pair sum wraps).
#[inline]
fn relaxed_dot(a: &[i8; 16], b: &[i8; 16]) -> [i16; 8] {
    core::array::from_fn(|i| {
        (i16::from(a[2 * i]) * i16::from(b[2 * i]))
            .wrapping_add(i16::from(a[2 * i + 1]) * i16::from(b[2 * i + 1]))
    })
}
