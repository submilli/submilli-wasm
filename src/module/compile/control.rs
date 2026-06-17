//! Structured control-flow lowering: blocks/branches → relative-IP `Op`s with the
//! operand-stack fixup (`keep`/`pop`) folded inline.

use wasmparser::{BlockType, BrTable};

use super::{wp_err, Translator};
use crate::module::op::{BranchTarget, Op};
use crate::Result;

#[derive(Copy, Clone, PartialEq, Eq)]
pub(super) enum BlockKind {
    Block,
    Loop,
    If,
}

#[derive(Copy, Clone)]
enum PatchSlot {
    Single,
    TableCase(u32),
    TableDefault,
}

struct Patch {
    op: u32,
    slot: PatchSlot,
}

pub(super) struct CtrlFrame {
    kind: BlockKind,
    base_height: u32,
    param_count: u32,
    result_count: u32,
    start_ip: u32,
    end_patches: Vec<Patch>,
    else_patch: Option<u32>,
    reachable_on_entry: bool,
    end_targeted: bool,
}

impl CtrlFrame {
    fn label_arity(&self) -> u32 {
        if self.kind == BlockKind::Loop {
            self.param_count
        } else {
            self.result_count
        }
    }
}

impl Translator<'_> {
    pub(super) fn push_func_frame(&mut self, n_results: u32) {
        self.ctrl.push(CtrlFrame {
            kind: BlockKind::Block,
            base_height: 0,
            param_count: 0,
            result_count: n_results,
            start_ip: 0,
            end_patches: Vec::new(),
            else_patch: None,
            reachable_on_entry: true,
            end_targeted: false,
        });
    }

    fn block_arity(&self, bt: BlockType) -> (u32, u32) {
        match bt {
            BlockType::Empty => (0, 0),
            BlockType::Type(_) => (0, 1),
            BlockType::FuncType(i) => {
                let ty = &self.ctx.types[i as usize];
                (ty.params().len() as u32, ty.results().len() as u32)
            }
        }
    }

    pub(super) fn push_block(&mut self, bt: BlockType, kind: BlockKind) {
        let (param_count, result_count) = self.block_arity(bt);
        let base_height = self.height.saturating_sub(param_count);
        self.ctrl.push(CtrlFrame {
            kind,
            base_height,
            param_count,
            result_count,
            start_ip: self.ops.len() as u32,
            end_patches: Vec::new(),
            else_patch: None,
            reachable_on_entry: self.reachable,
            end_targeted: false,
        });
    }

    pub(super) fn push_if(&mut self, bt: BlockType) {
        let (param_count, result_count) = self.block_arity(bt);
        let mut else_patch = None;
        if self.reachable {
            self.pop(1); // condition
            else_patch = Some(self.ops.len() as u32);
            self.emit(Op::BrIfNot(BranchTarget {
                ip: 0,
                keep: 0,
                pop: 0,
            }));
        }
        let base_height = self.height.saturating_sub(param_count);
        self.ctrl.push(CtrlFrame {
            kind: BlockKind::If,
            base_height,
            param_count,
            result_count,
            start_ip: self.ops.len() as u32,
            end_patches: Vec::new(),
            else_patch,
            reachable_on_entry: self.reachable,
            end_targeted: false,
        });
    }

    pub(super) fn do_else(&mut self) {
        let f = self.ctrl.last().expect("else without if");
        let (base, param_count, result_count, roe, else_patch) = (
            f.base_height,
            f.param_count,
            f.result_count,
            f.reachable_on_entry,
            f.else_patch,
        );
        if self.reachable {
            let idx = self.ops.len() as u32;
            self.emit(Op::Br(BranchTarget {
                ip: 0,
                keep: result_count,
                pop: 0,
            }));
            let frame = self.ctrl.last_mut().expect("if frame");
            frame.end_patches.push(Patch {
                op: idx,
                slot: PatchSlot::Single,
            });
            frame.end_targeted = true;
        }
        if let Some(idx) = else_patch {
            let else_start = self.ops.len() as u32;
            self.patch_ip(idx, PatchSlot::Single, else_start);
        }
        self.ctrl.last_mut().expect("if frame").else_patch = None;
        self.reachable = roe;
        self.height = base + param_count;
    }

    pub(super) fn do_end(&mut self) {
        let frame = self.ctrl.pop().expect("end without frame");
        let end_ip = self.ops.len() as u32;
        if let Some(idx) = frame.else_patch {
            // else-less `if`: the cond-false path falls through to end.
            self.patch_ip(idx, PatchSlot::Single, end_ip);
        }
        for patch in &frame.end_patches {
            self.patch_ip(patch.op, patch.slot, end_ip);
        }
        self.reachable = if frame.kind == BlockKind::Loop {
            self.reachable
        } else {
            self.reachable || frame.end_targeted || frame.else_patch.is_some()
        };
        self.height = frame.base_height + frame.result_count;
    }

    pub(super) fn br(&mut self, depth: u32) {
        let (target, patch_frame) = self.branch_target(depth);
        let idx = self.ops.len() as u32;
        self.emit(Op::Br(target));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        self.reachable = false;
    }

    pub(super) fn br_if(&mut self, depth: u32) {
        self.pop(1); // condition
        let (target, patch_frame) = self.branch_target(depth);
        let idx = self.ops.len() as u32;
        self.emit(Op::BrIf(target));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        // conditional: fall-through stays reachable with the operands intact
    }

    pub(super) fn br_table(&mut self, table: &BrTable<'_>) -> Result<()> {
        self.pop(1); // index
        let cases: Vec<u32> = table.targets().collect::<Result<_, _>>().map_err(wp_err)?;
        let mut targets = Vec::with_capacity(cases.len());
        let mut patches: Vec<(usize, PatchSlot)> = Vec::new();
        for (k, &depth) in cases.iter().enumerate() {
            let (bt, frame) = self.branch_target(depth);
            targets.push(bt);
            if let Some(i) = frame {
                patches.push((i, PatchSlot::TableCase(k as u32)));
            }
        }
        let (default, default_frame) = self.branch_target(table.default());
        if let Some(i) = default_frame {
            patches.push((i, PatchSlot::TableDefault));
        }
        let idx = self.ops.len() as u32;
        self.emit(Op::BrTable {
            targets: targets.into_boxed_slice(),
            default,
        });
        for (i, slot) in patches {
            let frame = &mut self.ctrl[i];
            frame.end_patches.push(Patch { op: idx, slot });
            frame.end_targeted = true;
        }
        self.reachable = false;
        Ok(())
    }

    pub(super) fn ret(&mut self) {
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

    pub(super) fn call(&mut self, func: u32) {
        let (params, results) = self.signature(self.ctx.func_types[func as usize]);
        self.pop(params);
        self.push(results);
        self.emit(Op::Call(func));
    }

    pub(super) fn call_indirect(&mut self, type_index: u32, table: u32) {
        let (params, results) = self.signature(type_index);
        self.pop(1 + params); // table index + params
        self.push(results);
        self.emit(Op::CallIndirect {
            type_idx: type_index,
            table,
        });
    }

    fn signature(&self, type_index: u32) -> (u32, u32) {
        let ty = &self.ctx.types[type_index as usize];
        (ty.params().len() as u32, ty.results().len() as u32)
    }

    /// Computes a branch's `BranchTarget`; returns the control-frame index to
    /// patch later (forward targets), or `None` for an already-resolved loop.
    fn branch_target(&self, depth: u32) -> (BranchTarget, Option<usize>) {
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

    fn register_branch(&mut self, frame: Option<usize>, op: u32, slot: PatchSlot) {
        if let Some(i) = frame {
            let f = &mut self.ctrl[i];
            f.end_patches.push(Patch { op, slot });
            f.end_targeted = true;
        }
    }

    fn patch_ip(&mut self, op: u32, slot: PatchSlot, ip: u32) {
        match &mut self.ops[op as usize] {
            Op::Br(t) | Op::BrIf(t) | Op::BrIfNot(t) => t.ip = ip,
            Op::BrTable { targets, default } => match slot {
                PatchSlot::TableCase(k) => targets[k as usize].ip = ip,
                PatchSlot::TableDefault => default.ip = ip,
                PatchSlot::Single => unreachable!("single slot on br_table"),
            },
            _ => unreachable!("patch on non-branch op"),
        }
    }
}
