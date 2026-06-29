//! Run-loop side of garbage collection (#27g): recover the live operand/local roots, trigger a
//! collection, and the **reservation flow** that keeps guest GC allocation within a limiter-granted
//! byte budget.
//!
//! The reservation check runs *before* an allocating op pops its operands, so a collection it
//! triggers still sees those operands (the object's field values) as roots — and so a suspend to
//! grow the reservation can simply re-execute the op. Growing the reservation needs the
//! (`T`-generic) limiter, so it suspends to the driver (`Outcome::GcGrow`), exactly like
//! `memory.grow`.

// Segment indexing is into the wasmparser-validated data/elem index space (#33 carve-out).
#![allow(clippy::indexing_slicing)]

use super::{Execution, StepOutcome};
use crate::canon::Layout;
use crate::instance::Instance;
use crate::module::op::Op;
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::Result;

impl Execution {
    /// Runs a mark-sweep collection seeded with this execution's live operand/local roots. A safe
    /// point: operands are recovered from the root shadow, so live references survive.
    pub(super) fn gc_collect_now(&mut self, inner: &mut StoreInner) {
        let roots: Vec<_> = self.operand_roots().collect();
        inner.gc_collect(&roots);
    }

    /// Ensures the GC reservation covers a `charge`-byte allocation. Called with operands still on
    /// the stack. `None` ⇒ place the object now; `Some(out)` ⇒ suspend `out` so the driver grows the
    /// reservation through the limiter, then re-executes this op. An object too large for the bound
    /// (the limiter, or the abort cap when none is installed) traps later in `grow_gc_reservation` —
    /// `gc_reserve` itself never errors.
    ///
    /// **Collect-then-grow only for real pressure.** Growth *within* the pre-authorized
    /// `gc_heap_reservation` is granted directly — no limiter, no collection — since the embedder
    /// authorized that much; a store that stays within its budget never collects, it just drops.
    /// Only growth *beyond* the budget is real memory pressure, and there we collect first (reclaim
    /// before committing more) and grow through the limiter if the collection didn't free enough.
    pub(super) fn gc_reserve(
        &mut self,
        inner: &mut StoreInner,
        charge: usize,
        ip: u32,
    ) -> Option<StepOutcome> {
        if inner.gc.fits(charge) {
            return None;
        }
        // Only collect when the growth we'd need exceeds the free budget (the limiter-gated path).
        if !inner.gc.is_free_grant(inner.gc.desired_reservation(charge)) && inner.gc.is_collecting()
        {
            self.gc_collect_now(inner);
            if inner.gc.fits(charge) {
                return None;
            }
        }
        // Recompute after a possible collection (which may have lowered `used`, hence the target).
        let reserved_target = inner.gc.desired_reservation(charge);
        if inner.gc.is_free_grant(reserved_target) {
            let granted = inner.gc.grant(reserved_target);
            inner.engine().add_gc_committed(granted);
            return None;
        }
        Some(StepOutcome::DoGcGrow {
            reserved_target,
            bytes_needed: charge as u64,
            return_ip: ip,
        })
    }

    /// The GC-heap byte charge of an allocating op (`None` for a non-allocating op), computed
    /// *without* popping — array element counts are peeked off the top of the operand stack, which
    /// re-execution after a reservation grow leaves untouched.
    pub(super) fn gc_alloc_charge(
        &self,
        inner: &StoreInner,
        instance: Instance,
        op: &Op,
    ) -> Result<Option<usize>> {
        let module = inner.instance(instance).module.clone();
        let data_len = match op {
            Op::StructNew(ty) | Op::StructNewDefault(ty) => match module.inner().layout(*ty) {
                Layout::Struct { size, .. } => *size,
                Layout::Array { .. } => unreachable!("struct.new on an array type"),
            },
            Op::ArrayNew(ty) | Op::ArrayNewDefault(ty) => {
                let stride = module.inner().layout(*ty).stride();
                array_bytes(self.peek_count(), stride)?
            }
            Op::ArrayNewFixed { ty, n } => module.inner().layout(*ty).body_size(*n as usize),
            Op::ArrayNewData { ty, data } => {
                let stride = module.inner().layout(*ty).stride();
                let dropped = inner.instance(instance).dropped_data[*data as usize];
                let seg = if dropped {
                    0
                } else {
                    module.inner().datas[*data as usize].bytes.len()
                };
                seg_clamped_charge(self.peek_count(), stride, seg)
            }
            Op::ArrayNewElem { ty, elem } => {
                let stride = module.inner().layout(*ty).stride();
                let seg = inner.instance(instance).elems[*elem as usize]
                    .len()
                    .saturating_mul(stride);
                seg_clamped_charge(self.peek_count(), stride, seg)
            }
            _ => return Ok(None),
        };
        Ok(Some(inner.gc_object_charge(data_len)))
    }

    /// The top operand read as an unsigned element count (the array length on `array.new*`).
    fn peek_count(&self) -> usize {
        self.top_i32() as u32 as usize
    }
}

/// `count * stride`, trapping (not overflowing) on a hostile element count.
fn array_bytes(count: usize, stride: usize) -> Result<usize> {
    count
        .checked_mul(stride)
        .ok_or_else(|| Trap::AllocationTooLarge.into())
}

/// The reservation charge for `array.new_data`/`array.new_elem`: the count-based body size clamped to
/// the source segment's byte length. The clamp stops a hostile (soon-to-be-out-of-bounds or
/// overflowing) count from over-reserving — the op then re-runs and traps with the correct
/// out-of-bounds / too-large error from its own handler, while the reservation only ever grew by a
/// bounded, segment-sized amount. A legitimate in-bounds count charges its exact body size.
fn seg_clamped_charge(count: usize, stride: usize, seg_bytes: usize) -> usize {
    count
        .checked_mul(stride)
        .map_or(seg_bytes, |v| v.min(seg_bytes))
}
