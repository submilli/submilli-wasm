//! Float↔int lane conversions (#37): saturating truncation (NaN→0, reusing the scalar `trunc_sat`),
//! int→float conversion, and the lane-count-changing demote/promote + `*_zero`/`*_low` forms.

use super::lanes::{f32x4, f64x2, from_f32x4, from_f64x2, from_i32x4, i32x4, u32x4};
use super::Execution;
use crate::exec::arith::{canon_f32, canon_f64};
use crate::exec::convert::{
    i32_trunc_sat_f32_s, i32_trunc_sat_f32_u, i32_trunc_sat_f64_s, i32_trunc_sat_f64_u,
};
use crate::module::op_simd::SimdOp;
use crate::Result;

impl Execution {
    pub(super) fn exec_simd_cvt(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            S::I32x4TruncSatF32x4S => self.v_unop(f32x4, from_i32x4, i32_trunc_sat_f32_s),
            S::I32x4TruncSatF32x4U => self.v_unop(f32x4, from_i32x4, i32_trunc_sat_f32_u),
            S::F32x4ConvertI32x4S => self.v_unop(i32x4, from_f32x4, |x| x as f32),
            S::F32x4ConvertI32x4U => self.v_unop(u32x4, from_f32x4, |x| x as f32),

            S::I32x4TruncSatF64x2SZero => {
                let l = f64x2(self.pop_v128());
                self.push_v128(from_i32x4([
                    i32_trunc_sat_f64_s(l[0]),
                    i32_trunc_sat_f64_s(l[1]),
                    0,
                    0,
                ]));
            }
            S::I32x4TruncSatF64x2UZero => {
                let l = f64x2(self.pop_v128());
                self.push_v128(from_i32x4([
                    i32_trunc_sat_f64_u(l[0]),
                    i32_trunc_sat_f64_u(l[1]),
                    0,
                    0,
                ]));
            }
            S::F64x2ConvertLowI32x4S => {
                let l = i32x4(self.pop_v128());
                self.push_v128(from_f64x2([l[0] as f64, l[1] as f64]));
            }
            S::F64x2ConvertLowI32x4U => {
                let l = u32x4(self.pop_v128());
                self.push_v128(from_f64x2([f64::from(l[0]), f64::from(l[1])]));
            }
            S::F32x4DemoteF64x2Zero => {
                let l = f64x2(self.pop_v128());
                let d = |x: f64| canon_f32(x as f32, x.is_nan());
                self.push_v128(from_f32x4([d(l[0]), d(l[1]), 0.0, 0.0]));
            }
            S::F64x2PromoteLowF32x4 => {
                let l = f32x4(self.pop_v128());
                let p = |x: f32| canon_f64(f64::from(x), x.is_nan());
                self.push_v128(from_f64x2([p(l[0]), p(l[1])]));
            }

            _ => return self.exec_simd_relaxed(s),
        }
        Ok(())
    }
}
