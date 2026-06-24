//! A **reclaimable** index-keyed arena: like [`Arena`](super::arena::Arena) but with a free-list and
//! per-slot generation so the mark-sweep collector (#27g) can free unreachable entries and recycle
//! their indices. Backs the `externref` and `exn` arenas, which (unlike the store-lifetime entity
//! arenas) hold guest-reachable garbage. Mirrors the GC object heap ([`super::gc::GcHeap`]): a freed
//! slot becomes `None`, its generation is bumped (so a host handle that outlived its entry faults
//! rather than aliasing a reused slot), and its index is pushed onto the free-list for reuse.

use std::collections::HashSet;

/// A reclaimable arena of `E`. A handle is a `u32` slot index; the parallel `generations` vector
/// carries the stale-handle check. `used`-byte accounting lives on the GC heap (the caller charges
/// and credits it), so this type stays a pure slot store.
pub(crate) struct ReclaimArena<E> {
    /// The entries, `None` for a freed (swept) slot.
    slots: Vec<Option<E>>,
    /// Per-slot reuse counter, parallel to `slots`; bumped on free (persists across reuse) so a
    /// captured-generation handle faults after its entry is collected. See [`Self::generation`].
    generations: Vec<u32>,
    /// Freed slot indices available for reuse (LIFO).
    free: Vec<u32>,
}

impl<E> Default for ReclaimArena<E> {
    fn default() -> Self {
        ReclaimArena {
            slots: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
        }
    }
}

// Manual `Debug` (no `E: Debug` bound) so an arena of non-`Debug` entries (e.g. `Box<dyn Any>`
// externref payloads) still lets `StoreInner` derive `Debug`.
impl<E> core::fmt::Debug for ReclaimArena<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ReclaimArena")
            .field("slots", &self.slots.len())
            .field("free", &self.free.len())
            .finish_non_exhaustive()
    }
}

impl<E> ReclaimArena<E> {
    /// Places `entity` in a freed slot (reusing its index, *keeping* its generation) or a fresh one,
    /// returning the slot index.
    pub(crate) fn alloc(&mut self, entity: E) -> u32 {
        if let Some(index) = self.free.pop() {
            // The slot's generation persists from when it was freed — do not reset it.
            self.slots[index as usize] = Some(entity);
            return index;
        }
        let index = self.slots.len() as u32;
        self.slots.push(Some(entity));
        self.generations.push(0);
        index
    }

    /// The entry at `index`, or `None` if out of range or swept.
    pub(crate) fn get(&self, index: u32) -> Option<&E> {
        self.slots.get(index as usize)?.as_ref()
    }

    /// Mutable sibling of [`get`](Self::get).
    pub(crate) fn get_mut(&mut self, index: u32) -> Option<&mut E> {
        self.slots.get_mut(index as usize)?.as_mut()
    }

    /// The current generation of slot `index` (for the host stale-handle check); `None` if out of
    /// range.
    pub(crate) fn generation(&self, index: u32) -> Option<u32> {
        self.generations.get(index as usize).copied()
    }

    /// Frees every live slot **not** in `live`, bumping its generation and recycling its index, and
    /// returns the total bytes freed (summed via `byte_of`) so the caller can credit the GC budget.
    /// `live` is the collector's visited set for this arena (#27g).
    pub(crate) fn sweep(&mut self, live: &HashSet<u32>, byte_of: impl Fn(&E) -> usize) -> usize {
        let mut freed_bytes = 0;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let Some(entry) = slot.as_ref() else {
                continue;
            };
            if !live.contains(&(i as u32)) {
                freed_bytes += byte_of(entry);
                *slot = None;
                self.generations[i] = self.generations[i].wrapping_add(1);
                self.free.push(i as u32);
            }
        }
        freed_bytes
    }
}
