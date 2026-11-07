//! Immediate operand forms (step 2c): an i32 binop whose operand is a constant the previous op just
//! pushed folds it in — the constant op is dropped and an immediate form takes its place, at the
//! same ip (a label pointing there now lands on the fused op, which does the same work). The
//! constant's slot is then never written, but only this op would have read it.

use super::{fusable_cmp_of, Translator};
use crate::module::op::{CmpKind, Op, Slot};
use crate::Result;

/// How an i32 binop folds a constant operand.
#[derive(Copy, Clone)]
enum Fold {
    /// Commutative: either operand may be the constant.
    Comm(fn(Slot, Slot, i32) -> Op),
    /// Only a constant right operand folds (shifts).
    Rhs(fn(Slot, Slot, i32) -> Op),
    /// `x - c` folds as `x + (-c)`.
    Sub,
    /// An i32 comparison; a constant left operand mirrors it.
    Cmp(CmpKind),
}

impl Fold {
    /// The immediate form of `src <op> k`, or of `k <op> src` when `swapped`.
    fn build(self, dst: Slot, src: Slot, k: i32, swapped: bool) -> Option<Op> {
        Some(match self {
            Fold::Comm(op) => op(dst, src, k),
            Fold::Rhs(op) if !swapped => op(dst, src, k),
            Fold::Sub if !swapped => Op::I32AddImm(dst, src, k.wrapping_neg()),
            Fold::Cmp(kind) => Op::I32CmpImm {
                kind: if swapped { kind.mirror() } else { kind },
                dst,
                lhs: src,
                imm: k,
            },
            Fold::Rhs(_) | Fold::Sub => return None,
        })
    }
}

/// `visit_*` methods for the i32 binops with an immediate form.
macro_rules! lower_fold {
    ($($visit:ident => $variant:ident, $fold:expr;)*) => {
        $(
            pub(in crate::module::compile) fn $visit(&mut self) -> Result<()> {
                if self.reachable && !self.fold($fold) {
                    self.binop(Op::$variant);
                }
                Ok(())
            }
        )*
    };
}

impl Translator<'_> {
    lower_fold! {
        visit_i32_add => I32Add, Fold::Comm(Op::I32AddImm);
        visit_i32_sub => I32Sub, Fold::Sub;
        visit_i32_mul => I32Mul, Fold::Comm(Op::I32MulImm);
        visit_i32_and => I32And, Fold::Comm(Op::I32AndImm);
        visit_i32_or => I32Or, Fold::Comm(Op::I32OrImm);
        visit_i32_xor => I32Xor, Fold::Comm(Op::I32XorImm);
        visit_i32_shl => I32Shl, Fold::Rhs(Op::I32ShlImm);
        visit_i32_shr_s => I32ShrS, Fold::Rhs(Op::I32ShrSImm);
        visit_i32_shr_u => I32ShrU, Fold::Rhs(Op::I32ShrUImm);
        visit_i32_eq => I32Eq, Fold::Cmp(CmpKind::Eq);
        visit_i32_ne => I32Ne, Fold::Cmp(CmpKind::Ne);
        visit_i32_lt_s => I32LtS, Fold::Cmp(CmpKind::LtS);
        visit_i32_lt_u => I32LtU, Fold::Cmp(CmpKind::LtU);
        visit_i32_gt_s => I32GtS, Fold::Cmp(CmpKind::GtS);
        visit_i32_gt_u => I32GtU, Fold::Cmp(CmpKind::GtU);
        visit_i32_le_s => I32LeS, Fold::Cmp(CmpKind::LeS);
        visit_i32_le_u => I32LeU, Fold::Cmp(CmpKind::LeU);
        visit_i32_ge_s => I32GeS, Fold::Cmp(CmpKind::GeS);
        visit_i32_ge_u => I32GeU, Fold::Cmp(CmpKind::GeU);
    }

    /// Folds a just-pushed constant operand into the immediate form; `false` when there is none
    /// (the caller then emits the plain three-address form). Runs on every i32 binop, so the
    /// no-constant case stays a single inlined check.
    #[inline]
    fn fold(&mut self, fold: Fold) -> bool {
        match self.pending_const {
            Some((konst, k)) => self.fold_const(fold, konst, k),
            None => false,
        }
    }

    fn fold_const(&mut self, fold: Fold, konst: Slot, k: i32) -> bool {
        let (lhs, rhs) = (self.operand(1), self.operand(0));
        let (src, swapped) = if rhs == konst {
            (lhs, false)
        } else if lhs == konst {
            (rhs, true)
        } else {
            return false;
        };
        let dst = self.height_slot(2);
        let Some(op) = fold.build(dst, src, k, swapped) else {
            return false;
        };
        let Some(offset) = self.unemit_const(konst, k) else {
            return false;
        };
        self.produce(2, dst);
        let cmp = fusable_cmp_of(&op);
        self.emit_result(op, dst);
        self.fusable_cmp = cmp;
        // The fused op stands for the sequence the constant began: keep its source offset.
        if self.retain_offsets {
            if let Some(last) = self.code.offsets.last_mut() {
                *last = offset;
            }
        }
        true
    }
}
