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
use crate::error::InternalError;
use crate::value::{AnyRef, ExnRef, ExternRef, Rooted, Val};

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
        let idx = handle.checked(self.externrefs.generation(handle.raw()), self)?;
        Ok(self.externref(idx))
    }

    /// Mutable sibling of [`externref_checked`](Self::externref_checked).
    pub(crate) fn externref_checked_mut(
        &mut self,
        handle: Rooted<ExternRef>,
    ) -> crate::Result<Option<&mut (dyn Any + Send + Sync)>> {
        let idx = handle.checked(self.externrefs.generation(handle.raw()), self)?;
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
            let object = self.gc.get(i).ok_or(InternalError::GcMetadata(
                "extern.convert_any referenced a missing GC object",
            ))?;
            if let Some(e) = object.extern_index() {
                return Ok(Val::ExternRef(Some(Rooted::from_raw(
                    e,
                    crate::canon::RefKind::Extern,
                ))));
            }
        }
        let idx = self.push_extern(ExternEntry::Internal(handle))?;
        Ok(Val::ExternRef(Some(Rooted::from_raw(
            idx,
            crate::canon::RefKind::Extern,
        ))))
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
        match self.externrefs.get(idx) {
            Some(ExternEntry::Internal(h)) => return Ok(anyref_value(*h)),
            Some(ExternEntry::Host(_)) => {}
            None => {
                return Err(InternalError::GcMetadata(
                    "any.convert_extern referenced a missing externref",
                )
                .into())
            }
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
        Ok(Rooted::from_raw(
            self.exns.alloc(entity),
            crate::canon::RefKind::Exn,
        ))
    }

    /// The exception instance behind an **internal** handle (run loop / unwinder); the entry is live
    /// by construction (it's a root while in flight), so a missing slot is an invariant violation.
    pub(crate) fn exn(&self, handle: Rooted<ExnRef>) -> crate::Result<&ExnEntity> {
        self.exns
            .get(handle.raw())
            .ok_or_else(|| InternalError::GcMetadata("missing in-flight exception").into())
    }

    pub(crate) fn exn_mut(&mut self, handle: Rooted<ExnRef>) -> crate::Result<&mut ExnEntity> {
        self.exns
            .get_mut(handle.raw())
            .ok_or_else(|| InternalError::GcMetadata("missing in-flight exception").into())
    }

    /// Host-facing exn access: the generation captured on the handle must still match the slot's,
    /// else the exception was collected and the slot may be reused — a stale handle (#27g).
    pub(crate) fn exn_checked(&self, handle: Rooted<ExnRef>) -> crate::Result<&ExnEntity> {
        let idx = handle.checked(self.exns.generation(handle.raw()), self)?;
        self.exns
            .get(idx)
            .ok_or_else(|| crate::Error::msg("stale exnref (exception was collected)"))
    }

    /// The current generation of an `exn` slot (for stamping a host handle at hand-out).
    pub(crate) fn exn_generation(&self, index: u32) -> Option<u32> {
        self.exns.generation(index)
    }

    /// Stamps a store-owned value before handing it to the embedder. Packed store state carries
    /// only raw indices, so reference generations and store identity are reconstructed here.
    pub(crate) fn stamp_host_value(&self, value: Val) -> crate::Result<Val> {
        match value {
            Val::FuncRef(Some(func)) => self.stamp_func(func),
            Val::AnyRef(Some(reference)) => self.stamp_anyref(reference),
            Val::ExternRef(Some(reference)) => self.stamp_externref(reference),
            Val::ExnRef(Some(reference)) => self.stamp_exnref(reference),
            _ => Ok(value),
        }
    }

    fn stamp_func(&self, func: crate::func::Func) -> crate::Result<Val> {
        self.check_handle(func.store);
        if self.funcs.get_opt(func.index).is_none() {
            return Err(
                InternalError::GcMetadata("store value referenced a missing function").into(),
            );
        }
        Ok(Val::FuncRef(Some(crate::func::Func::from_raw_store(
            func.index,
            self.store_id(),
        ))))
    }

    fn stamp_anyref(&self, reference: Rooted<AnyRef>) -> crate::Result<Val> {
        let AnyRefHandle::Slot(slot) = decode_anyref_handle(reference.raw()) else {
            reference.check_store(self);
            return Ok(Val::AnyRef(Some(Rooted::from_raw_store(
                reference.raw(),
                self.store_id(),
                crate::canon::RefKind::Any,
            ))));
        };
        if reference.gc_slot_checked(self)? != slot {
            return Err(InternalError::GcMetadata(
                "anyref handle resolved to an inconsistent GC slot",
            )
            .into());
        }
        let generation = self.gc.generation(slot).ok_or(InternalError::GcMetadata(
            "store value referenced a missing GC object",
        ))?;
        Ok(Val::AnyRef(Some(Rooted::from_raw_gen(
            reference.raw(),
            generation,
            self.store_id(),
            crate::canon::RefKind::Any,
        ))))
    }

    fn stamp_externref(&self, reference: Rooted<ExternRef>) -> crate::Result<Val> {
        let _ = self.externref_checked(reference)?;
        let generation =
            self.externrefs
                .generation(reference.raw())
                .ok_or(InternalError::GcMetadata(
                    "store value referenced a missing externref",
                ))?;
        Ok(Val::ExternRef(Some(Rooted::from_raw_gen(
            reference.raw(),
            generation,
            self.store_id(),
            crate::canon::RefKind::Extern,
        ))))
    }

    fn stamp_exnref(&self, reference: Rooted<ExnRef>) -> crate::Result<Val> {
        let _ = self.exn_checked(reference)?;
        let generation = self
            .exns
            .generation(reference.raw())
            .ok_or(InternalError::GcMetadata(
                "store value referenced a missing exception",
            ))?;
        Ok(Val::ExnRef(Some(Rooted::from_raw_gen(
            reference.raw(),
            generation,
            self.store_id(),
            crate::canon::RefKind::Exn,
        ))))
    }

    /// Transfers a reference result from the execution stack to host ownership. The operand shadow
    /// stops rooting the value at the call boundary, so non-null managed values acquire a host root
    /// and a current generation before they are returned to the embedder.
    pub(crate) fn root_host_result(&self, value: Val) -> crate::Result<Val> {
        let value = self.stamp_host_value(value)?;
        match value {
            Val::AnyRef(Some(reference)) => {
                if matches!(decode_anyref_handle(reference.raw()), AnyRefHandle::Slot(_)) {
                    self.push_gc_root(reference.raw(), crate::canon::RefKind::Any);
                }
            }
            Val::ExternRef(Some(reference)) => {
                self.push_gc_root(reference.raw(), crate::canon::RefKind::Extern);
            }
            Val::ExnRef(Some(reference)) => {
                self.push_gc_root(reference.raw(), crate::canon::RefKind::Exn);
            }
            _ => {}
        }
        Ok(value)
    }

    /// Infallible hand-out used by Wasmtime-shaped accessors whose public signatures cannot return
    /// an invariant error. Valid stored values are stamped and rooted normally. Corrupt store state
    /// is latched for the next execution boundary while the original, still self-validating handle
    /// is returned; it cannot be silently re-stamped as a different live object.
    pub(crate) fn root_stored_value(&mut self, value: Val) -> Val {
        match self.root_host_result(value) {
            Ok(value) => value,
            Err(error) => {
                let internal = error.downcast_ref::<InternalError>().copied().unwrap_or(
                    InternalError::GcMetadata("stored reference could not be handed to the host"),
                );
                self.latch_internal_error(internal);
                value
            }
        }
    }

    /// Parks an uncaught exception's `exnref` for the embedder (`Func::call` → `ThrownException`); it
    /// is a GC root while here, retrieved via [`take_pending_exception`](Self::take_pending_exception).
    pub(crate) fn set_pending_exception(&mut self, exn: Rooted<ExnRef>) {
        self.pending_exception_generation = self.pending_exception_generation.wrapping_add(1);
        self.pending_exception = Some(exn);
    }

    pub(crate) fn pending_exception(&self) -> Option<Rooted<ExnRef>> {
        self.pending_exception
    }

    pub(crate) fn pending_exception_generation(&self) -> u64 {
        self.pending_exception_generation
    }

    pub(crate) fn restore_pending_exception(
        &mut self,
        exception: Option<Rooted<ExnRef>>,
        generation: u64,
    ) {
        self.pending_exception = exception;
        self.pending_exception_generation = generation;
    }

    pub(crate) fn take_pending_exception(&mut self) -> Option<Rooted<ExnRef>> {
        self.pending_exception.take()
    }

    pub(crate) fn take_pending_exception_host(&mut self) -> Option<Rooted<ExnRef>> {
        let exn = self.pending_exception()?;
        let Some(generation) = self.exns.generation(exn.raw()) else {
            self.latch_internal_error(InternalError::GcMetadata(
                "pending exception referenced a missing exception",
            ));
            return None;
        };
        self.pending_exception = None;
        self.push_gc_root(exn.raw(), crate::canon::RefKind::Exn);
        Some(Rooted::from_raw_gen(
            exn.raw(),
            generation,
            self.store_id(),
            crate::canon::RefKind::Exn,
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
    pub(crate) fn push_gc_root(&self, handle: u32, kind: crate::canon::RefKind) {
        self.gc_roots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((handle, kind));
    }

    /// The current host-root high-water mark (recorded by `RootScope::new`).
    pub(crate) fn gc_roots_mark(&self) -> usize {
        self.gc_roots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Drops host roots back to `mark` (on `RootScope` drop).
    pub(crate) fn gc_roots_truncate(&self, mark: usize) {
        self.gc_roots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .truncate(mark);
    }

    /// Pins a host-allocated GC object's type for the store's lifetime (idempotent per type), so its
    /// bare `type_id` stays valid even if the embedder drops its `StructType`/`ArrayType`.
    pub(crate) fn pin_gc_type(&mut self, id: CanonicalTypeId) -> bool {
        let inserted = self.gc_host_alloc_types.insert(id);
        if inserted {
            self.engine().incref_type(id);
        }
        inserted
    }

    /// Rolls back a pin newly acquired for an allocation that did not commit.
    pub(crate) fn unpin_gc_type(&mut self, id: CanonicalTypeId) {
        if self.gc_host_alloc_types.remove(&id) {
            self.engine().decref_type(id);
        }
    }

    /// Allocates a host-built object while retaining its canonical type for the object's lifetime.
    /// A failed allocation rolls back a pin acquired by this attempt.
    pub(crate) fn alloc_host_gc(
        &mut self,
        id: CanonicalTypeId,
        object: GcObject,
    ) -> crate::Result<u32> {
        let pinned = self.pin_gc_type(id);
        match self.alloc_gc(object) {
            Ok(index) => Ok(index),
            Err(error) => {
                if pinned {
                    self.unpin_gc_type(id);
                }
                Err(error)
            }
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
    pub(crate) fn gc_collect(
        &mut self,
        stack_roots: &[(u32, crate::canon::RefKind)],
    ) -> crate::Result<()> {
        self.collect(stack_roots)
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
