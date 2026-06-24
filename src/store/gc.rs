//! The managed GC object heap: a store-side handle table for `struct`/`array` objects. `i31` and
//! nulls are never allocated here — `i31` is unboxed in the `anyref` handle ([`anyref_handle_i31`]).
//! The mark-sweep collector ([`super::gc_collect`]) reclaims slots into a free-list and bumps each
//! freed slot's generation (the stale-handle check); `Collector::Null` keeps it allocate-only. Guest
//! allocation draws a limiter-granted reservation; the limiter is the heap's bound, not a wasm-style
//! maximum (an `ABORT_SAFETY_CAP` only prevents an OOM-abort).
#![allow(dead_code)]
// `i31` (un)boxing is intentional 31-bit two's-complement wraparound.
#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use super::gc_codec::{le_u32, NULL_REF};
use crate::canon::CanonicalTypeId;
use crate::trap::Trap;
use crate::value::{AnyRef, Rooted, Val};
use crate::Result;

/// A fixed **abort-safety** cap on a store's GC heap (bytes), applied **only when no
/// `ResourceLimiter` is installed** (with a limiter, the limiter is the sole bound — not a wasm-style
/// maximum). It traps a hostile allocation rather than OOM-aborting, and bounds the free budget and
/// the limiter-less host/const-eval paths.
pub(crate) const ABORT_SAFETY_CAP: usize = 1 << 30; // 1 GiB

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
/// used for casts + field layout; `kind` distinguishes struct/array/extern. Deliberately tiny: the
/// array element count is *derived* from the body length and the element stride (not stored), the
/// mark bit lives in the heap's mark bitmap, and the slot *generation* lives in the heap's parallel
/// `generations` vector — so the header is just an id + a tag, and a slot is just an `Option`.
#[derive(Debug)]
pub(crate) struct GcHeader {
    pub type_id: CanonicalTypeId,
    pub kind: ObjKind,
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
            header: header(type_id, ObjKind::Struct),
            data,
        }
    }

    /// An array object from its packed element bytes. The element count is `data.len() / stride`
    /// (recovered via [`array_len`](Self::array_len)), not stored.
    pub(crate) fn new_array(type_id: CanonicalTypeId, data: Box<[u8]>) -> Self {
        GcObject {
            header: header(type_id, ObjKind::Array),
            data,
        }
    }

    /// An `extern` wrapper around externref-arena index `idx`.
    pub(crate) fn extern_wrapper(idx: u32) -> Self {
        let header = header(CanonicalTypeId::new(u32::MAX), ObjKind::Extern);
        GcObject {
            header,
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

    /// Array element count, derived from the body length and element `stride` (≥ 1 always; no div0).
    pub(crate) fn array_len(&self, stride: usize) -> u32 {
        (self.data.len() / stride.max(1)) as u32
    }

    /// Heap footprint of this object for `used`/limiter accounting: [`OBJECT_OVERHEAD`] + body.
    /// Must match [`object_charge`] so `used` is consistent across alloc/reserve/sweep.
    pub(crate) fn byte_size(&self) -> usize {
        OBJECT_OVERHEAD.saturating_add(self.data.len())
    }
}

/// Fixed per-object cost on top of the body bytes: the `slots` cell (`Option<GcObject>`), the
/// parallel `generations` `u32`, and ~16 B of `Box` allocator overhead — a slightly-conservative
/// `used` bound (mark bitmap / free-list omitted as negligible).
const OBJECT_OVERHEAD: usize =
    core::mem::size_of::<Option<GcObject>>() + core::mem::size_of::<u32>() + 16;

/// The heap byte charge of an object with a `data_len`-byte body — the reservation pre-charge (see
/// `Execution::gc_reserve`). Mirrors [`GcObject::byte_size`].
pub(crate) fn object_charge(data_len: usize) -> usize {
    OBJECT_OVERHEAD.saturating_add(data_len)
}

fn header(type_id: CanonicalTypeId, kind: ObjKind) -> GcHeader {
    GcHeader { type_id, kind }
}

/// Initial reservation-growth step. The step **doubles** each grow (so the heap ramps up quickly)
/// but is **capped at the store's `gc_heap_reservation`** (floored at this batch), so a large heap
/// grows in bounded linear chunks rather than doubling its whole footprint. ARCHITECTURE §14.
const RESERVE_BATCH: usize = 64 * 1024;

/// The store's managed-object heap (handle table). A `Rooted<AnyRef>` whose handle decodes to
/// `Slot(i)` indexes `slots[i]`. Guest allocation draws a byte budget (`reserved`) from the limiter,
/// collecting (tracing collector) then growing it when exhausted; host/const-eval allocation is
/// ceiling-bounded (`alloc_unreserved`). `Collector::Null` keeps `collecting = false`.
#[derive(Debug)]
pub(crate) struct GcHeap {
    /// The objects (or `None` when swept), indexed by slot; an empty slot is just a niche-`None`.
    slots: Vec<Option<GcObject>>,
    /// Per-slot reuse counter, **parallel** to `slots`. Bumped on free so a host `Rooted` that
    /// outlived its object faults rather than aliasing a reused slot ([`super::gc_ref`] check). Kept
    /// out of the slot so it survives the free *and* so the slot has no tail padding.
    generations: Vec<u32>,
    /// Swept slot indices available for reuse (LIFO).
    free: Vec<u32>,
    /// Bytes backing live objects.
    used: usize,
    /// Bytes the guest path may allocate within before growing again (the engine-wide committed
    /// total tracks this). Growth ≤ `reservation` skips the limiter; beyond it is limiter-gated.
    reserved: usize,
    /// Pre-authorized free budget (`Config::gc_heap_reservation`): reservation growth up to here
    /// skips the limiter, and it caps a single growth step.
    reservation: usize,
    /// The next reservation-growth step: starts at [`RESERVE_BATCH`], doubles each grow, capped at
    /// `max(reservation, RESERVE_BATCH)`.
    next_grow: usize,
    /// Mark bitmap (1 bit per slot), used only during a collection: the mark phase sets a slot's
    /// bit, sweep frees the live slots whose bit is clear, then the bitmap is cleared. Out of the
    /// object header so "clear all marks" is a `memset` and the scan is sequential, not pointer-
    /// chasing. Empty/all-zero between collections.
    marks: Vec<u64>,
    /// Whether a tracing collector runs (`Collector::Auto`/`MarkSweep`); `false` = allocate-only.
    collecting: bool,
}

impl GcHeap {
    /// Bytes currently backing live GC objects. Wasmtime's `Store::gc_heap_capacity`.
    pub(crate) fn byte_size(&self) -> usize {
        self.used
    }

    /// Creates a heap with the store's pre-authorized `reservation` (free budget + growth-step cap).
    /// The real growth bound beyond it is the limiter; `collecting` selects tracing (mark-sweep) vs.
    /// allocate-only (null) behavior.
    pub(crate) fn new(collecting: bool, reservation: usize) -> Self {
        GcHeap {
            slots: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            marks: Vec::new(),
            used: 0,
            reserved: 0,
            // The free budget skips the limiter, so bound it by the abort cap for OOM-safety.
            reservation: reservation.min(ABORT_SAFETY_CAP),
            next_grow: RESERVE_BATCH,
            collecting,
        }
    }

    pub(crate) fn is_collecting(&self) -> bool {
        self.collecting
    }

    /// Charges/credits externref/exn-arena bytes against `used` so those arenas share the GC heap's
    /// budget (collect-then-grow + the limiter account them, #27g); `credit` is the sweep's refund.
    pub(crate) fn charge(&mut self, bytes: usize) {
        self.used = self.used.saturating_add(bytes);
    }

    pub(crate) fn credit(&mut self, bytes: usize) {
        self.used = self.used.saturating_sub(bytes);
    }

    /// Bytes currently reserved (the engine-wide committed total tracks these).
    pub(crate) fn reserved(&self) -> usize {
        self.reserved
    }

    /// Whether growing to `target` stays within the free budget (granted directly — no limiter).
    pub(crate) fn is_free_grant(&self, target: usize) -> bool {
        target <= self.reservation
    }

    /// Whether this store's live footprint is large enough to bother honoring an engine-wide
    /// GC-pressure request (tiny tenants ignore it, avoiding a thundering herd).
    pub(crate) fn footprint_over_floor(&self) -> bool {
        self.used >= RESERVE_BATCH
    }

    /// Whether `size` bytes fit the current guest reservation (the fast path: no collection/limiter).
    pub(crate) fn fits(&self, size: usize) -> bool {
        self.used
            .checked_add(size)
            .is_some_and(|n| n <= self.reserved)
    }

    /// The hard ceiling for the limiter-less alloc paths (host/const-eval, `array.new_data`/`_elem`):
    /// the largest budget already legitimately established — the limiter-granted `reserved`, the free
    /// `reservation`, or the abort cap as the floor (so a limiter that grew past the cap isn't stuck).
    fn unreserved_ceiling(&self) -> usize {
        self.reserved.max(self.reservation).max(ABORT_SAFETY_CAP)
    }

    /// Whether an unreserved `size`-byte allocation fits [`unreserved_ceiling`] (else it traps).
    pub(crate) fn can_fit_limit(&self, size: usize) -> bool {
        self.used
            .checked_add(size)
            .is_some_and(|n| n <= self.unreserved_ceiling())
    }

    /// The new (absolute) reservation for an allocation of `size` that doesn't fit: one growth step
    /// (`next_grow`) beyond the current reservation, but at least enough to hold `used + size`. The
    /// step doubles only up to the reservation (linear chunks for a large heap). **Not** abort-cap
    /// bounded — the limiter decides the bound (`grow_gc_reservation` applies the cap limiter-less).
    pub(crate) fn desired_reservation(&self, size: usize) -> usize {
        let need = self.used.saturating_add(size);
        self.reserved.saturating_add(self.next_grow).max(need)
    }

    /// Records a reservation growth to `target` bytes (absolute), advancing the growth step
    /// (doubled, capped at `max(reservation, RESERVE_BATCH)`). Returns the bytes actually added (for
    /// the engine-wide committed-bytes accounting). The caller (`grow_gc_reservation` or the free
    /// path) has already bounded `target` against the limiter or the abort cap.
    pub(crate) fn grant(&mut self, target: usize) -> usize {
        let old = self.reserved;
        self.reserved = target;
        let step_cap = self.reservation.max(RESERVE_BATCH);
        self.next_grow = self.next_grow.saturating_mul(2).min(step_cap);
        self.reserved.saturating_sub(old)
    }

    /// Places `object` in a free or fresh slot, charging its size against `used`. Only fails if the
    /// handle space is exhausted (the budget is the caller's concern).
    pub(crate) fn alloc(&mut self, object: GcObject) -> Result<u32> {
        let size = object.byte_size();
        if let Some(index) = self.free.pop() {
            debug_assert!(
                self.slots[index as usize].is_none(),
                "reusing a live gc slot"
            );
            // The slot's generation persists from when it was freed — do not reset it.
            self.slots[index as usize] = Some(object);
            self.used = self.used.saturating_add(size);
            return Ok(index);
        }
        // Slot indices must stay below `NULL_REF` (so a stored handle is never the null sentinel),
        // which also keeps them inside the 31-bit `anyref` range.
        if self.slots.len() as u64 >= u64::from(NULL_REF) {
            return Err(Trap::AllocationTooLarge.into());
        }
        let index = self.slots.len() as u32;
        self.slots.push(Some(object));
        self.generations.push(0); // parallel; fresh slot starts at generation 0
        self.used = self.used.saturating_add(size);
        Ok(index)
    }

    /// Allocates a host- or const-eval-built object, bounded by the hard ceiling (these paths have
    /// no run-loop to suspend for a limiter consultation, so they grow `used` up to `limit`, then
    /// trap). A guest collection later reclaims any of these that become unreachable.
    pub(crate) fn alloc_unreserved(&mut self, object: GcObject) -> Result<u32> {
        if !self.can_fit_limit(object.byte_size()) {
            return Err(Trap::AllocationTooLarge.into());
        }
        self.alloc(object)
    }

    /// Traps unless `extra_bytes` fits under the hard ceiling (pre-check before building a large
    /// `array.new*` backing `Vec`, so a hostile element count traps rather than OOM-aborting).
    pub(crate) fn check_capacity(&self, extra_bytes: usize) -> Result<()> {
        if self.can_fit_limit(extra_bytes) {
            Ok(())
        } else {
            Err(Trap::AllocationTooLarge.into())
        }
    }

    /// The object at slot `index`, or `None` if out of range or swept (a guest-/host-flowed handle
    /// must fault, not panic).
    pub(crate) fn get(&self, index: u32) -> Option<&GcObject> {
        self.slots.get(index as usize)?.as_ref()
    }

    pub(crate) fn get_mut(&mut self, index: u32) -> Option<&mut GcObject> {
        self.slots.get_mut(index as usize)?.as_mut()
    }

    /// The current generation of slot `index` (for the host stale-handle check); `None` if out of
    /// range.
    pub(crate) fn generation(&self, index: u32) -> Option<u32> {
        self.generations.get(index as usize).copied()
    }

    /// Marks slot `index` reachable; returns `true` if it was newly marked (so the caller traces
    /// its children exactly once). A dangling/already-swept index is ignored.
    pub(crate) fn mark(&mut self, index: u32) -> bool {
        let i = index as usize;
        // Ignore a dangling/swept index — only live slots are roots.
        if self.slots.get(i).is_none_or(Option::is_none) {
            return false;
        }
        let (word, bit) = (i / 64, 1u64 << (i % 64));
        if word >= self.marks.len() {
            self.marks.resize(word + 1, 0);
        }
        if self.marks[word] & bit != 0 {
            false
        } else {
            self.marks[word] |= bit;
            true
        }
    }

    /// Frees every unmarked live slot (running each freed object's reclamation is the caller's job
    /// for externref payloads — GC structs/arrays own only plain bytes), recycles the freed indices,
    /// and bumps their generation. Clears the mark bitmap for the next collection. `used` is updated.
    pub(crate) fn sweep(&mut self) {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let Some(obj) = slot.as_ref() else {
                continue;
            };
            let marked = self
                .marks
                .get(i / 64)
                .is_some_and(|word| word & (1 << (i % 64)) != 0);
            if !marked {
                self.used = self.used.saturating_sub(obj.byte_size());
                *slot = None;
                self.generations[i] = self.generations[i].wrapping_add(1);
                self.free.push(i as u32);
            }
        }
        self.marks.clear(); // all-zero between collections
    }
}

#[cfg(test)]
#[path = "gc_tests.rs"]
mod tests;
