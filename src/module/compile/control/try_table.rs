//! `try_table` lowering (#28d): compile a `try_table` like a `block`, then at its `end` emit a
//! one-instruction landing pad (`Op::Br` to the clause's label) per catch clause and record an
//! exception-table [`HandlerSpan`] on the function. The unwinder (`exec::exn`) consults that span on
//! a throw. Lives in a child module of `control` so it can use the parent's private `CtrlFrame`,
//! `branch_target`, etc. without widening their visibility.

use wasmparser::{Catch, TryTable};

use super::{BlockKind, CtrlFrame, HandlerClause, PatchSlot};
use crate::module::compile::Translator;
use crate::module::handler::{HandlerRec, HandlerSpan};
use crate::module::op::{BranchTarget, Op};
use crate::Result;

fn conv_catch(c: &Catch) -> HandlerClause {
    let (tag, label, payload_args, payload_ref) = match *c {
        Catch::One { tag, label } => (Some(tag), label, true, false),
        Catch::OneRef { tag, label } => (Some(tag), label, true, true),
        Catch::All { label } => (None, label, false, false),
        Catch::AllRef { label } => (None, label, false, true),
    };
    HandlerClause {
        tag,
        label,
        payload_args,
        payload_ref,
    }
}

impl Translator<'_> {
    /// `try_table`: a `block` that also installs catch handlers. Pushes a control frame; the landing
    /// pads + exception-table span are emitted at the matching `end` ([`end_try_table`]).
    pub(in crate::module::compile) fn push_try_table(&mut self, tt: &TryTable) {
        self.fusable_cmp = None;
        let (param_count, result_count) = self.block_arity(tt.ty);
        let base_height = self.height.saturating_sub(param_count);
        self.ctrl.push(CtrlFrame {
            kind: BlockKind::TryTable,
            base_height,
            param_count,
            result_count,
            start_ip: self.next_ip(),
            end_patches: Vec::new(),
            else_patch: None,
            reachable_on_entry: self.reachable,
            end_targeted: false,
            clauses: tt.catches.iter().map(conv_catch).collect(),
        });
    }

    /// Closes a reachable `try_table`: emit a skip-branch (normal completion jumps over the landing
    /// pads), then a landing pad per catch clause, and record the exception-table span. Catch labels
    /// resolve in the **outer** context (the try_table's own label is excluded), so the frame is
    /// popped first.
    pub(in crate::module::compile) fn end_try_table(&mut self) -> Result<()> {
        let frame = self.ctrl.pop().expect("try_table frame");
        let base_height = frame.base_height;
        let body_end = self.next_ip();

        let skip_idx = self.next_ip();
        let (keep, _) = super::branch::fixup(frame.result_count, 0)?;
        self.emit(Op::Br(BranchTarget {
            ip: 0,
            keep,
            pop: 0,
        }));

        let mut recs = Vec::with_capacity(frame.clauses.len());
        for c in &frame.clauses {
            // The landing pad branches with the operand stack at restore-height + payload.
            self.height = base_height + self.payload_count(c);
            let (target, patch_frame) = self.branch_target(c.label)?;
            let landing_ip = self.next_ip();
            self.emit(Op::Br(target));
            self.register_branch(patch_frame, landing_ip, PatchSlot::Single);
            recs.push(HandlerRec {
                tag: c.tag,
                restore_height: base_height,
                payload_args: c.payload_args,
                payload_ref: c.payload_ref,
                landing_ip,
            });
        }
        let cont = self.next_ip();
        self.patch_ip(skip_idx, PatchSlot::Single, cont);
        for patch in &frame.end_patches {
            self.patch_ip(patch.op, patch.slot, cont);
        }
        self.code.handlers.push(HandlerSpan {
            start_ip: frame.start_ip,
            end_ip: body_end,
            clauses: recs.into_boxed_slice(),
        });
        self.reachable = self.reachable || frame.end_targeted;
        self.height = base_height + frame.result_count;
        Ok(())
    }

    /// The number of values a catch clause pushes: the tag's params (`catch`/`catch_ref`) plus one
    /// for the `exnref` (`catch_ref`/`catch_all_ref`).
    fn payload_count(&self, c: &HandlerClause) -> u32 {
        let args = if c.payload_args {
            let tidx = self.ctx.tag_types[c.tag.expect("catch clause has a tag") as usize];
            self.ctx.types[tidx as usize].func_sig().0.len() as u32
        } else {
            0
        };
        args + u32::from(c.payload_ref)
    }
}
