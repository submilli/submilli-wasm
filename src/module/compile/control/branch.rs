//! Branch lowering + the forward-target patch machinery: `br`/`br_if` (with the fused
//! compare-and-branch collapse), `br_table` (side-tabled targets), `return`, and the
//! `PatchSlot` resolution that writes forward `ip`s at each frame's `end`.

use wasmparser::BrTable;

use super::{BlockKind, Patch, PatchSlot};
use crate::module::compile::{wp_err, Translator};
use crate::module::op::{BrTableRange, BranchTarget, Op};
use crate::Result;

impl Translator<'_> {
    pub(in crate::module::compile) fn br(&mut self, depth: u32) {
        let (target, patch_frame) = self.branch_target(depth);
        let idx = self.ops.len() as u32;
        self.emit(Op::Br(target));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        self.reachable = false;
    }

    pub(in crate::module::compile) fn br_if(&mut self, depth: u32) {
        self.pop(1); // condition
        let (target, patch_frame) = self.branch_target(depth);
        // Fuse with an immediately preceding i32 relop: replace it in place (offsets stay
        // aligned — nothing is pushed) so compare-and-branch is one dispatch at runtime.
        if let Some(kind) = self.fusable_cmp.take() {
            let idx = self.ops.len() as u32 - 1;
            self.ops[idx as usize] = Op::BrIfCmp {
                kind,
                negate: false,
                target,
            };
            self.register_branch(patch_frame, idx, PatchSlot::Single);
            return;
        }
        let idx = self.ops.len() as u32;
        self.emit(Op::BrIf(target));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        // conditional: fall-through stays reachable with the operands intact
    }

    pub(in crate::module::compile) fn br_table(&mut self, table: &BrTable<'_>) -> Result<()> {
        self.pop(1); // index
        let cases: Vec<u32> = table.targets().collect::<Result<_, _>>().map_err(wp_err)?;
        // Append this table's targets (cases then default) contiguously to the side-table; the
        // `Op` carries only the `{base, len}` range into it.
        let base = self.br_table_targets.len() as u32;
        let len = cases.len() as u32;
        let mut patches: Vec<(usize, PatchSlot)> = Vec::new();
        for (k, &depth) in cases.iter().enumerate() {
            let (bt, frame) = self.branch_target(depth);
            self.br_table_targets.push(bt);
            if let Some(i) = frame {
                patches.push((i, PatchSlot::TableCase(k as u32)));
            }
        }
        let (default, default_frame) = self.branch_target(table.default());
        self.br_table_targets.push(default);
        if let Some(i) = default_frame {
            patches.push((i, PatchSlot::TableDefault));
        }
        let idx = self.ops.len() as u32;
        self.emit(Op::BrTable(BrTableRange { base, len }));
        for (i, slot) in patches {
            let frame = &mut self.ctrl[i];
            frame.end_patches.push(Patch { op: idx, slot });
            frame.end_targeted = true;
        }
        self.reachable = false;
        Ok(())
    }

    pub(in crate::module::compile) fn ret(&mut self) {
        let keep = self.ctrl[0].result_count;
        let pop = self.height.saturating_sub(keep);
        let idx = self.ops.len() as u32;
        self.emit(Op::Br(BranchTarget { ip: 0, keep, pop }));
        self.ctrl[0].end_patches.push(Patch {
            op: idx,
            slot: PatchSlot::Single,
        });
        self.ctrl[0].end_targeted = true;
        self.reachable = false;
    }

    /// Computes a branch's `BranchTarget`; returns the control-frame index to
    /// patch later (forward targets), or `None` for an already-resolved loop.
    pub(super) fn branch_target(&self, depth: u32) -> (BranchTarget, Option<usize>) {
        let i = self.ctrl.len() - 1 - depth as usize;
        let frame = &self.ctrl[i];
        let keep = frame.label_arity();
        let pop = self.height.saturating_sub(frame.base_height + keep);
        if frame.kind == BlockKind::Loop {
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
        }
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
        if let Op::BrTable(range) = &self.ops[op as usize] {
            let range = *range;
            let flat = match slot {
                PatchSlot::TableCase(k) => range.base + k,
                PatchSlot::TableDefault => range.base + range.len,
                PatchSlot::Single => unreachable!("single slot on br_table"),
            };
            self.br_table_targets[flat as usize].ip = ip;
            return;
        }
        match &mut self.ops[op as usize] {
            Op::Br(t)
            | Op::BrIf(t)
            | Op::BrIfNot(t)
            | Op::BrIfCmp { target: t, .. }
            | Op::BrOnNull(t)
            | Op::BrOnNonNull(t)
            | Op::BrOnCast { target: t, .. }
            | Op::BrOnCastFail { target: t, .. } => {
                t.ip = ip;
            }
            _ => unreachable!("patch on non-branch op"),
        }
    }
}
