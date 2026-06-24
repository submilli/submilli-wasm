//! The garbage-collected side of `StoreInner`: the `impl` over the three **GC-managed reference
//! arenas** — the object heap (`struct`/`array`), the `externref` arena, and the `exn` arena —
//! plus host-root bookkeeping, GC-type pinning, the `extern`/`any` bridge, and the collector entry.
//!
//! These obey the collector's *allocate → reserve → reclaim* discipline: every entry charges the
//! shared GC byte budget, is reachable-traced, and is freed by [`collect`](StoreInner::collect)
//! when unreachable. That is the seam from [`inner`](super::inner), which owns the store-*lifetime*
//! entity arenas (funcs/memories/tables/globals/tags/instances) — grow-only, never collected.

use core::any::Any;
use std::sync::atomic::Ordering;

use crate::canon::CanonicalTypeId;
use crate::value::{ExnRef, ExternRef, Rooted, Val};

use super::entity::{ExnEntity, ExternEntry};
use super::gc::{anyref_handle_slot, anyref_value, decode_anyref_handle, AnyRefHandle, GcObject};
use super::StoreInner;

impl StoreInner {
    pub(crate) fn alloc_externref(
        &mut self,
        value: Box<dyn Any + Send + Sync>,
    ) -> crate::Result<u32> {
        self.push_extern(ExternEntry::Host(value))
    }

    /// Charges the entry into the GC budget (ceiling-bounded, like the other limiter-less GC allocs —
    /// host `ExternRef::new` reserves through the limiter first; the run-loop conversion path is
    /// bounded by the abort cap and reclaimed at the next collection), then stores it.
    fn push_extern(&mut self, entry: ExternEntry) -> crate::Result<u32> {
        let charge = entry.byte_size();
        if !self.gc.can_fit_limit(charge) {
            return Err(crate::trap::Trap::AllocationTooLarge.into());
        }
        self.gc.charge(charge);
        Ok(self.externrefs.alloc(entry))
    }

    /// The host payload behind an `externref` index, if it is a live host ref (not an internalized
    /// `anyref`, and not a swept slot).
    pub(crate) fn externref(&self, index: u32) -> Option<&(dyn Any + Send + Sync)> {
        match self.externrefs.get(index)? {
            ExternEntry::Host(v) => Some(v.as_ref()),
            ExternEntry::Internal(_) => None,
        }
    }

    /// Mutable sibling of [`externref`](Self::externref).
    pub(crate) fn externref_mut(&mut self, index: u32) -> Option<&mut (dyn Any + Send + Sync)> {
        match self.externrefs.get_mut(index)? {
            ExternEntry::Host(v) => Some(v.as_mut()),
            ExternEntry::Internal(_) => None,
        }
    }

    /// The current generation of an `externref` slot, for stamping a host handle at hand-out.
    pub(crate) fn externref_generation(&self, index: u32) -> u32 {
        self.externrefs.generation(index).unwrap_or(0)
    }

    /// Host-facing `externref` access: faults if the captured generation no longer matches the
    /// slot's (the referent was collected and the slot may be reused — a stale handle, #27g). `None`
    /// for a live slot that carries no host payload (an internalized `anyref`).
    pub(crate) fn externref_checked(
        &self,
        handle: Rooted<ExternRef>,
    ) -> crate::Result<Option<&(dyn Any + Send + Sync)>> {
        let idx = handle.checked(self.externrefs.generation(handle.raw()))?;
        Ok(self.externref(idx))
    }

    /// Mutable sibling of [`externref_checked`](Self::externref_checked).
    pub(crate) fn externref_checked_mut(
        &mut self,
        handle: Rooted<ExternRef>,
    ) -> crate::Result<Option<&mut (dyn Any + Send + Sync)>> {
        let idx = handle.checked(self.externrefs.generation(handle.raw()))?;
        Ok(self.externref_mut(idx))
    }

    /// `extern.convert_any`: internal `anyref` → `externref` (host wrappers unwrap to their extern;
    /// any other ref is wrapped in a fresh `Internal` entry; a host externref passes through).
    pub(crate) fn extern_convert_any(&mut self, v: Val) -> crate::Result<Val> {
        let handle = match v {
            Val::AnyRef(None) => return Ok(Val::ExternRef(None)),
            Val::AnyRef(Some(r)) => r.raw(),
            Val::ExternRef(_) => return Ok(v),
            _ => unreachable!("extern.convert_any operand is a reference"),
        };
        if let AnyRefHandle::Slot(i) = decode_anyref_handle(handle) {
            if let Some(e) = self.gc.get(i).expect("live gc slot").extern_index() {
                return Ok(Val::ExternRef(Some(Rooted::from_raw(e))));
            }
        }
        let idx = self.push_extern(ExternEntry::Internal(handle))?;
        Ok(Val::ExternRef(Some(Rooted::from_raw(idx))))
    }

    /// `any.convert_extern`: `externref` → `anyref` (an internalized entry recovers its original
    /// ref; a host extern is wrapped in a fresh `Extern` GC object; an `any`-rep value passes through).
    pub(crate) fn any_convert_extern(&mut self, v: Val) -> crate::Result<Val> {
        let idx = match v {
            Val::ExternRef(None) => return Ok(Val::AnyRef(None)),
            Val::ExternRef(Some(r)) => r.raw(),
            Val::AnyRef(_) => return Ok(v),
            _ => unreachable!("any.convert_extern operand is a reference"),
        };
        if let Some(ExternEntry::Internal(h)) = self.externrefs.get(idx) {
            return Ok(anyref_value(*h));
        }
        // The extern wrapper is a tiny object created outside the run loop's reservation flow, so
        // it is bounded by the hard ceiling (a later guest collection reclaims it if unreachable).
        let slot = self.gc.alloc_unreserved(GcObject::extern_wrapper(idx))?;
        Ok(anyref_value(anyref_handle_slot(slot)))
    }

    /// Allocates an exception instance, charging it into the GC budget (ceiling-bounded; the guest
    /// `throw` path reserves through the limiter first), and returns an **internal** (unchecked)
    /// `exnref` handle — it lives on the operand stack / pending slot as a root until caught.
    pub(crate) fn alloc_exn(&mut self, entity: ExnEntity) -> crate::Result<Rooted<ExnRef>> {
        let charge = entity.byte_size();
        if !self.gc.can_fit_limit(charge) {
            return Err(crate::trap::Trap::AllocationTooLarge.into());
        }
        self.gc.charge(charge);
        Ok(Rooted::from_raw(self.exns.alloc(entity)))
    }

    /// The exception instance behind an **internal** handle (run loop / unwinder); the entry is live
    /// by construction (it's a root while in flight), so a missing slot is an invariant violation.
    pub(crate) fn exn(&self, handle: Rooted<ExnRef>) -> &ExnEntity {
        self.exns
            .get(handle.raw())
            .expect("live exn (internal handle)")
    }

    pub(crate) fn exn_mut(&mut self, handle: Rooted<ExnRef>) -> &mut ExnEntity {
        self.exns
            .get_mut(handle.raw())
            .expect("live exn (internal handle)")
    }

    /// Host-facing exn access: the generation captured on the handle must still match the slot's,
    /// else the exception was collected and the slot may be reused — a stale handle (#27g).
    pub(crate) fn exn_checked(&self, handle: Rooted<ExnRef>) -> crate::Result<&ExnEntity> {
        let idx = handle.checked(self.exns.generation(handle.raw()))?;
        self.exns
            .get(idx)
            .ok_or_else(|| crate::Error::msg("stale exnref (exception was collected)"))
    }

    /// The current generation of an `exn` slot (for stamping a host handle at hand-out).
    pub(crate) fn exn_generation(&self, index: u32) -> Option<u32> {
        self.exns.generation(index)
    }

    /// Parks an uncaught exception's `exnref` for the embedder (`Func::call` → `ThrownException`); it
    /// is a GC root while here, retrieved via [`take_pending_exception`](Self::take_pending_exception).
    pub(crate) fn set_pending_exception(&mut self, exn: Rooted<ExnRef>) {
        self.pending_exception = Some(exn);
    }

    pub(crate) fn take_pending_exception(&mut self) -> Option<Rooted<ExnRef>> {
        let exn = self.pending_exception.take()?;
        // Stamp the slot generation on the way out: once it leaves the pending slot it is no longer a
        // root, so a later collection can reclaim it — a generation-checked handle then faults rather
        // than reading a reused slot (#27g). Internal re-raise paths use the unchecked `exn()`.
        Some(Rooted::from_raw_gen(
            exn.raw(),
            self.exns.generation(exn.raw()).unwrap_or(0),
        ))
    }

    /// Places a managed `struct`/`array` whose budget the run loop already reserved (the guest
    /// allocation path — see `Execution::gc_reserve`). Fails only on handle-space exhaustion.
    pub(crate) fn alloc_gc(&mut self, object: GcObject) -> crate::Result<u32> {
        self.gc.alloc(object)
    }

    /// Allocates a host- or const-eval-built GC object, bounded by the hard ceiling (no run-loop
    /// reservation flow available — see [`GcHeap::alloc_unreserved`]).
    pub(crate) fn alloc_gc_unreserved(&mut self, object: GcObject) -> crate::Result<u32> {
        self.gc.alloc_unreserved(object)
    }

    /// Registers a host-held GC root (a `Rooted` handed to the embedder), keeping its object alive
    /// across collections until the enclosing `RootScope` drops (or the store does). `kind` is the
    /// reference hierarchy so the collector decodes the handle correctly.
    pub(crate) fn push_gc_root(&mut self, handle: u32, kind: crate::canon::RefKind) {
        self.gc_roots.push((handle, kind));
    }

    /// The current host-root high-water mark (recorded by `RootScope::new`).
    pub(crate) fn gc_roots_mark(&self) -> usize {
        self.gc_roots.len()
    }

    /// Drops host roots back to `mark` (on `RootScope` drop).
    pub(crate) fn gc_roots_truncate(&mut self, mark: usize) {
        self.gc_roots.truncate(mark);
    }

    /// Pins a host-allocated GC object's type for the store's lifetime (idempotent per type), so its
    /// bare `type_id` stays valid even if the embedder drops its `StructType`/`ArrayType`.
    pub(crate) fn pin_gc_type(&mut self, id: CanonicalTypeId) {
        if self.gc_host_alloc_types.insert(id) {
            self.engine().incref_type(id);
        }
    }

    /// Traps unless `extra_bytes` fits under the GC-heap ceiling (pre-check for big `array.new*`).
    pub(crate) fn gc_check_capacity(&self, extra_bytes: usize) -> crate::Result<()> {
        self.gc.check_capacity(extra_bytes)
    }

    /// The heap byte charge of a GC object with a `data_len`-byte body (header + body).
    pub(crate) fn gc_object_charge(&self, data_len: usize) -> usize {
        super::gc::object_charge(data_len)
    }

    /// Runs one mark-sweep collection over the GC heap, given the run loop's live operand/local
    /// roots. A no-op under `Collector::Null`. Public entry for the run-loop reservation flow.
    pub(crate) fn gc_collect(&mut self, stack_roots: &[(u32, crate::canon::RefKind)]) {
        self.collect(stack_roots);
    }

    /// Reads and clears this store's engine-pressure GC-request mailbox: `true` ⇒ the engine asked it
    /// to collect since the last check. Clearing affects only *this* store's mailbox, so servicing it
    /// doesn't suppress the request for the engine's other stores.
    pub(crate) fn take_gc_request(&self) -> bool {
        self.gc_request.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn gc_object(&self, index: u32) -> Option<&GcObject> {
        self.gc.get(index)
    }

    pub(crate) fn gc_object_mut(&mut self, index: u32) -> Option<&mut GcObject> {
        self.gc.get_mut(index)
    }
}
