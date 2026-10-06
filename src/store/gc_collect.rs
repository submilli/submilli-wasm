//! The non-moving, stop-the-world **mark-sweep** collector (#27g) over the store's GC object heap.
//!
//! Precise root enumeration: operand/local roots arrive from the run loop as `(handle, RefKind)`
//! pairs (the untyped operand stack's byte-shadow — ARCHITECTURE §7); the remaining roots
//! (globals, tables, element-segment instances, the pending exception, and host-held `Rooted`s)
//! live in `StoreInner`. A single unified worklist trace follows reference fields/elements through
//! the per-type [`Layout`], spanning hierarchies (`extern.convert_any`/`any.convert_extern`).
//! Sweep frees unmarked slots (recycling their indices, bumping their generation).
//!
//! All three GC-managed arenas are reclaimed: the GC object heap (structs/arrays/extern-wrappers)
//! plus the **externref** and **exception** arenas (#27g). The trace visits each arena's reachable
//! entries (the `extern_seen`/`exn_seen` sets); whatever isn't visited is freed in the sweep, its
//! byte charge credited back to the shared GC budget.

use std::collections::HashSet;

use crate::canon::{Layout, RefKind, Slot};
use crate::error::InternalError;
use crate::value::{Ref, Val};
use crate::Result;

use super::entity::{ExnEntity, ExternEntry};
use super::gc::{decode_anyref_handle, AnyRefHandle};
use super::gc_codec::{le_u32, NULL_REF};
use super::{ObjKind, StoreInner};

/// A reachable referent pending trace: an index into one of the store's GC-managed arenas.
enum Reached {
    /// A managed `struct`/`array`/extern-wrapper object (GC heap slot).
    Gc(u32),
    /// An `externref` arena entry.
    Extern(u32),
    /// An exception instance (`exnref` arena entry).
    Exn(u32),
}

impl StoreInner {
    /// Runs one mark-sweep collection. `stack_roots` are the live operand/local references the run
    /// loop recovered from its root shadow; all other roots are read from `self`. A no-op under
    /// `Collector::Null` (which never reclaims).
    pub(crate) fn collect(&mut self, stack_roots: &[(u32, RefKind)]) -> Result<()> {
        if !self.gc.is_collecting() {
            return Ok(());
        }
        let mut work: Vec<Reached> = Vec::new();
        for &(handle, kind) in stack_roots {
            seed_handle(&mut work, handle, kind);
        }
        self.seed_entity_roots(&mut work);

        let (extern_seen, exn_seen) = match self.trace_reachable(work) {
            Ok(live) => live,
            Err(error) => {
                self.gc.clear_marks();
                return Err(error);
            }
        };

        // Free the unreachable entries of each arena, crediting their bytes back to the shared GC
        // budget; the visited sets are the live sets (#27g).
        let freed = self.externrefs.sweep(&extern_seen, ExternEntry::byte_size);
        self.gc.credit(freed);
        let freed = self.exns.sweep(&exn_seen, ExnEntity::byte_size);
        self.gc.credit(freed);
        self.gc.sweep();
        Ok(())
    }

    /// Walks the unified worklist. GC-object liveness is recorded in mark bits; the returned sets
    /// are the live entries in the separately reclaimed externref and exception arenas.
    fn trace_reachable(&mut self, mut work: Vec<Reached>) -> Result<(HashSet<u32>, HashSet<u32>)> {
        let mut extern_seen = HashSet::new();
        let mut exn_seen = HashSet::new();
        while let Some(r) = work.pop() {
            match r {
                Reached::Gc(i) => {
                    if self.gc.get(i).is_none() {
                        return Err(InternalError::GcMetadata("dangling GC root").into());
                    }
                    if self.gc.mark(i) {
                        self.trace_gc(i, &mut work)?;
                    }
                }
                Reached::Extern(i) => {
                    if extern_seen.insert(i) {
                        self.trace_extern(i, &mut work)?;
                    }
                }
                Reached::Exn(i) => {
                    if exn_seen.insert(i) {
                        self.trace_exn(i, &mut work)?;
                    }
                }
            }
        }
        Ok((extern_seen, exn_seen))
    }

    /// Seeds the worklist from the non-stack roots: globals, table elements, element-segment
    /// instances, the pending exception, and host-held `Rooted`s.
    fn seed_entity_roots(&self, work: &mut Vec<Reached>) {
        for g in self.globals.iter() {
            seed_val(work, &g.value);
        }
        for t in self.tables.iter() {
            for r in &t.elems {
                seed_ref(work, r);
            }
        }
        for inst in self.instances.iter() {
            for seg in &inst.elems {
                for r in seg {
                    seed_ref(work, r);
                }
            }
        }
        if let Some(e) = self.pending_exception {
            work.push(Reached::Exn(e.raw()));
        }
        let roots = self
            .gc_roots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for &(handle, kind) in roots.iter() {
            seed_handle(work, handle, kind);
        }
    }

    /// Traces a freshly-marked GC object's reference fields/elements (via its `Layout`).
    fn trace_gc(&self, index: u32, work: &mut Vec<Reached>) -> Result<()> {
        let obj = self
            .gc
            .get(index)
            .ok_or(InternalError::GcMetadata("marked GC object is missing"))?;
        match obj.header.kind {
            ObjKind::Struct => {
                let fields = self
                    .engine()
                    .struct_fields(obj.header.type_id)
                    .map_err(|_| InternalError::GcMetadata("struct type metadata is missing"))?;
                for &slot in Layout::for_struct(&fields).fields() {
                    seed_slot(work, slot, &obj.data)?;
                }
            }
            ObjKind::Array => {
                let field = self
                    .engine()
                    .array_field(obj.header.type_id)
                    .map_err(|_| InternalError::GcMetadata("array type metadata is missing"))?;
                let layout = Layout::for_array(&field);
                if layout.stride() == 0 || obj.data.len() % layout.stride() != 0 {
                    return Err(InternalError::GcMetadata(
                        "array body does not match its element layout",
                    )
                    .into());
                }
                for i in 0..obj.array_len(layout.stride()) as usize {
                    seed_slot(work, layout.elem_at(i), &obj.data)?;
                }
            }
            ObjKind::Extern => {
                let idx = obj.extern_index().ok_or(InternalError::GcMetadata(
                    "extern wrapper body is truncated",
                ))?;
                work.push(Reached::Extern(idx));
            }
        }
        Ok(())
    }

    /// Traces an `externref` entry: an internalized `anyref` chains back into the GC heap; a host
    /// payload has no GC children.
    fn trace_extern(&self, index: u32, work: &mut Vec<Reached>) -> Result<()> {
        let entry = self
            .externrefs
            .get(index)
            .ok_or(InternalError::GcMetadata("dangling externref root"))?;
        if let ExternEntry::Internal(handle) = entry {
            seed_handle(work, *handle, RefKind::Any);
        }
        Ok(())
    }

    /// Traces an exception instance: its argument values are roots.
    fn trace_exn(&self, index: u32, work: &mut Vec<Reached>) -> Result<()> {
        let exn = self
            .exns
            .get(index)
            .ok_or(InternalError::GcMetadata("dangling exception root"))?;
        for v in &exn.args {
            seed_val(work, v);
        }
        Ok(())
    }
}

/// Pushes the referent of a raw `(handle, hierarchy)` onto the worklist (null/`i31`/funcref → no-op).
fn seed_handle(work: &mut Vec<Reached>, handle: u32, kind: RefKind) {
    if handle == NULL_REF {
        return;
    }
    match kind {
        // A funcref points at a `Func` entity that lives for the store's whole life — not collected.
        RefKind::Func => {}
        RefKind::Any => {
            if let AnyRefHandle::Slot(i) = decode_anyref_handle(handle) {
                work.push(Reached::Gc(i));
            }
        }
        RefKind::Extern => work.push(Reached::Extern(handle)),
        RefKind::Exn => work.push(Reached::Exn(handle)),
    }
}

/// Seeds from a `Val` root (global value / exception argument).
fn seed_val(work: &mut Vec<Reached>, v: &Val) {
    match v {
        Val::ExternRef(Some(r)) => seed_handle(work, r.raw(), RefKind::Extern),
        Val::AnyRef(Some(r)) => seed_handle(work, r.raw(), RefKind::Any),
        Val::ExnRef(Some(r)) => seed_handle(work, r.raw(), RefKind::Exn),
        _ => {} // funcref, nulls, scalars
    }
}

/// Seeds from a `Ref` root (table element / element-segment instance).
fn seed_ref(work: &mut Vec<Reached>, r: &Ref) {
    match r {
        Ref::Extern(Some(rt)) => seed_handle(work, rt.raw(), RefKind::Extern),
        Ref::Any(Some(rt)) => seed_handle(work, rt.raw(), RefKind::Any),
        Ref::Exn(Some(rt)) => seed_handle(work, rt.raw(), RefKind::Exn),
        _ => {} // funcref, nulls
    }
}

/// Seeds from one packed slot of an object body (only reference slots carry a handle to trace).
fn seed_slot(work: &mut Vec<Reached>, slot: Slot, data: &[u8]) -> Result<()> {
    if let Slot::Ref { offset, kind } = slot {
        let end = offset
            .checked_add(4)
            .ok_or(InternalError::GcMetadata("reference slot offset overflow"))?;
        let bytes = data.get(offset..end).ok_or(InternalError::GcMetadata(
            "reference slot exceeds object body",
        ))?;
        seed_handle(work, le_u32(bytes), kind);
    }
    Ok(())
}
