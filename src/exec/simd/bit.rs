//! Bitwise ops over the whole 128-bit value + `v128.any_true` (#37). Operate on raw `u128`.

use super::Execution;
use crate::module::op_simd::SimdOp;
use crate::value::Val;
use crate::Result;

impl Execution {
    pub(super) fn exec_simd_bit(&mut self, s: &SimdOp) -> Result<()> {
        use SimdOp as S;
        match s {
            S::V128Not => {
                let a = self.pop_v128();
                self.push_v128(!a);
            }
            S::V128And => self.bit_binop(|a, b| a & b),
            S::V128Or => self.bit_binop(|a, b| a | b),
            S::V128Xor => self.bit_binop(|a, b| a ^ b),
            S::V128AndNot => self.bit_binop(|a, b| a & !b),
            S::V128Bitselect => {
                let c = self.pop_v128();
                let v2 = self.pop_v128();
                let v1 = self.pop_v128();
                self.push_v128((v1 & c) | (v2 & !c));
            }
            S::V128AnyTrue => {
                let v = self.pop_v128();
                self.push(Val::I32(i32::from(v != 0)));
            }
            _ => return self.exec_simd_iarith(s),
        }
        Ok(())
    }

    fn bit_binop(&mut self, f: impl Fn(u128, u128) -> u128) {
        let b = self.pop_v128();
        let a = self.pop_v128();
        self.push_v128(f(a, b));
    }
}
