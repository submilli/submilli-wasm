//! Float lanewise comparisons → all-ones/zero lane masks (#37). No NaN canonicalization: an
//! unordered compare is simply `false` (zero mask), which Rust's `<`/`==` already give for NaN.

use super::lanes::{f32x4, f64x2};
use super::Execution;
use crate::module::op_simd::SimdOp;
use crate::Result;

impl Execution {
    #[allow(clippy::float_cmp)] // lanewise `f*x*.eq`/`ne` are exact bit comparisons by spec
    pub(super) fn exec_simd_fcmp(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            S::F32x4Eq => self.v_cmp(f32x4, |a, b| a == b),
            S::F32x4Ne => self.v_cmp(f32x4, |a, b| a != b),
            S::F32x4Lt => self.v_cmp(f32x4, |a, b| a < b),
            S::F32x4Gt => self.v_cmp(f32x4, |a, b| a > b),
            S::F32x4Le => self.v_cmp(f32x4, |a, b| a <= b),
            S::F32x4Ge => self.v_cmp(f32x4, |a, b| a >= b),
            S::F64x2Eq => self.v_cmp(f64x2, |a, b| a == b),
            S::F64x2Ne => self.v_cmp(f64x2, |a, b| a != b),
            S::F64x2Lt => self.v_cmp(f64x2, |a, b| a < b),
            S::F64x2Gt => self.v_cmp(f64x2, |a, b| a > b),
            S::F64x2Le => self.v_cmp(f64x2, |a, b| a <= b),
            S::F64x2Ge => self.v_cmp(f64x2, |a, b| a >= b),
            _ => return self.exec_simd_cvt(s),
        }
        Ok(())
    }
}
