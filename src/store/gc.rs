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

use super::gc_codec::{le_u32, NULL_REF};
use crate::canon::CanonicalTypeId;
use crate::trap::Trap;
use crate::value::{AnyRef, Rooted, Val};
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

/// Wraps an `anyref` handle (slot or `i31`) as a value.
pub(crate) fn anyref_value(handle: u32) -> Val {
    Val::AnyRef(Some(Rooted::<AnyRef>::from_raw(handle)))
}

/// Which kind of aggregate a managed object is — its self-describing tag (the field/element
/// types live in the per-type `Layout`, not the object).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObjKind {
    Struct,
    Array,
    /// An externalized host `externref` wrapped as `any` (`any.convert_extern`); `data` holds the
    /// externref-arena index and `type_id` is an unused sentinel.
    Extern,
}

/// A managed object's header. `type_id` is the engine-canonical type id (baked at allocation),
/// used for casts + field layout; `kind` distinguishes struct/array/extern; `len` is the array
/// element count (0 for structs). `mark`/`generation` are inert until a tracing collector.
#[derive(Debug)]
pub(crate) struct GcHeader {
    pub type_id: CanonicalTypeId,
    pub kind: ObjKind,
    pub len: u32,
    pub mark: bool,
    pub generation: u32,
}

/// One heap-allocated managed object: a header plus a single tightly-packed body buffer. Body
/// bytes are fully initialized by the caller before the object is reachable (zero-on-allocation).
#[derive(Debug)]
pub(crate) struct GcObject {
    pub header: GcHeader,
    pub data: Box<[u8]>,
}

impl GcObject {
    /// A struct object from its packed field bytes.
    pub(crate) fn new_struct(type_id: CanonicalTypeId, data: Box<[u8]>) -> Self {
        GcObject {
            header: header(type_id, ObjKind::Struct, 0),
            data,
        }
    }

    /// An array object of `len` elements from its packed element bytes.
    pub(crate) fn new_array(type_id: CanonicalTypeId, len: u32, data: Box<[u8]>) -> Self {
        GcObject {
            header: header(type_id, ObjKind::Array, len),
            data,
        }
    }

    /// An `extern` wrapper around externref-arena index `idx`.
    pub(crate) fn extern_wrapper(idx: u32) -> Self {
        GcObject {
            header: header(CanonicalTypeId::new(u32::MAX), ObjKind::Extern, 0),
            data: idx.to_le_bytes().to_vec().into_boxed_slice(),
        }
    }

    /// The wrapped externref index, if this is an `extern` wrapper.
    pub(crate) fn extern_index(&self) -> Option<u32> {
        match self.header.kind {
            ObjKind::Extern => Some(le_u32(&self.data)),
            ObjKind::Struct | ObjKind::Array => None,
        }
    }

    /// Estimated heap footprint: the inline object plus its body buffer.
    fn byte_size(&self) -> usize {
        core::mem::size_of::<GcObject>() + self.data.len()
    }
}

fn header(type_id: CanonicalTypeId, kind: ObjKind, len: u32) -> GcHeader {
    GcHeader {
        type_id,
        kind,
        len,
        mark: false,
        generation: 0,
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
        // Slot indices must stay below `NULL_REF` (so a stored slot handle is never the null
        // sentinel) — which also keeps them inside the 31-bit `anyref` range.
        if self.slots.len() as u64 >= u64::from(NULL_REF) {
            return Err(Trap::AllocationTooLarge.into());
        }
        let index = self.slots.len() as u32;
        self.slots.push(object);
        self.used_bytes = used;
        Ok(index)
    }

    /// Traps unless `extra_bytes` more would still fit under the heap ceiling. Used to bound a
    /// large `array.new*` *before* building its backing `Vec`, so a hostile element count traps
    /// rather than aborting the process on a failed host allocation.
    pub(crate) fn check_capacity(&self, extra_bytes: usize) -> Result<()> {
        match self.used_bytes.checked_add(extra_bytes) {
            Some(n) if n <= self.limit => Ok(()),
            _ => Err(Trap::AllocationTooLarge.into()),
        }
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
    use crate::canon::{RefKind, ScalarKind, Slot};
    use crate::store::gc_codec::{read_slot, read_slot_packed, write_slot};
    use crate::value::V128;

    fn scalar(offset: usize, kind: ScalarKind) -> Slot {
        Slot::Scalar { offset, kind }
    }

    #[test]
    fn alloc_and_read_struct_and_array() {
        let mut heap = GcHeap::new(Some(1 << 20));
        // struct { i32, i32 } at offsets 0,4.
        let mut sdata = vec![0u8; 8];
        write_slot(scalar(0, ScalarKind::I32), &mut sdata, Val::I32(1));
        write_slot(scalar(4, ScalarKind::I32), &mut sdata, Val::I32(2));
        let s = heap
            .alloc(GcObject::new_struct(
                CanonicalTypeId::new(0),
                sdata.into_boxed_slice(),
            ))
            .unwrap();
        // array i64[3].
        let a = heap
            .alloc(GcObject::new_array(
                CanonicalTypeId::new(1),
                3,
                vec![0u8; 24].into_boxed_slice(),
            ))
            .unwrap();
        assert_ne!(s, a);
        let so = heap.get(s).unwrap();
        assert_eq!(so.header.kind, ObjKind::Struct);
        assert_eq!(
            read_slot(scalar(4, ScalarKind::I32), &so.data).unwrap_i32(),
            2
        );
        let ao = heap.get(a).unwrap();
        assert_eq!(ao.header.kind, ObjKind::Array);
        assert_eq!(ao.header.len, 3);
        assert!(heap.get(999).is_none());
    }

    #[test]
    fn scalar_round_trip_per_kind() {
        let cases = [
            (ScalarKind::I8, Val::I32(-1), 1),
            (ScalarKind::I16, Val::I32(-1), 2),
            (ScalarKind::I32, Val::I32(-123_456), 4),
            (ScalarKind::I64, Val::I64(i64::MIN), 8),
            (ScalarKind::F32, Val::F32(1.5_f32.to_bits()), 4),
            (ScalarKind::F64, Val::F64(2.5_f64.to_bits()), 8),
            (ScalarKind::V128, Val::V128(V128::from(u128::MAX)), 16),
        ];
        for (kind, v, width) in cases {
            let mut data = vec![0u8; width];
            write_slot(scalar(0, kind), &mut data, v);
            let got = read_slot(scalar(0, kind), &data);
            // Packed kinds read back zero-extended; compare via the i32 low bits there.
            match kind {
                ScalarKind::I8 => assert_eq!(got.unwrap_i32(), 0xFF),
                ScalarKind::I16 => assert_eq!(got.unwrap_i32(), 0xFFFF),
                _ => assert_eq!(format!("{got:?}"), format!("{v:?}")),
            }
        }
    }

    #[test]
    fn packed_sign_and_zero_extension() {
        let mut data = vec![0u8; 2];
        write_slot(scalar(0, ScalarKind::I8), &mut data, Val::I32(-1));
        assert_eq!(read_slot_packed(scalar(0, ScalarKind::I8), &data, true), -1);
        assert_eq!(
            read_slot_packed(scalar(0, ScalarKind::I8), &data, false),
            0xFF
        );
    }

    #[test]
    fn ref_slot_null_and_nonnull() {
        let slot = Slot::Ref {
            offset: 0,
            kind: RefKind::Any,
        };
        let mut data = vec![0u8; 4];
        write_slot(slot, &mut data, Val::AnyRef(None));
        assert!(matches!(read_slot(slot, &data), Val::AnyRef(None)));
        write_slot(slot, &mut data, anyref_value(anyref_handle_slot(42)));
        match read_slot(slot, &data) {
            Val::AnyRef(Some(r)) => assert_eq!(r.raw(), 42),
            _ => panic!("expected non-null anyref"),
        }
    }

    #[test]
    fn packed_i8_array_body_is_compact() {
        // An i8[1000] body is 1000 bytes, not 1000 * size_of::<Val>().
        let obj = GcObject::new_array(CanonicalTypeId::new(0), 1000, vec![0u8; 1000].into());
        assert_eq!(obj.data.len(), 1000);
        assert!(obj.byte_size() < 1000 + 64);
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
        let mut heap = GcHeap::new(Some(64)); // smaller than the object below
        let err = heap
            .alloc(GcObject::new_array(
                CanonicalTypeId::new(0),
                256,
                vec![0u8; 256].into_boxed_slice(),
            ))
            .unwrap_err();
        assert_eq!(
            *err.downcast_ref::<Trap>().unwrap(),
            Trap::AllocationTooLarge
        );
    }
}
