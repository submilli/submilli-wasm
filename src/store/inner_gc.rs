//! GC-heap and `externref`-arena methods on `StoreInner`, split from [`inner`](super::inner) to
//! keep that file under the size cap. Covers allocation (guest-reserved vs. host/const-eval
//! ceiling-bounded), the `extern`/`any` conversions, host-root registration, and the collector
//! entry point.

use core::any::Any;

use crate::canon::CanonicalTypeId;
use crate::value::{Rooted, Val};

use super::entity::ExternEntry;
use super::gc::{anyref_handle_slot, anyref_value, decode_anyref_handle, AnyRefHandle, GcObject};
use super::StoreInner;

impl StoreInner {
    pub(crate) fn alloc_externref(&mut self, value: Box<dyn Any + Send + Sync>) -> u32 {
        self.push_extern(ExternEntry::Host(value))
    }

    fn push_extern(&mut self, entry: ExternEntry) -> u32 {
        let index = self.externrefs.0.len() as u32;
        self.externrefs.0.push(entry);
        index
    }

    /// The host payload behind an `externref` index, if it is a host ref (not an internalized
    /// `anyref`).
    pub(crate) fn externref(&self, index: u32) -> Option<&(dyn Any + Send + Sync)> {
        match self.externrefs.0.get(index as usize)? {
            ExternEntry::Host(v) => Some(v.as_ref()),
            ExternEntry::Internal(_) => None,
        }
    }

    /// Mutable sibling of [`externref`](Self::externref).
    pub(crate) fn externref_mut(&mut self, index: u32) -> Option<&mut (dyn Any + Send + Sync)> {
        match self.externrefs.0.get_mut(index as usize)? {
            ExternEntry::Host(v) => Some(v.as_mut()),
            ExternEntry::Internal(_) => None,
        }
    }

    /// `extern.convert_any`: internal `anyref` → `externref` (host wrappers unwrap to their extern;
    /// any other ref is wrapped in a fresh `Internal` entry; a host externref passes through).
    pub(crate) fn extern_convert_any(&mut self, v: Val) -> Val {
        let handle = match v {
            Val::AnyRef(None) => return Val::ExternRef(None),
            Val::AnyRef(Some(r)) => r.raw(),
            Val::ExternRef(_) => return v,
            _ => unreachable!("extern.convert_any operand is a reference"),
        };
        if let AnyRefHandle::Slot(i) = decode_anyref_handle(handle) {
            if let Some(e) = self.gc.get(i).expect("live gc slot").extern_index() {
                return Val::ExternRef(Some(Rooted::from_raw(e)));
            }
        }
        let idx = self.push_extern(ExternEntry::Internal(handle));
        Val::ExternRef(Some(Rooted::from_raw(idx)))
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
        if let Some(ExternEntry::Internal(h)) = self.externrefs.0.get(idx as usize) {
            return Ok(anyref_value(*h));
        }
        // The extern wrapper is a tiny object created outside the run loop's reservation flow, so
        // it is bounded by the hard ceiling (a later guest collection reclaims it if unreachable).
        let slot = self.gc.alloc_unreserved(GcObject::extern_wrapper(idx))?;
        Ok(anyref_value(anyref_handle_slot(slot)))
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

    pub(crate) fn gc_object(&self, index: u32) -> Option<&GcObject> {
        self.gc.get(index)
    }

    pub(crate) fn gc_object_mut(&mut self, index: u32) -> Option<&mut GcObject> {
        self.gc.get_mut(index)
    }
}
