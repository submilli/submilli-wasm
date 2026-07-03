//! `v128` memory ops (#37): plain load/store, widening loads, splat loads, zero-extend loads, and
//! per-lane load/store. All reuse the bounds-checked generic [`load_n`](Execution::load_n) /
//! [`store_n`](Execution::store_n) (memory64-aware), so out-of-bounds traps exactly as scalar ops.

use super::lanes::{
    from_i16x8, from_i32x4, from_i64x2, from_u16x8, from_u32x4, from_u64x2, from_u8x16, u16x8,
    u32x4, u64x2, u8x16,
};
use super::Execution;
use crate::instance::Instance;
use crate::module::op_simd::SimdOp;
use crate::store::StoreInner;
use crate::Result;

impl Execution {
    /// Handles a SIMD memory op, returning `true` if `s` was one (else the caller dispatches the
    /// pure-compute ops).
    #[allow(clippy::too_many_lines)] // flat lanewise dispatch; arms are short
    pub(super) fn exec_simd_mem(
        &mut self,
        inner: &mut StoreInner,
        code: &crate::module::code::Code,
        s: &SimdOp,
        instance: Instance,
    ) -> Result<bool> {
        use SimdOp as S;
        match s {
            S::V128Load(m) => {
                let b = self.load_n::<16>(inner, code, instance, m)?;
                self.push_v128(u128::from_le_bytes(b));
            }
            S::V128Store(m) => {
                let v = self.pop_v128();
                self.store_n::<16>(inner, code, instance, m, v.to_le_bytes())?;
            }
            S::V128Load8x8S(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(from_i16x8(core::array::from_fn(|i| i16::from(b[i] as i8))));
            }
            S::V128Load8x8U(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(from_i16x8(core::array::from_fn(|i| i16::from(b[i]))));
            }
            S::V128Load16x4S(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(from_i32x4(core::array::from_fn(|i| {
                    i32::from(i16::from_le_bytes([b[2 * i], b[2 * i + 1]]))
                })));
            }
            S::V128Load16x4U(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(from_i32x4(core::array::from_fn(|i| {
                    i32::from(u16::from_le_bytes([b[2 * i], b[2 * i + 1]]))
                })));
            }
            S::V128Load32x2S(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(from_i64x2(core::array::from_fn(|i| {
                    i64::from(i32::from_le_bytes([
                        b[4 * i],
                        b[4 * i + 1],
                        b[4 * i + 2],
                        b[4 * i + 3],
                    ]))
                })));
            }
            S::V128Load32x2U(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(from_i64x2(core::array::from_fn(|i| {
                    i64::from(u32::from_le_bytes([
                        b[4 * i],
                        b[4 * i + 1],
                        b[4 * i + 2],
                        b[4 * i + 3],
                    ]))
                })));
            }
            S::V128Load8Splat(m) => {
                let b = self.load_n::<1>(inner, code, instance, m)?;
                self.push_v128(from_u8x16([b[0]; 16]));
            }
            S::V128Load16Splat(m) => {
                let b = self.load_n::<2>(inner, code, instance, m)?;
                self.push_v128(from_u16x8([u16::from_le_bytes(b); 8]));
            }
            S::V128Load32Splat(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_v128(from_u32x4([u32::from_le_bytes(b); 4]));
            }
            S::V128Load64Splat(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(from_u64x2([u64::from_le_bytes(b); 2]));
            }
            S::V128Load32Zero(m) => {
                let b = self.load_n::<4>(inner, code, instance, m)?;
                self.push_v128(u128::from(u32::from_le_bytes(b)));
            }
            S::V128Load64Zero(m) => {
                let b = self.load_n::<8>(inner, code, instance, m)?;
                self.push_v128(u128::from(u64::from_le_bytes(b)));
            }
            S::V128Load8Lane { mem, lane } => {
                let v = self.pop_v128();
                let b = self.load_n::<1>(inner, code, instance, mem)?;
                let mut a = u8x16(v);
                a[*lane as usize] = b[0];
                self.push_v128(from_u8x16(a));
            }
            S::V128Load16Lane { mem, lane } => {
                let v = self.pop_v128();
                let b = self.load_n::<2>(inner, code, instance, mem)?;
                let mut a = u16x8(v);
                a[*lane as usize] = u16::from_le_bytes(b);
                self.push_v128(from_u16x8(a));
            }
            S::V128Load32Lane { mem, lane } => {
                let v = self.pop_v128();
                let b = self.load_n::<4>(inner, code, instance, mem)?;
                let mut a = u32x4(v);
                a[*lane as usize] = u32::from_le_bytes(b);
                self.push_v128(from_u32x4(a));
            }
            S::V128Load64Lane { mem, lane } => {
                let v = self.pop_v128();
                let b = self.load_n::<8>(inner, code, instance, mem)?;
                let mut a = u64x2(v);
                a[*lane as usize] = u64::from_le_bytes(b);
                self.push_v128(from_u64x2(a));
            }
            S::V128Store8Lane { mem, lane } => {
                let a = u8x16(self.pop_v128());
                self.store_n::<1>(inner, code, instance, mem, [a[*lane as usize]])?;
            }
            S::V128Store16Lane { mem, lane } => {
                let a = u16x8(self.pop_v128());
                self.store_n::<2>(inner, code, instance, mem, a[*lane as usize].to_le_bytes())?;
            }
            S::V128Store32Lane { mem, lane } => {
                let a = u32x4(self.pop_v128());
                self.store_n::<4>(inner, code, instance, mem, a[*lane as usize].to_le_bytes())?;
            }
            S::V128Store64Lane { mem, lane } => {
                let a = u64x2(self.pop_v128());
                self.store_n::<8>(inner, code, instance, mem, a[*lane as usize].to_le_bytes())?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
