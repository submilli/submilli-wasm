//! Operand providers (step 2b): each operand-stack entry records the slot its value lives in.
//! `local.get` pushes the local's own slot instead of copying it, so the ops that read operands
//! through [`Translator::operand`] use the local directly; every other entry lives in its own
//! height slot (`n_locals + height`). Locals occupy the slots below `n_locals`, so an entry's slot
//! alone says which kind it is. An entry is *materialized* — copied into its own height slot —
//! only where something needs it there: an op that addresses its operands by position
//! (`slot_below`, windows, calls), a branch's kept values, or a control-flow merge.
//!
//! Merges stay consistent because a block entry materializes every local entry: inside the block,
//! `local.set` may only rewrite entries pushed within it, which never outlive the block without
//! being materialized at its end or at a branch out of it.
//!
//! Compile time stays linear on hostile input: the heights of local entries are kept in
//! ascending order (`local_heights`), and at most [`MAX_LOCAL_ENTRIES`] are live at once — a
//! further `local.get` is copied right away — so no operation scans more than that many entries.

use super::Translator;
use crate::error::InternalError;
use crate::module::op::{Op, Slot};

/// How many entries may read a local's slot at once. Real code rarely holds more than a few; the
/// cap bounds every scan of them.
const MAX_LOCAL_ENTRIES: usize = 16;

impl Translator<'_> {
    /// Pushes `n` entries computed into their own height slots.
    pub(super) fn push(&mut self, n: u32) {
        let Some(height) = self.height.checked_add(n) else {
            self.error
                .get_or_insert(InternalError::Compiler("operand height overflow"));
            return;
        };
        for h in self.height..height {
            let slot = self.slot_of(h);
            self.stack.push(slot);
        }
        self.height = height;
        self.max_operands = self.max_operands.max(height);
    }

    /// Replaces the top `n` entries with one result computed into `dst` (its own height slot).
    pub(super) fn produce(&mut self, n: u32, dst: Slot) {
        self.pop(n);
        self.push_known(dst);
    }

    #[inline]
    fn push_known(&mut self, slot: Slot) {
        let Some(height) = self.height.checked_add(1) else {
            self.error
                .get_or_insert(InternalError::Compiler("operand height overflow"));
            return;
        };
        self.stack.push(slot);
        self.height = height;
        self.max_operands = self.max_operands.max(height);
    }

    /// Pushes a `local.get`: no op, just an entry reading the local's slot — or, past
    /// [`MAX_LOCAL_ENTRIES`] live ones, a copy into its own slot.
    #[inline]
    pub(super) fn push_local(&mut self, local: u32) {
        let height = self.height;
        if self.local_heights.len() < MAX_LOCAL_ENTRIES {
            self.local_heights.push(height);
            self.push_known(local);
        } else {
            self.push(1);
            let own = self.slot_of(height);
            self.emit(Op::Copy(own, local));
        }
    }

    #[inline]
    pub(super) fn pop(&mut self, n: u32) {
        let Some(height) = self.height.checked_sub(n) else {
            self.error
                .get_or_insert(InternalError::Compiler("operand height underflow"));
            return;
        };
        self.truncate_stack(height);
    }

    /// Resets the stack absolutely to `base` entries plus `n` fresh ones in their own slots — a
    /// block end, `else`, or landing pad, where every path delivers those values there. Still
    /// counted toward the frame size: such a height can exceed any the body pushed (a payload).
    pub(super) fn reset_height(&mut self, base: u32, n: u32) {
        self.truncate_stack(base);
        self.push(n);
    }

    /// [`reset_height`](Self::reset_height) after a block's label (`end`/`else`): when the code
    /// that follows is dead, the label's values are not pushed — the translator never pops in
    /// dead code, so they would only pile up (and inflate the frame size).
    pub(super) fn reset_after_label(&mut self, base: u32, n: u32) {
        let n = if self.reachable { n } else { 0 };
        self.reset_height(base, n);
    }

    /// A new block's base height. In dead code the frozen height can sit below the block's params
    /// (they come from the polymorphic stack), so it saturates rather than failing.
    pub(super) fn block_base(&mut self, params: u32) -> u32 {
        if self.reachable {
            self.height_below(params)
        } else {
            self.height.saturating_sub(params)
        }
    }

    /// Runs on every pop, so the rare work lives out of line.
    #[inline]
    fn truncate_stack(&mut self, height: u32) {
        if height > self.height {
            self.pad_stack(height);
            return;
        }
        while self.local_heights.last().is_some_and(|&h| h >= height) {
            self.local_heights.pop();
        }
        self.stack.truncate(height as usize);
        self.height = height;
    }

    /// A label's base above the current height — a dead param block truncated below an enclosing
    /// block's base. Pad with own-slot entries: that block was entered reachable, so its entry
    /// materialized everything below its base, which no runtime write since has moved.
    #[cold]
    fn pad_stack(&mut self, height: u32) {
        self.push(height.saturating_sub(self.height));
    }

    /// The slot an op reads the operand `depth` entries below the stack top from.
    pub(super) fn operand(&mut self, depth: u32) -> Slot {
        let index = self.stack.len().checked_sub(depth as usize + 1);
        if let Some(&slot) = index.and_then(|i| self.stack.get(i)) {
            return slot;
        }
        self.error
            .get_or_insert(InternalError::Compiler("operand stack underflow"));
        0
    }

    /// Ensures the top `n` operands sit in their own height slots.
    pub(super) fn materialize(&mut self, n: u32) {
        let from = self.height_below(n);
        while let Some(&h) = self.local_heights.last().filter(|&&h| h >= from) {
            self.local_heights.pop();
            self.materialize_at(h);
        }
    }

    /// Materializes every local entry on the stack: a block entry, so no path through the block
    /// can leave an outer entry rewritten (see the module docs).
    pub(super) fn materialize_locals(&mut self) {
        self.materialize(self.height);
    }

    /// Before local `local` is written: entries still reading its old value get their own copy
    /// (one pass over the live local entries, keeping the others in order).
    pub(super) fn preserve(&mut self, local: u32) {
        let mut kept = 0;
        for i in 0..self.local_heights.len() {
            let Some(&h) = self.local_heights.get(i) else {
                break;
            };
            if self.stack.get(h as usize) == Some(&local) {
                self.materialize_at(h);
            } else if let Some(slot) = self.local_heights.get_mut(kept) {
                *slot = h;
                kept += 1;
            }
        }
        self.local_heights.truncate(kept);
    }

    /// The local read by the entry `local_heights[i]` points at.
    fn reads_at(&self, i: usize) -> Option<u32> {
        let h = *self.local_heights.get(i)?;
        self.stack.get(h as usize).copied()
    }

    /// Whether any entry still reads local `local`.
    pub(super) fn reads_local(&self, local: u32) -> bool {
        (0..self.local_heights.len()).any(|i| self.reads_at(i) == Some(local))
    }

    /// Copies the local entry at height `h` (already removed from `local_heights`) into its own
    /// height slot.
    fn materialize_at(&mut self, h: u32) {
        let own = self.slot_of(h);
        let Some(entry) = self.stack.get_mut(h as usize) else {
            return;
        };
        let src = std::mem::replace(entry, own);
        self.emit(Op::Copy(own, src));
    }

    /// Re-pushes an entry whose value lives in `slot` (`br_on_null`'s fall-through reference):
    /// a local, or the new top's own height slot.
    pub(super) fn push_entry(&mut self, slot: Slot) {
        if slot < self.n_locals {
            self.push_local(slot);
        } else {
            self.push(1);
        }
    }

    /// The frame slot of stack height `height`.
    pub(super) fn slot_of(&mut self, height: u32) -> Slot {
        self.n_locals.checked_add(height).unwrap_or_else(|| {
            self.error
                .get_or_insert(InternalError::Compiler("operand slot overflow"));
            0
        })
    }
}
