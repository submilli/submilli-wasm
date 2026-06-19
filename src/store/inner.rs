//! `StoreInner` — the non-generic entity storage the runtime operates on, plus a
//! simple index arena. Public handles (`Memory`/`Global`/… ) are indices into these.

use core::any::Any;
use std::num::NonZeroU64;

use crate::engine::Engine;
use crate::extern_::{Global, Memory, Table};
use crate::func::Func;
use crate::instance::Instance;

use super::gc::{GcHeap, GcObject};
use super::{FuncEntity, GlobalEntity, InstanceEntity, MemoryEntity, TableEntity};

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

/// An index-keyed arena; a handle is a `u32` index into the backing `Vec`.
#[derive(Debug)]
pub(crate) struct Arena<E>(Vec<E>);

impl<E> Default for Arena<E> {
    fn default() -> Self {
        Arena(Vec::new())
    }
}

impl<E> Arena<E> {
    fn alloc(&mut self, entity: E) -> u32 {
        let index = self.0.len() as u32;
        self.0.push(entity);
        index
    }

    fn get(&self, index: u32) -> &E {
        &self.0[index as usize]
    }

    fn get_mut(&mut self, index: u32) -> &mut E {
        &mut self.0[index as usize]
    }

    fn len(&self) -> u32 {
        self.0.len() as u32
    }
}

/// Store-side arena of host `externref` payloads. Grow-only for now: entries live for
/// the store's lifetime (no reclamation until a tracing collector, whose host-root
/// enumeration hook lands then). `Box<dyn Any>` isn't `Debug`, hence the manual impl.
#[derive(Default)]
struct ExternRefs(Vec<Box<dyn Any + Send + Sync>>);

impl core::fmt::Debug for ExternRefs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExternRefs")
            .field("len", &self.0.len())
            .finish()
    }
}

/// Non-generic storage for all of a store's runtime entities.
#[derive(Debug)]
pub(crate) struct StoreInner {
    engine: Engine,
    funcs: Arena<FuncEntity>,
    memories: Arena<MemoryEntity>,
    tables: Arena<TableEntity>,
    globals: Arena<GlobalEntity>,
    instances: Arena<InstanceEntity>,
    /// Host payloads backing `externref` values; `Rooted<ExternRef>` indexes here.
    externrefs: ExternRefs,
    /// Managed `struct`/`array` objects; `Rooted<AnyRef>` slot handles index here.
    /// Allocate-only (null collector); reclamation comes with a tracing collector.
    gc: GcHeap,
    /// Active fuel — charged per op (meaningful only when `engine.consume_fuel()`).
    /// With an async yield interval this is the current slice; total = `fuel + fuel_reserve`.
    fuel: u64,
    /// Fuel held back from the active slice, released on each async yield.
    fuel_reserve: u64,
    /// Async fuel-yield granularity: yield to the executor every this-many units.
    fuel_yield_interval: Option<NonZeroU64>,
    /// Absolute epoch value at/after which execution interrupts (`u64::MAX` = none).
    epoch_deadline: u64,
}

impl StoreInner {
    pub(crate) fn new(engine: Engine) -> Self {
        let gc = GcHeap::new(engine.gc_memory_threshold());
        StoreInner {
            engine,
            funcs: Arena::default(),
            memories: Arena::default(),
            tables: Arena::default(),
            globals: Arena::default(),
            instances: Arena::default(),
            externrefs: ExternRefs::default(),
            gc,
            fuel: 0,
            fuel_reserve: 0,
            fuel_yield_interval: None,
            epoch_deadline: u64::MAX,
        }
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Stores a host `externref` payload, returning its index (held by `Rooted<ExternRef>`).
    pub(crate) fn alloc_externref(&mut self, value: Box<dyn Any + Send + Sync>) -> u32 {
        let index = self.externrefs.0.len() as u32;
        self.externrefs.0.push(value);
        index
    }

    /// The payload behind an `externref` index, if present.
    pub(crate) fn externref(&self, index: u32) -> Option<&(dyn Any + Send + Sync)> {
        self.externrefs.0.get(index as usize).map(AsRef::as_ref)
    }

    /// Allocates a managed `struct`/`array` object, returning its heap slot index (held by a
    /// `Rooted<AnyRef>`). Traps on heap exhaustion. Wired by the host GC API and the
    /// aggregate instructions.
    #[allow(dead_code)]
    pub(crate) fn alloc_gc(&mut self, object: GcObject) -> crate::Result<u32> {
        self.gc.alloc(object)
    }

    /// The managed object at a heap slot index, if present.
    #[allow(dead_code)]
    pub(crate) fn gc_object(&self, index: u32) -> Option<&GcObject> {
        self.gc.get(index)
    }

    /// Mutable access to the managed object at a heap slot index, if present.
    #[allow(dead_code)]
    pub(crate) fn gc_object_mut(&mut self, index: u32) -> Option<&mut GcObject> {
        self.gc.get_mut(index)
    }

    /// Total remaining fuel (active slice + reserve).
    pub(crate) fn fuel(&self) -> u64 {
        self.fuel + self.fuel_reserve
    }

    /// Sets total fuel, splitting it into the active slice + reserve per the yield
    /// interval (no interval ⇒ all active, reserve 0 — identical to plain metering).
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

    /// Charges one unit from the active slice. `NeedYield` when the slice is empty
    /// but reserve remains (async yield point); `Exhausted` when no fuel is left.
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

    /// Moves up to one interval's worth of fuel from reserve into the active slice
    /// (after an async yield). No-op without an interval.
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

    /// True once the engine's epoch has reached this store's deadline.
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
