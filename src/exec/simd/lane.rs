//! Lane access: `v128.const`, splat, extract/replace lane, shuffle, swizzle (#37).

use super::lanes::{
    f64x2, from_f32x4, from_f64x2, from_i16x8, from_i32x4, from_i64x2, from_i8x16, from_u8x16,
    i16x8, i32x4, i64x2, i8x16, u16x8, u32x4, u64x2, u8x16,
};
use super::Execution;
use crate::module::op_simd::SimdOp;
use crate::value::Val;
use crate::Result;

impl Execution {
    #[allow(clippy::too_many_lines, clippy::many_single_char_names)]
    pub(super) fn exec_simd_lane(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            S::V128Const(bits) => self.push_v128(*bits),

            S::I8x16Splat => {
                let x = self.pop_i32() as i8;
                self.push_v128(from_i8x16([x; 16]));
            }
            S::I16x8Splat => {
                let x = self.pop_i32() as i16;
                self.push_v128(from_i16x8([x; 8]));
            }
            S::I32x4Splat => {
                let x = self.pop_i32();
                self.push_v128(from_i32x4([x; 4]));
            }
            S::I64x2Splat => {
                let x = self.pop().unwrap_i64();
                self.push_v128(from_i64x2([x; 2]));
            }
            S::F32x4Splat => {
                let x = self.pop().unwrap_f32();
                self.push_v128(from_f32x4([x; 4]));
            }
            S::F64x2Splat => {
                let x = self.pop().unwrap_f64();
                self.push_v128(from_f64x2([x; 2]));
            }

            S::I8x16ExtractLaneS(l) => self.extract(|v| i32::from(i8x16(v)[*l as usize]), Val::I32),
            S::I8x16ExtractLaneU(l) => self.extract(|v| i32::from(u8x16(v)[*l as usize]), Val::I32),
            S::I16x8ExtractLaneS(l) => self.extract(|v| i32::from(i16x8(v)[*l as usize]), Val::I32),
            S::I16x8ExtractLaneU(l) => self.extract(|v| i32::from(u16x8(v)[*l as usize]), Val::I32),
            S::I32x4ExtractLane(l) => self.extract(|v| i32x4(v)[*l as usize], Val::I32),
            S::I64x2ExtractLane(l) => self.extract(|v| i64x2(v)[*l as usize], Val::I64),
            S::F32x4ExtractLane(l) => self.extract(|v| u32x4(v)[*l as usize], Val::F32),
            S::F64x2ExtractLane(l) => self.extract(|v| u64x2(v)[*l as usize], Val::F64),

            S::I8x16ReplaceLane(l) => {
                let x = self.pop_i32() as i8;
                let v = self.pop_v128();
                let mut a = i8x16(v);
                a[*l as usize] = x;
                self.push_v128(from_i8x16(a));
            }
            S::I16x8ReplaceLane(l) => {
                let x = self.pop_i32() as i16;
                let v = self.pop_v128();
                let mut a = i16x8(v);
                a[*l as usize] = x;
                self.push_v128(from_i16x8(a));
            }
            S::I32x4ReplaceLane(l) => {
                let x = self.pop_i32();
                let v = self.pop_v128();
                let mut a = i32x4(v);
                a[*l as usize] = x;
                self.push_v128(from_i32x4(a));
            }
            S::I64x2ReplaceLane(l) => {
                let x = self.pop().unwrap_i64();
                let v = self.pop_v128();
                let mut a = i64x2(v);
                a[*l as usize] = x;
                self.push_v128(from_i64x2(a));
            }
            S::F32x4ReplaceLane(l) => {
                let x = self.pop().unwrap_f32();
                let v = self.pop_v128();
                let mut a = super::lanes::f32x4(v);
                a[*l as usize] = x;
                self.push_v128(from_f32x4(a));
            }
            S::F64x2ReplaceLane(l) => {
                let x = self.pop().unwrap_f64();
                let v = self.pop_v128();
                let mut a = f64x2(v);
                a[*l as usize] = x;
                self.push_v128(from_f64x2(a));
            }

            S::I8x16Shuffle(idx) => {
                let b = u8x16(self.pop_v128());
                let a = u8x16(self.pop_v128());
                let ab: [u8; 32] = core::array::from_fn(|i| if i < 16 { a[i] } else { b[i - 16] });
                let r: [u8; 16] = core::array::from_fn(|i| ab[idx[i] as usize]);
                self.push_v128(from_u8x16(r));
            }
            S::I8x16Swizzle => {
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

            _ => return self.exec_simd_bit(s),
        }
        Ok(())
    }

    /// Pops a `v128`, extracts a scalar via `f`, and pushes it wrapped by `wrap`.
    fn extract<T>(&mut self, f: impl Fn(u128) -> T, wrap: impl Fn(T) -> Val) {
        let v = self.pop_v128();
        self.push(wrap(f(v)));
    }
}
