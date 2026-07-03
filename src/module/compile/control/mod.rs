//! Structured control-flow lowering: blocks/branches → relative-IP `Op`s with the
//! operand-stack fixup (`keep`/`pop`) folded inline.

use wasmparser::BlockType;

use super::Translator;
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
        // A `loop` start is a back-edge target (and `block`/`try_table` starts delimit frames):
        // nothing emitted before this boundary may fuse with a branch after it.
        self.fusable_cmp = None;
        let (param_count, result_count) = self.block_arity(bt);
        let base_height = self.height.saturating_sub(param_count);
        self.ctrl.push(CtrlFrame {
            kind,
            base_height,
            param_count,
            result_count,
            start_ip: self.next_ip(),
            end_patches: Vec::new(),
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
            self.pop(1); // condition
                         // Fuse `relop; if` into one negated compare-and-branch (see `br_if`).
            if let Some(kind) = self.fusable_cmp.take() {
                let idx = self.next_ip() - 1;
                *self.op_mut(idx) = Op::BrIfCmp {
                    kind,
                    negate: true,
                    target: BranchTarget {
                        ip: 0,
                        keep: 0,
                        pop: 0,
                    },
                };
                else_patch = Some(idx);
            } else {
                else_patch = Some(self.next_ip());
                self.emit(Op::BrIfNot(BranchTarget {
                    ip: 0,
                    keep: 0,
                    pop: 0,
                }));
            }
        }
        self.fusable_cmp = None;
        let base_height = self.height.saturating_sub(param_count);
        self.ctrl.push(CtrlFrame {
            kind: BlockKind::If,
            base_height,
            param_count,
            result_count,
            start_ip: self.next_ip(),
            end_patches: Vec::new(),
            else_patch,
            reachable_on_entry: self.reachable,
            end_targeted: false,
            clauses: Vec::new(),
        });
    }

    pub(super) fn do_else(&mut self) -> Result<()> {
        self.fusable_cmp = None;
        let f = self.ctrl.last().expect("else without if");
        let (base, param_count, result_count, roe, else_patch) = (
            f.base_height,
            f.param_count,
            f.result_count,
            f.reachable_on_entry,
            f.else_patch,
        );
        if self.reachable {
            let idx = self.next_ip();
            let (keep, _) = branch::fixup(result_count, 0)?;
            self.emit(Op::Br(BranchTarget {
                ip: 0,
                keep,
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
            let else_start = self.next_ip();
            self.patch_ip(idx, PatchSlot::Single, else_start);
        }
        self.ctrl.last_mut().expect("if frame").else_patch = None;
        self.reachable = roe;
        self.height = base + param_count;
        Ok(())
    }

    pub(super) fn do_end(&mut self) -> Result<()> {
        self.fusable_cmp = None;
        // A reachable `try_table` emits landing pads + an exception-table span; a dead one (and
        // every other block) falls through to the plain block path below.
        let top = self.ctrl.last().expect("end without frame");
        if top.kind == BlockKind::TryTable && top.reachable_on_entry {
            return self.end_try_table();
        }
        let frame = self.ctrl.pop().expect("end without frame");
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
        self.height = frame.base_height + frame.result_count;
        Ok(())
    }

    /// `throw $tag` / `throw_ref`: stack-polymorphic terminators (like `unreachable`/`ret`). No
    /// compile-time pop — the rest of the block is dead and `do_end` resets height absolutely; the
    /// interpreter pops the tag args / `exnref` at runtime.
    pub(super) fn throw(&mut self, tag_index: u32) {
        self.emit(Op::Throw(tag_index));
        self.reachable = false;
    }

    pub(super) fn throw_ref(&mut self) {
        self.emit(Op::ThrowRef);
        self.reachable = false;
    }

    /// `br_on_null`: on the (null) branch the reference is consumed and the target
    /// receives only its label values; on fall-through the non-null reference stays.
    /// So the branch target is computed with the reference already popped.
    pub(super) fn br_on_null(&mut self, depth: u32) -> Result<()> {
        self.pop(1); // reference (excluded from the branch target's operands)
        let (target, patch_frame) = self.branch_target(depth)?;
        let idx = self.next_ip();
        self.emit(Op::BrOnNull(target));
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        self.push(1); // fall-through keeps the (non-null) reference
        Ok(())
    }

    /// `br_on_non_null`: on the (non-null) branch the reference is kept and the target's
    /// label arity includes it; on fall-through (null) the reference is dropped. So the
    /// branch target is computed with the reference still on the stack.
    pub(super) fn br_on_non_null(&mut self, depth: u32) -> Result<()> {
        let (target, patch_frame) = self.branch_target(depth)?;
        let idx = self.next_ip();
        self.emit(Op::BrOnNonNull(target));
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
        let idx = self.next_ip();
        self.emit(if on_fail {
            Op::BrOnCastFail { ty, target }
        } else {
            Op::BrOnCast { ty, target }
        });
        self.register_branch(patch_frame, idx, PatchSlot::Single);
        Ok(())
    }

    fn signature(&self, type_index: u32) -> (u32, u32) {
        let (params, results) = self.ctx.types[type_index as usize].func_sig();
        (params.len() as u32, results.len() as u32)
    }
}
