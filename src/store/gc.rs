//! The managed GC object heap: a store-side handle table for `struct`/`array` objects
//! under the **null collector** (allocate-only; freed only when the `Store` drops). Mirrors
//! the `externref` arena in [`super::inner`] but holds typed aggregates instead of opaque
//! host payloads. `i31` and nulls are never allocated here — `i31` is unboxed directly in the
//! `anyref` handle (see [`anyref_handle_i31`]).
//!
//! The mark bit and slot generation live in the object header but stay **inert** until the
//! mark-sweep collector. This whole surface is wired into execution by the host GC API
//! and the aggregate instructions; until those land it is unused — hence the
//! module-level `dead_code` allowance.
#![allow(dead_code)]
// `i31` (un)boxing is intentional 31-bit two's-complement wraparound.
#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

/// Per-store GC-heap byte ceiling when `Config::gc_memory_threshold` is unset. Generous and
/// fixed for now (no physical-RAM detection yet); a precise limiter-batch budget comes later.
const DEFAULT_GC_HEAP_LIMIT: usize = 1 << 30; // 1 GiB

/// Top bit of an `anyref` handle: set ⇒ the handle is an unboxed `i31`; clear ⇒ a heap slot
/// index (so slot indices use the low 31 bits).
const I31_TAG: u32 = 1 << 31;

/// A decoded `anyref` handle: either an unboxed `i31` value or a GC-heap slot index.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum AnyRefHandle {
    I31(i32),
    Slot(u32),
}

/// Encodes an `i31` into an `anyref` handle: tag bit set, low 31 bits carry the value's
/// two's-complement pattern (the high bit is dropped — `i31` holds 31 bits).
pub(crate) fn anyref_handle_i31(value: i32) -> u32 {
    I31_TAG | (value as u32 & !I31_TAG)
}

/// Encodes a heap slot index into an `anyref` handle (the low 31 bits).
pub(crate) fn anyref_handle_slot(index: u32) -> u32 {
    debug_assert!(
        index & I31_TAG == 0,
        "slot index overflows the 31-bit handle range"
    );
    index
}

/// Decodes an `anyref` handle back into an unboxed `i31` (sign-extended to `i32`) or a slot
/// index. `i31.get_u` re-derives the unsigned reading from the same payload.
pub(crate) fn decode_anyref_handle(handle: u32) -> AnyRefHandle {
    if handle & I31_TAG == 0 {
        AnyRefHandle::Slot(handle)
    } else {
        let payload = handle & !I31_TAG; // low 31 bits
        AnyRefHandle::I31(((payload << 1) as i32) >> 1) // sign-extend bit 30
    }
}

/// A managed object's header. `type_index` identifies the type for casts + field layout (a
/// module-relative placeholder for now; engine-canonical interning comes later). `mark` and
/// `generation` are populated but inert until a tracing collector reclaims.
#[derive(Debug)]
pub(crate) struct GcHeader {
    pub type_index: u32,
    pub mark: bool,
    pub generation: u32,
}

impl GcHeader {
    pub(crate) fn new(type_index: u32) -> Self {
        GcHeader {
            type_index,
            mark: false,
            generation: 0,
        }
    }
}

/// The payload of a managed object: a `struct`'s fields or an `array`'s elements. Each is a
/// fully-initialized `Vec<Val>` — the caller supplies defaults, never uninitialized memory
/// (zero-on-allocation).
#[derive(Debug)]
pub(crate) enum GcBody {
    Struct(Vec<Val>),
    Array(Vec<Val>),
}

/// One heap-allocated managed object (struct or array) plus its header.
#[derive(Debug)]
pub(crate) struct GcObject {
    pub header: GcHeader,
    pub body: GcBody,
}

impl GcObject {
    /// Estimated heap footprint: the inline object plus its `Vec` backing store.
    fn byte_size(&self) -> usize {
        let elems = match &self.body {
            GcBody::Struct(fields) => fields.len(),
            GcBody::Array(elems) => elems.len(),
        };
        core::mem::size_of::<GcObject>() + elems * core::mem::size_of::<Val>()
    }
}

/// The store's managed-object heap: a grow-only handle table under the null collector. A
/// `Rooted<AnyRef>` whose handle decodes to `Slot(i)` indexes `slots[i]`. Freed wholesale on
/// `Store` drop (no reclamation until a tracing collector lands).
#[derive(Debug)]
pub(crate) struct GcHeap {
    slots: Vec<GcObject>,
    used_bytes: usize,
    limit: usize,
}

impl GcHeap {
    /// Creates a heap whose byte ceiling is `threshold` (else [`DEFAULT_GC_HEAP_LIMIT`]).
    pub(crate) fn new(threshold: Option<usize>) -> Self {
        GcHeap {
            slots: Vec::new(),
            used_bytes: 0,
            limit: threshold.unwrap_or(DEFAULT_GC_HEAP_LIMIT),
        }
    }

    /// Allocates `object`, returning its slot index. Charges its estimated size against the
    /// heap ceiling; exhaustion traps (never UB, never `abort`). Allocate-only — the null
    /// collector never reclaims, so this only grows until the store drops.
    pub(crate) fn alloc(&mut self, object: GcObject) -> Result<u32> {
        let size = object.byte_size();
        let Some(used) = self
            .used_bytes
            .checked_add(size)
            .filter(|&n| n <= self.limit)
        else {
            return Err(Trap::AllocationTooLarge.into());
        };
        // Slot indices must fit the 31-bit handle range (the top bit tags `i31`).
        if self.slots.len() as u64 >= u64::from(I31_TAG) {
            return Err(Trap::AllocationTooLarge.into());
        }
        let index = self.slots.len() as u32;
        self.slots.push(object);
        self.used_bytes = used;
        Ok(index)
    }

    /// The object at `index`, or `None` if out of range (a guest-flowed handle must trap, not
    /// panic, once the aggregate instructions land).
    pub(crate) fn get(&self, index: u32) -> Option<&GcObject> {
        self.slots.get(index as usize)
    }

    pub(crate) fn get_mut(&mut self, index: u32) -> Option<&mut GcObject> {
        self.slots.get_mut(index as usize)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn obj(type_index: u32, body: GcBody) -> GcObject {
        GcObject {
            header: GcHeader::new(type_index),
            body,
        }
    }

    #[test]
    fn alloc_and_read_struct_and_array() {
        let mut heap = GcHeap::new(Some(1 << 20));
        let s = heap
            .alloc(obj(0, GcBody::Struct(vec![Val::I32(1), Val::I32(2)])))
            .unwrap();
        let a = heap
            .alloc(obj(1, GcBody::Array(vec![Val::I64(7); 3])))
            .unwrap();
        assert_ne!(s, a);
        match &heap.get(s).unwrap().body {
            GcBody::Struct(fields) => assert_eq!(fields.len(), 2),
            GcBody::Array(_) => panic!("expected struct"),
        }
        match &heap.get(a).unwrap().body {
            GcBody::Array(elems) => assert_eq!(elems.len(), 3),
            GcBody::Struct(_) => panic!("expected array"),
        }
        assert!(heap.get(999).is_none());
    }

    #[test]
    fn i31_handle_round_trips() {
        for v in [0_i32, 1, -1, 42, -42, (1 << 30) - 1, -(1 << 30)] {
            assert_eq!(
                decode_anyref_handle(anyref_handle_i31(v)),
                AnyRefHandle::I31(v)
            );
        }
        assert_eq!(
            decode_anyref_handle(anyref_handle_slot(123)),
            AnyRefHandle::Slot(123)
        );
    }

    #[test]
    fn alloc_past_limit_traps() {
        let mut heap = GcHeap::new(Some(64)); // smaller than one object
        let err = heap
            .alloc(obj(0, GcBody::Array(vec![Val::I64(0); 4])))
            .unwrap_err();
        assert_eq!(
            *err.downcast_ref::<Trap>().unwrap(),
            Trap::AllocationTooLarge
        );
    }
}
