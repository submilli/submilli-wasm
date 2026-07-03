//! Branch lowering + the forward-target patch machinery: `br`/`br_if` (with the fused
//! compare-and-branch collapse), `br_table` (side-tabled targets), `return`, and the
//! `PatchSlot` resolution that writes forward `ip`s at each frame's `end`.

use wasmparser::BrTable;

use super::{BlockKind, Patch, PatchSlot};
use crate::module::compile::{wp_err, Translator};
use crate::module::op::{BrTableRange, BranchTarget, Op, NULLABLE_BIT};
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
    pub(in crate::module::compile) fn br(&mut self, depth: u32) -> Result<()> {
        let (target, patch_frame) = self.branch_target(depth)?;
        let idx = self.next_ip();
        self.emit(Op::Br(target));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        self.reachable = false;
        Ok(())
    }

    pub(in crate::module::compile) fn br_if(&mut self, depth: u32) -> Result<()> {
        self.pop(1); // condition
        let (target, patch_frame) = self.branch_target(depth)?;
        // Fuse with an immediately preceding i32 relop: replace it in place (offsets stay
        // aligned — nothing is pushed) so compare-and-branch is one dispatch at runtime.
        if let Some(kind) = self.fusable_cmp.take() {
            let idx = self.next_ip() - 1;
            *self.op_mut(idx) = Op::BrIfCmp {
                kind,
                negate: false,
                target,
            };
            self.register_branch(patch_frame, idx, PatchSlot::Single);
            return Ok(());
        }
        let idx = self.next_ip();
        self.emit(Op::BrIf(target));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        // conditional: fall-through stays reachable with the operands intact
        Ok(())
    }

    pub(in crate::module::compile) fn br_table(&mut self, table: &BrTable<'_>) -> Result<()> {
        self.pop(1); // index
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
        let idx = self.next_ip();
        self.emit(Op::BrTable(BrTableRange { base, len }));
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
        let (keep, pop) = fixup(arity, self.height.saturating_sub(arity))?;
        let idx = self.next_ip();
        self.emit(Op::Br(BranchTarget { ip: 0, keep, pop }));
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
        let i = self.ctrl.len() - 1 - depth as usize;
        let frame = &self.ctrl[i];
        let arity = frame.label_arity();
        let (keep, pop) = fixup(arity, self.height.saturating_sub(frame.base_height + arity))?;
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
        if let Op::BrTable(range) = self.op_ref(op) {
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
            Op::Br(t)
            | Op::BrIf(t)
            | Op::BrIfNot(t)
            | Op::BrIfCmp { target: t, .. }
            | Op::BrOnNull(t)
            | Op::BrOnNonNull(t) => {
                t.ip = ip;
            }
            _ => unreachable!("patch on non-branch op"),
        }
    }
}
