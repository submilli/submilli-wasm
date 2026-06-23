//! `StoreInner` — the non-generic entity storage the runtime operates on, plus a
//! simple index arena. Public handles (`Memory`/`Global`/… ) are indices into these.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::canon::CanonicalTypeId;
use crate::engine::Engine;
use crate::extern_::{Global, Memory, Table, Tag};
use crate::func::Func;
use crate::instance::Instance;
use crate::value::{ExnRef, Rooted};

use super::arena::Arena;
use super::entity::ExternRefs;
use super::gc::GcHeap;
use super::{
    ExnEntity, FuncEntity, GlobalEntity, HostFrame, InstanceEntity, MemoryEntity, TableEntity,
    TagEntity,
};

/// Outcome of charging one unit of fuel (see [`StoreInner::consume_fuel_step`]).
pub(crate) enum FuelStep {
    /// Fuel was available and charged; keep running.
    Ran,
    /// Active fuel is exhausted but reserve remains: the async driver should yield
    /// to the executor, then [`refuel_from_reserve`](StoreInner::refuel_from_reserve).
    NeedYield,
    /// No fuel left at all → `Trap::OutOfFuel`.
    Exhausted,
}

/// Non-generic storage for all of a store's runtime entities.
#[derive(Debug)]
pub(crate) struct StoreInner {
    engine: Engine,
    funcs: Arena<FuncEntity>,
    memories: Arena<MemoryEntity>,
    pub(super) tables: Arena<TableEntity>,
    pub(super) globals: Arena<GlobalEntity>,
    tags: Arena<TagEntity>,
    /// Exception instances backing `exnref` values; `Rooted<ExnRef>` indexes here. Grow-only
    /// (freed on store drop); its `args` are GC roots the tracing collector (#27g) enumerates
    /// transitively (the exn arena itself is not reclaimed yet — see `gc_collect`).
    pub(super) exns: Arena<ExnEntity>,
    pub(super) instances: Arena<InstanceEntity>,
    /// Host payloads backing `externref` values; `Rooted<ExternRef>` indexes here. Traced for GC
    /// reachability; box reclamation is a follow-up (see `gc_collect`).
    pub(super) externrefs: ExternRefs,
    /// Managed `struct`/`array` objects; `Rooted<AnyRef>` slot handles index here. Reclaimed by the
    /// tracing collector (`Collector::Auto`/`MarkSweep`); allocate-only under `Collector::Null`.
    pub(crate) gc: GcHeap,
    /// Live host-held GC roots (`Rooted` handed to the embedder via the GC host API, scoped by
    /// `RootScope`). Enumerated at collection so a host reference held across a guest collection
    /// keeps its object alive (#27g). Each is `(handle, hierarchy)`.
    pub(super) gc_roots: Vec<(u32, crate::canon::RefKind)>,
    /// Canonical type ids of host-allocated GC objects, each pinned with one type-registration
    /// (decref'd on store drop) so a host object outliving its `StructType` handle keeps its type
    /// (#27i; mirrors wasmtime's `gc_host_alloc_types`).
    pub(super) gc_host_alloc_types: std::collections::HashSet<CanonicalTypeId>,
    /// Active fuel — charged per op (meaningful only when `engine.consume_fuel()`).
    /// With an async yield interval this is the current slice; total = `fuel + fuel_reserve`.
    fuel: u64,
    /// Fuel held back from the active slice, released on each async yield.
    fuel_reserve: u64,
    /// Async fuel-yield granularity: yield to the executor every this-many units.
    fuel_yield_interval: Option<NonZeroU64>,
    /// Absolute epoch value at/after which execution interrupts (`u64::MAX` = none).
    epoch_deadline: u64,
    /// Exception surfaced from the last call / a host `throw`; taken via `take_pending_exception`.
    pub(super) pending_exception: Option<Rooted<ExnRef>>,
    /// Live wasm frames during a host call, for `WasmBacktrace::capture` (#29d).
    host_frames: Vec<HostFrame>,
    /// This store's GC-request mailbox (the engine holds a `Weak`). The engine posts to it under
    /// engine-wide GC pressure; the run loop reads-and-clears it at a back-edge and self-collects.
    gc_request: Arc<AtomicBool>,
}

impl StoreInner {
    pub(crate) fn new(engine: Engine) -> Self {
        let gc = GcHeap::new(engine.is_collecting(), engine.gc_heap_reservation());
        let gc_request = engine.register_gc_request();
        StoreInner {
            engine,
            funcs: Arena::default(),
            memories: Arena::default(),
            tables: Arena::default(),
            globals: Arena::default(),
            tags: Arena::default(),
            exns: Arena::default(),
            instances: Arena::default(),
            externrefs: ExternRefs::default(),
            gc,
            gc_roots: Vec::new(),
            gc_host_alloc_types: std::collections::HashSet::new(),
            fuel: 0,
            fuel_reserve: 0,
            fuel_yield_interval: None,
            epoch_deadline: u64::MAX,
            pending_exception: None,
            host_frames: Vec::new(),
            gc_request,
        }
    }

    /// Reads and clears this store's GC-request mailbox: `true` ⇒ the engine asked it to collect
    /// (under engine-wide pressure) since the last check. Clearing affects only *this* store's
    /// mailbox, so servicing it doesn't suppress the request for the engine's other stores.
    pub(crate) fn take_gc_request(&self) -> bool {
        self.gc_request.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn set_pending_exception(&mut self, exn: Rooted<ExnRef>) {
        self.pending_exception = Some(exn);
    }

    pub(crate) fn take_pending_exception(&mut self) -> Option<Rooted<ExnRef>> {
        self.pending_exception.take()
    }

    pub(crate) fn host_frames(&self) -> &[HostFrame] {
        &self.host_frames
    }

    /// Swaps in a host-call frame snapshot, returning the previous one to restore (re-entrant
    /// host→wasm→host must not clobber the outer frame's snapshot).
    pub(crate) fn swap_host_frames(&mut self, frames: Vec<HostFrame>) -> Vec<HostFrame> {
        std::mem::replace(&mut self.host_frames, frames)
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub(crate) fn fuel(&self) -> u64 {
        self.fuel + self.fuel_reserve
    }

    /// Sets total fuel, split into active + reserve per the yield interval (none ⇒ all active).
    pub(crate) fn set_fuel(&mut self, total: u64) {
        let active = self
            .fuel_yield_interval
            .map_or(total, |i| total.min(i.get()));
        self.fuel = active;
        self.fuel_reserve = total - active;
    }

    /// Sets the async fuel-yield interval, then re-splits the current total fuel.
    pub(crate) fn set_fuel_yield_interval(&mut self, interval: Option<u64>) {
        self.fuel_yield_interval = interval.and_then(NonZeroU64::new);
        self.set_fuel(self.fuel());
    }

    /// Charges one unit; `NeedYield` if only reserve remains (yield point), `Exhausted` if none.
    pub(crate) fn consume_fuel_step(&mut self) -> FuelStep {
        if self.fuel > 0 {
            self.fuel -= 1;
            FuelStep::Ran
        } else if self.fuel_reserve > 0 {
            FuelStep::NeedYield
        } else {
            FuelStep::Exhausted
        }
    }

    /// Moves one interval's fuel from reserve into the active slice (after a yield); no-op if unset.
    pub(crate) fn refuel_from_reserve(&mut self) {
        let take = self
            .fuel_yield_interval
            .map_or(0, NonZeroU64::get)
            .min(self.fuel_reserve);
        self.fuel += take;
        self.fuel_reserve -= take;
    }

    pub(crate) fn set_epoch_deadline(&mut self, deadline: u64) {
        self.epoch_deadline = deadline;
    }

    pub(crate) fn epoch_deadline_reached(&self) -> bool {
        self.engine.current_epoch() >= self.epoch_deadline
    }

    pub(crate) fn alloc_func(&mut self, entity: FuncEntity) -> Func {
        Func {
            index: self.funcs.alloc(entity),
        }
    }

    pub(crate) fn func(&self, handle: Func) -> &FuncEntity {
        self.funcs.get(handle.index)
    }

    pub(crate) fn alloc_memory(&mut self, entity: MemoryEntity) -> Memory {
        Memory {
            index: self.memories.alloc(entity),
        }
    }

    pub(crate) fn memory(&self, handle: Memory) -> &MemoryEntity {
        self.memories.get(handle.index)
    }

    pub(crate) fn memory_mut(&mut self, handle: Memory) -> &mut MemoryEntity {
        self.memories.get_mut(handle.index)
    }

    pub(crate) fn alloc_table(&mut self, entity: TableEntity) -> Table {
        Table {
            index: self.tables.alloc(entity),
        }
    }

    pub(crate) fn table(&self, handle: Table) -> &TableEntity {
        self.tables.get(handle.index)
    }

    pub(crate) fn table_mut(&mut self, handle: Table) -> &mut TableEntity {
        self.tables.get_mut(handle.index)
    }

    pub(crate) fn alloc_global(&mut self, entity: GlobalEntity) -> Global {
        Global {
            index: self.globals.alloc(entity),
        }
    }

    pub(crate) fn global(&self, handle: Global) -> &GlobalEntity {
        self.globals.get(handle.index)
    }

    pub(crate) fn global_mut(&mut self, handle: Global) -> &mut GlobalEntity {
        self.globals.get_mut(handle.index)
    }

    pub(crate) fn alloc_tag(&mut self, entity: TagEntity) -> Tag {
        Tag {
            index: self.tags.alloc(entity),
        }
    }

    pub(crate) fn tag(&self, handle: Tag) -> &TagEntity {
        self.tags.get(handle.index)
    }

    /// Allocates an exception instance, returning its `exnref` handle.
    pub(crate) fn alloc_exn(&mut self, entity: ExnEntity) -> Rooted<ExnRef> {
        Rooted::from_raw(self.exns.alloc(entity))
    }

    pub(crate) fn exn(&self, handle: Rooted<ExnRef>) -> &ExnEntity {
        self.exns.get(handle.raw())
    }

    pub(crate) fn exn_mut(&mut self, handle: Rooted<ExnRef>) -> &mut ExnEntity {
        self.exns.get_mut(handle.raw())
    }

    /// The handle the *next* [`alloc_instance`](Self::alloc_instance) will return,
    /// without allocating. Lets instantiation build `FuncEntity`s that point back
    /// at the instance before it exists (see `instance::init`). Only valid while no
    /// other instance is allocated in between (we hold `&mut StoreInner` throughout).
    pub(crate) fn reserve_instance(&self) -> Instance {
        Instance {
            index: self.instances.len(),
        }
    }

    pub(crate) fn alloc_instance(&mut self, entity: InstanceEntity) -> Instance {
        Instance {
            index: self.instances.alloc(entity),
        }
    }

    pub(crate) fn instance(&self, handle: Instance) -> &InstanceEntity {
        self.instances.get(handle.index)
    }

    /// Fallible instance lookup — `None` for an unregistered handle (synthetic test executions).
    pub(crate) fn try_instance(&self, handle: Instance) -> Option<&InstanceEntity> {
        self.instances.get_opt(handle.index)
    }

    pub(crate) fn memory_count(&self) -> usize {
        self.memories.len() as usize
    }

    pub(crate) fn table_count(&self) -> usize {
        self.tables.len() as usize
    }

    pub(crate) fn instance_count(&self) -> usize {
        self.instances.len() as usize
    }

    pub(crate) fn instance_mut(&mut self, handle: Instance) -> &mut InstanceEntity {
        self.instances.get_mut(handle.index)
    }
}

impl Drop for StoreInner {
    fn drop(&mut self) {
        // Release this store's GC reservation from the engine-wide committed total.
        self.engine.sub_gc_committed(self.gc.reserved());
        // Release the type-registrations pinned by host GC allocations.
        for &id in &self.gc_host_alloc_types {
            self.engine.decref_type(id);
        }
    }
}

#[cfg(test)]
#[path = "exn_tests.rs"]
mod exn_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{GlobalType, Mutability, Val, ValType};

    #[test]
    fn global_arena_round_trip() {
        let mut inner = StoreInner::new(Engine::default());
        let handle = inner.alloc_global(GlobalEntity {
            value: Val::I32(7),
            ty: GlobalType::new(ValType::I32, Mutability::Var),
        });
        assert_eq!(inner.global(handle).value.unwrap_i32(), 7);
        inner.global_mut(handle).value = Val::I32(9);
        assert_eq!(inner.global(handle).value.unwrap_i32(), 9);
    }
}
