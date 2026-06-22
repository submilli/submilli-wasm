//! Float lanewise arithmetic (#37). Arithmetic + sqrt/rounding canonicalize NaN (reusing the scalar
//! `canon_f32/64`); `min`/`max` reuse the scalar `f*_min/max` (WebAssembly ±0 + NaN rules); `pmin`/
//! `pmax` are the spec's raw ternaries (operand-order, NaN→first, NOT canonicalized); `neg`/`abs`
//! are bitwise (preserve the NaN payload).

use super::lanes::{f32x4, f64x2, from_f32x4, from_f64x2, from_u32x4, from_u64x2, u32x4, u64x2};
use super::Execution;
use crate::exec::arith::{canon_f32, canon_f64, f32_max, f32_min, f64_max, f64_min};
use crate::module::op_simd::SimdOp;
use crate::Result;

const SIGN32: u32 = 0x8000_0000;
const SIGN64: u64 = 0x8000_0000_0000_0000;

impl Execution {
    #[allow(clippy::too_many_lines)] // flat lanewise dispatch; arms are one-liners
    pub(super) fn exec_simd_farith(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            // f32x4 arithmetic (canonicalize the result NaN unless an input was NaN)
            S::F32x4Add => self.v_binop(f32x4, from_f32x4, |a, b| canon_f32(a + b, nan2(a, b))),
            S::F32x4Sub => self.v_binop(f32x4, from_f32x4, |a, b| canon_f32(a - b, nan2(a, b))),
            S::F32x4Mul => self.v_binop(f32x4, from_f32x4, |a, b| canon_f32(a * b, nan2(a, b))),
            S::F32x4Div => self.v_binop(f32x4, from_f32x4, |a, b| canon_f32(a / b, nan2(a, b))),
            S::F32x4Min => self.v_binop(f32x4, from_f32x4, f32_min),
            S::F32x4Max => self.v_binop(f32x4, from_f32x4, f32_max),
            S::F32x4PMin => self.v_binop(f32x4, from_f32x4, |a, b| if b < a { b } else { a }),
            S::F32x4PMax => self.v_binop(f32x4, from_f32x4, |a, b| if a < b { b } else { a }),
            S::F32x4Sqrt => self.v_unop(f32x4, from_f32x4, |x| canon_f32(x.sqrt(), x.is_nan())),
            S::F32x4Ceil => self.v_unop(f32x4, from_f32x4, |x| canon_f32(x.ceil(), x.is_nan())),
            S::F32x4Floor => self.v_unop(f32x4, from_f32x4, |x| canon_f32(x.floor(), x.is_nan())),
            S::F32x4Trunc => self.v_unop(f32x4, from_f32x4, |x| canon_f32(x.trunc(), x.is_nan())),
            S::F32x4Nearest => {
                self.v_unop(f32x4, from_f32x4, |x| {
                    canon_f32(x.round_ties_even(), x.is_nan())
                });
            }
            S::F32x4Abs => self.v_unop(u32x4, from_u32x4, |x| x & !SIGN32),
            S::F32x4Neg => self.v_unop(u32x4, from_u32x4, |x| x ^ SIGN32),

            // f64x2 arithmetic
            S::F64x2Add => self.v_binop(f64x2, from_f64x2, |a, b| canon_f64(a + b, nan2d(a, b))),
            S::F64x2Sub => self.v_binop(f64x2, from_f64x2, |a, b| canon_f64(a - b, nan2d(a, b))),
            S::F64x2Mul => self.v_binop(f64x2, from_f64x2, |a, b| canon_f64(a * b, nan2d(a, b))),
            S::F64x2Div => self.v_binop(f64x2, from_f64x2, |a, b| canon_f64(a / b, nan2d(a, b))),
            S::F64x2Min => self.v_binop(f64x2, from_f64x2, f64_min),
            S::F64x2Max => self.v_binop(f64x2, from_f64x2, f64_max),
            S::F64x2PMin => self.v_binop(f64x2, from_f64x2, |a, b| if b < a { b } else { a }),
            S::F64x2PMax => self.v_binop(f64x2, from_f64x2, |a, b| if a < b { b } else { a }),
            S::F64x2Sqrt => self.v_unop(f64x2, from_f64x2, |x| canon_f64(x.sqrt(), x.is_nan())),
            S::F64x2Ceil => self.v_unop(f64x2, from_f64x2, |x| canon_f64(x.ceil(), x.is_nan())),
            S::F64x2Floor => self.v_unop(f64x2, from_f64x2, |x| canon_f64(x.floor(), x.is_nan())),
            S::F64x2Trunc => self.v_unop(f64x2, from_f64x2, |x| canon_f64(x.trunc(), x.is_nan())),
            S::F64x2Nearest => {
                self.v_unop(f64x2, from_f64x2, |x| {
                    canon_f64(x.round_ties_even(), x.is_nan())
                });
            }
            S::F64x2Abs => self.v_unop(u64x2, from_u64x2, |x| x & !SIGN64),
            S::F64x2Neg => self.v_unop(u64x2, from_u64x2, |x| x ^ SIGN64),

            _ => return self.exec_simd_fcmp(s),
        }
        Ok(())
    }
}

#[inline]
fn nan2(a: f32, b: f32) -> bool {
    a.is_nan() || b.is_nan()
}
#[inline]
fn nan2d(a: f64, b: f64) -> bool {
    a.is_nan() || b.is_nan()
}
