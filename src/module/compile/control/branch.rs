//! Branch lowering + the forward-target patch machinery: `br`/`br_if` (with the fused
//! compare-and-branch collapse), `br_table` (side-tabled targets), `return`, and the
//! `PatchSlot` resolution that writes forward `ip`s at each frame's `end`.

use wasmparser::BrTable;

use super::{BlockKind, Patch, PatchSlot};
use crate::error::InternalError;
use crate::module::compile::{fused_branch, wp_err, Translator};
use crate::module::op::{BrTableRange, BranchTarget, Op, Slot, NULLABLE_BIT};
use crate::Result;

/// Packs a branch's operand-stack fixup into the 16-byte `BranchTarget` form, rejecting a
/// function whose operand stack outgrows `u16` — a compile-time resource bound (like the wasm
/// stack limit); `keep` is a label arity, spec-capped at 1000, so only pathological `pop`s hit it.
pub(super) fn fixup(keep: u32, pop: u32) -> Result<(u16, u16)> {
    let conv = |v: u32| {
        u16::try_from(v).map_err(|_| crate::Error::msg("branch stack fixup exceeds 65535 operands"))
    };
    Ok((conv(keep)?, conv(pop)?))
}

impl Translator<'_> {
    /// Prepares a conditional branch's condition/index operand (already popped, read from
    /// `operand`) and its kept values. A branch that keeps nothing moves no slots, so the operand
    /// may stay wherever it lives; otherwise the kept values are materialized and the operand is
    /// copied into its own slot, the stack top the runtime fixup counts down from.
    pub(super) fn branch_operand(&mut self, operand: Slot, keep: u16) -> Slot {
        if keep == 0 {
            return operand;
        }
        self.materialize(u32::from(keep));
        let top = self.height_slot(0);
        if operand != top {
            self.emit(Op::Copy(top, operand));
        }
        top
    }

    pub(in crate::module::compile) fn br(&mut self, depth: u32) -> Result<()> {
        let (target, patch_frame) = self.branch_target(depth)?;
        self.materialize(u32::from(target.keep));
        let top = self.height_slot(0);
        let idx = self.next_ip();
        self.emit(Op::Br(target, top));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        self.reachable = false;
        Ok(())
    }

    pub(in crate::module::compile) fn br_if(&mut self, depth: u32) -> Result<()> {
        let cond = self.operand(0);
        self.pop(1);
        let (target, patch_frame) = self.branch_target(depth)?;
        let s = self.branch_operand(cond, target.keep);
        // Fuse with an immediately preceding i32 relop: replace it in place (offsets stay
        // aligned — nothing is pushed) so compare-and-branch is one dispatch at runtime. Only for
        // a branch that keeps no values: the fused op carries no operand fixup.
        let fused = self.fusable_cmp.take();
        if let Some((kind, lhs, rhs, _)) = fused.filter(|f| f.3 == cond && target.keep == 0) {
            let idx = self.next_ip() - 1;
            *self.op_mut(idx) = fused_branch(kind, false, target, lhs, rhs);
            self.clear_fusion(); // the relop is gone, and with it its destination
            self.register_branch(patch_frame, idx, PatchSlot::Single);
            return Ok(());
        }
        let idx = self.next_ip();
        self.emit(Op::BrIf(target, s));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        // conditional: fall-through stays reachable with the operands intact
        Ok(())
    }

    pub(in crate::module::compile) fn br_table(&mut self, table: &BrTable<'_>) -> Result<()> {
        let index = self.operand(0);
        self.pop(1);
        let cases: Vec<u32> = table.targets().collect::<Result<_, _>>().map_err(wp_err)?;
        // Append this table's targets (cases then default) contiguously to the side-table; the
        // `Op` carries only the `{base, len}` range into it.
        let base = self.code.br_tables.len() as u32 - self.base.br_tables;
        let len = cases.len() as u32;
        let mut patches: Vec<(usize, PatchSlot)> = Vec::new();
        for (k, &depth) in cases.iter().enumerate() {
            let (bt, frame) = self.branch_target(depth)?;
            self.push_edge(bt);
            if let Some(i) = frame {
                patches.push((i, PatchSlot::TableCase(k as u32)));
            }
        }
        let (default, default_frame) = self.branch_target(table.default())?;
        self.push_edge(default);
        if let Some(i) = default_frame {
            patches.push((i, PatchSlot::TableDefault));
        }
        // Every target has the same arity, so the default's `keep` speaks for all of them.
        let s = self.branch_operand(index, default.keep);
        let idx = self.next_ip();
        self.emit(Op::BrTable(BrTableRange { base, len }, s));
        for (i, slot) in patches {
            let frame = &mut self.ctrl[i];
            frame.end_patches.push(Patch { op: idx, slot });
            frame.end_targeted = true;
        }
        self.reachable = false;
        Ok(())
    }

    pub(in crate::module::compile) fn ret(&mut self) -> Result<()> {
        let arity = self.ctrl[0].result_count;
        let pop = self
            .height
            .checked_sub(arity)
            .ok_or(InternalError::Compiler("return operand height underflow"))?;
        let (keep, pop) = fixup(arity, pop)?;
        self.materialize(arity);
        let top = self.height_slot(0);
        let idx = self.next_ip();
        self.emit(Op::Br(BranchTarget { ip: 0, keep, pop }, top));
        self.ctrl[0].end_patches.push(Patch {
            op: idx,
            slot: PatchSlot::Single,
        });
        self.ctrl[0].end_targeted = true;
        self.reachable = false;
        Ok(())
    }

    /// Computes a branch's `BranchTarget`; returns the control-frame index to
    /// patch later (forward targets), or `None` for an already-resolved loop.
    pub(super) fn branch_target(&self, depth: u32) -> Result<(BranchTarget, Option<usize>)> {
        let i = self
            .ctrl
            .len()
            .checked_sub(1)
            .and_then(|top| top.checked_sub(depth as usize))
            .ok_or(InternalError::Compiler("branch control depth out of range"))?;
        let frame = self
            .ctrl
            .get(i)
            .ok_or(InternalError::Compiler("branch control frame missing"))?;
        let arity = frame.label_arity();
        let target_height = frame
            .base_height
            .checked_add(arity)
            .ok_or(InternalError::Compiler("branch target height overflow"))?;
        let pop = self
            .height
            .checked_sub(target_height)
            .ok_or(InternalError::Compiler("branch operand height underflow"))?;
        let (keep, pop) = fixup(arity, pop)?;
        Ok(if frame.kind == BlockKind::Loop {
            (
                BranchTarget {
                    ip: frame.start_ip,
                    keep,
                    pop,
                },
                None,
            )
        } else {
            (BranchTarget { ip: 0, keep, pop }, Some(i))
        })
    }

    pub(super) fn register_branch(&mut self, frame: Option<usize>, op: u32, slot: PatchSlot) {
        if let Some(i) = frame {
            let f = &mut self.ctrl[i];
            f.end_patches.push(Patch { op, slot });
            f.end_targeted = true;
        }
    }

    pub(super) fn patch_ip(&mut self, op: u32, slot: PatchSlot, ip: u32) {
        // `br_table` targets are out-of-line: resolve the flat side-table index, then patch there
        // (kept separate to avoid aliasing `self.ops` and `self.br_table_targets`).
        if let Op::BrTable(range, _) = self.op_ref(op) {
            let range = *range;
            let flat = match slot {
                PatchSlot::TableCase(k) => range.base + k,
                PatchSlot::TableDefault => range.base + range.len,
                PatchSlot::Single => unreachable!("single slot on br_table"),
            };
            self.edge_mut(flat).ip = ip;
            return;
        }
        // `br_on_cast` edges are pooled like `br_table` cases (bit 31 of the packed index is
        // the cast's nullable flag).
        if let Op::BrOnCast { target, .. } | Op::BrOnCastFail { target, .. } = self.op_ref(op) {
            let flat = target & !NULLABLE_BIT;
            self.edge_mut(flat).ip = ip;
            return;
        }
        match self.op_mut(op) {
            Op::Br(t, _)
            | Op::BrIf(t, _)
            | Op::BrIfNot(t, _)
            | Op::BrOnNull(t, _)
            | Op::BrOnNonNull(t, _) => {
                t.ip = ip;
            }
            Op::BrIfCmp { ip: target_ip, .. } | Op::BrIfCmpImm { ip: target_ip, .. } => {
                *target_ip = ip;
            }
            _ => unreachable!("patch on non-branch op"),
        }
    }
}
