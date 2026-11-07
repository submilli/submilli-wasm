//! Structured control-flow lowering: blocks/branches → relative-IP `Op`s with the
//! operand-stack fixup (`keep`/`pop`) folded inline.

use wasmparser::BlockType;

use super::{fused_branch, Translator};
use crate::canon::IrHeap;
use crate::module::op::{BranchTarget, Op, NULLABLE_BIT};
use crate::Result;

mod branch;
mod calls;
mod try_table;
mod visit;

#[derive(Copy, Clone, PartialEq, Eq)]
pub(super) enum BlockKind {
    Block,
    Loop,
    If,
    TryTable,
}

/// A compile-time catch clause (one per `try_table` catch), resolved into a `HandlerRec` at the
/// matching `end` (see [`try_table`]). `tag` is `None` for `catch_all`/`catch_all_ref`.
struct HandlerClause {
    tag: Option<u32>,
    label: u32,
    payload_args: bool,
    payload_ref: bool,
}

#[derive(Copy, Clone)]
pub(super) enum PatchSlot {
    Single,
    TableCase(u32),
    TableDefault,
}

pub(super) struct Patch {
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
    /// Catch clauses (only for `BlockKind::TryTable`; empty otherwise).
    clauses: Vec<HandlerClause>,
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
    /// Kind of the innermost open frame, or `None` once the implicit function frame is closed.
    /// Backs the decode-level [`wasmparser::FrameStack`] the fused driver hands to
    /// `BinaryReader::visit_operator` — the validator remains the authority on syntax; this only
    /// steers the reader's `else`/`end` decode and its end-of-expression detection.
    pub(super) fn open_frame_kind(&self) -> Option<BlockKind> {
        self.ctrl.last().map(|f| f.kind)
    }

    /// An empty patch list for a new frame, reusing a finished frame's: a block's first incoming
    /// branch would otherwise allocate one, and its `end` free it (most compile-time allocations).
    pub(super) fn patch_list(&mut self) -> Vec<Patch> {
        self.patch_pool.pop().unwrap_or_default()
    }

    pub(super) fn recycle_patches(&mut self, mut patches: Vec<Patch>) {
        patches.clear();
        self.patch_pool.push(patches);
    }

    pub(super) fn push_func_frame(&mut self, n_results: u32) {
        let end_patches = self.patch_list();
        self.ctrl.push(CtrlFrame {
            kind: BlockKind::Block,
            base_height: 0,
            param_count: 0,
            result_count: n_results,
            start_ip: 0,
            end_patches,
            else_patch: None,
            reachable_on_entry: true,
            end_targeted: false,
            clauses: Vec::new(),
        });
    }

    fn block_arity(&self, bt: BlockType) -> (u32, u32) {
        match bt {
            BlockType::Empty => (0, 0),
            BlockType::Type(_) => (0, 1),
            BlockType::FuncType(i) => {
                let (params, results) = self.ctx.types[i as usize].func_sig();
                (params.len() as u32, results.len() as u32)
            }
        }
    }

    pub(super) fn push_block(&mut self, bt: BlockType, kind: BlockKind) {
        if self.reachable {
            self.materialize_locals();
        }
        // A `loop` start is a back-edge target (and `block`/`try_table` starts delimit frames):
        // nothing emitted before this boundary may fuse with a branch after it.
        self.clear_fusion();
        let (param_count, result_count) = self.block_arity(bt);
        let base_height = self.block_base(param_count);
        let end_patches = self.patch_list();
        self.ctrl.push(CtrlFrame {
            kind,
            base_height,
            param_count,
            result_count,
            start_ip: self.next_ip(),
            end_patches,
            else_patch: None,
            reachable_on_entry: self.reachable,
            end_targeted: false,
            clauses: Vec::new(),
        });
    }

    pub(super) fn push_if(&mut self, bt: BlockType) {
        let (param_count, result_count) = self.block_arity(bt);
        let mut else_patch = None;
        if self.reachable {
            let s = self.operand(0); // the condition (the branch keeps nothing: any slot)
            self.pop(1);
            self.materialize_locals();
            let target = BranchTarget {
                ip: 0,
                keep: 0,
                pop: 0,
            };
            // Fuse `relop; if` into one negated compare-and-branch (see `br_if`).
            let fused = self.fusable_cmp.take();
            if let Some((kind, lhs, rhs, _)) = fused.filter(|f| f.3 == s) {
                let idx = self.next_ip() - 1;
                *self.op_mut(idx) = fused_branch(kind, true, target, lhs, rhs);
                else_patch = Some(idx);
            } else {
                else_patch = Some(self.next_ip());
                self.emit(Op::BrIfNot(target, s));
            }
        }
        self.clear_fusion();
        let base_height = self.block_base(param_count);
        let end_patches = self.patch_list();
        self.ctrl.push(CtrlFrame {
            kind: BlockKind::If,
            base_height,
            param_count,
            result_count,
            start_ip: self.next_ip(),
            end_patches,
            else_patch,
            reachable_on_entry: self.reachable,
            end_targeted: false,
            clauses: Vec::new(),
        });
    }

    pub(super) fn do_else(&mut self) -> Result<()> {
        self.clear_fusion();
        let f = self.ctrl.last().expect("else without if");
        let (base, param_count, result_count, roe, else_patch) = (
            f.base_height,
            f.param_count,
            f.result_count,
            f.reachable_on_entry,
            f.else_patch,
        );
        if self.reachable {
            let (keep, _) = branch::fixup(result_count, 0)?;
            self.materialize(result_count);
            let top = self.height_slot(0);
            let idx = self.next_ip();
            self.emit(Op::Br(
                BranchTarget {
                    ip: 0,
                    keep,
                    pop: 0,
                },
                top,
            ));
            let frame = self.ctrl.last_mut().expect("if frame");
            frame.end_patches.push(Patch {
                op: idx,
                slot: PatchSlot::Single,
            });
            frame.end_targeted = true;
        }
        if let Some(idx) = else_patch {
            let else_start = self.next_ip();
            self.patch_ip(idx, PatchSlot::Single, else_start);
        }
        self.ctrl.last_mut().expect("if frame").else_patch = None;
        self.reachable = roe;
        self.reset_after_label(base, param_count);
        Ok(())
    }

    pub(super) fn do_end(&mut self) -> Result<()> {
        self.clear_fusion();
        // A reachable `try_table` emits landing pads + an exception-table span; a dead one (and
        // every other block) falls through to the plain block path below.
        let top = self.ctrl.last().expect("end without frame");
        if top.kind == BlockKind::TryTable && top.reachable_on_entry {
            return self.end_try_table();
        }
        let frame = self.ctrl.pop().expect("end without frame");
        if self.reachable {
            // The fall-through meets the branches here, which deliver into the result slots.
            self.materialize(frame.result_count);
        }
        let end_ip = self.next_ip();
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
        self.reset_after_label(frame.base_height, frame.result_count);
        self.recycle_patches(frame.end_patches);
        Ok(())
    }

    /// `throw $tag` / `throw_ref`: stack-polymorphic terminators (like `unreachable`/`ret`). No
    /// compile-time pop — the rest of the block is dead and `do_end` resets height absolutely; the
    /// interpreter pops the tag args / `exnref` at runtime.
    pub(super) fn throw(&mut self, tag_index: u32) {
        let (params, _) = self.signature(self.ctx.tag_types[tag_index as usize]);
        let s = self.slot_below(params);
        self.emit(Op::Throw(tag_index, s));
        self.reachable = false;
    }

    pub(super) fn throw_ref(&mut self) {
        let s = self.slot_below(1);
        self.emit(Op::ThrowRef(s));
        self.reachable = false;
    }

    /// `br_on_null`: on the (null) branch the reference is consumed and the target
    /// receives only its label values; on fall-through the non-null reference stays.
    /// So the branch target is computed with the reference already popped.
    pub(super) fn br_on_null(&mut self, depth: u32) -> Result<()> {
        let reference = self.operand(0);
        self.pop(1); // excluded from the branch target's operands
        let (target, patch_frame) = self.branch_target(depth)?;
        let s = self.branch_operand(reference, target.keep);
        let idx = self.next_ip();
        self.emit(Op::BrOnNull(target, s));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        self.push_entry(s); // fall-through keeps the (non-null) reference, where it now lives
        Ok(())
    }

    /// `br_on_non_null`: on the (non-null) branch the reference is kept and the target's
    /// label arity includes it; on fall-through (null) the reference is dropped. So the
    /// branch target is computed with the reference still on the stack.
    pub(super) fn br_on_non_null(&mut self, depth: u32) -> Result<()> {
        let (target, patch_frame) = self.branch_target(depth)?;
        // The kept values include the reference itself (keep >= 1).
        self.materialize(u32::from(target.keep).max(1));
        let s = self.height_slot(1); // the reference
        let idx = self.next_ip();
        self.emit(Op::BrOnNonNull(target, s));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        self.pop(1); // fall-through drops the reference
        Ok(())
    }

    /// `br_on_cast`/`br_on_cast_fail`: the reference stays on the stack on *both* edges (cast to
    /// the to-type on the matching edge, kept as the from-type on the other), so this is
    /// height-neutral — only the runtime predicate decides which way it goes.
    pub(super) fn br_on_cast(
        &mut self,
        depth: u32,
        ty: IrHeap,
        nullable: bool,
        on_fail: bool,
    ) -> Result<()> {
        let (bt, patch_frame) = self.branch_target(depth)?;
        // The edge is pooled (shared with `br_table` cases); the op carries the packed index.
        let pool = self.push_edge(bt);
        let target = pool | if nullable { NULLABLE_BIT } else { 0 };
        let heap = u32::try_from(self.code.cast_heaps.len())
            .map_err(|_| crate::Error::msg("too many br_on_cast instructions in module"))?;
        self.code.cast_heaps.push(ty);
        // A stack window whose kept values include the reference (keep >= 1).
        self.materialize(u32::from(bt.keep).max(1));
        let top = self.height_slot(0);
        let idx = self.next_ip();
        self.emit(if on_fail {
            Op::BrOnCastFail { heap, target, top }
        } else {
            Op::BrOnCast { heap, target, top }
        });
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        Ok(())
    }

    fn signature(&self, type_index: u32) -> (u32, u32) {
        let (params, results) = self.ctx.types[type_index as usize].func_sig();
        (params.len() as u32, results.len() as u32)
    }
}
