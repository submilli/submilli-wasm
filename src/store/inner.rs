//! `StoreInner` — the non-generic entity storage the runtime operates on, plus a
//! simple index arena. Public handles (`Memory`/`Global`/… ) are indices into these.

use core::any::Any;
use std::num::NonZeroU64;

use crate::canon::CanonicalTypeId;
use crate::engine::Engine;
use crate::extern_::{Global, Memory, Table, Tag};
use crate::func::Func;
use crate::instance::Instance;
use crate::value::{ExnRef, Rooted, Val};

use super::arena::Arena;
use super::entity::{ExternEntry, ExternRefs};
use super::gc::{
    anyref_handle_slot, anyref_value, decode_anyref_handle, AnyRefHandle, GcHeap, GcObject,
};
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
    tables: Arena<TableEntity>,
    globals: Arena<GlobalEntity>,
    tags: Arena<TagEntity>,
    /// Exception instances backing `exnref` values; `Rooted<ExnRef>` indexes here. Grow-only
    /// (freed on store drop); its `args` are GC roots the tracing collector (#27g) must enumerate.
    exns: Arena<ExnEntity>,
    instances: Arena<InstanceEntity>,
    /// Host payloads backing `externref` values; `Rooted<ExternRef>` indexes here.
    externrefs: ExternRefs,
    /// Managed `struct`/`array` objects; `Rooted<AnyRef>` slot handles index here.
    /// Allocate-only (null collector); reclamation comes with a tracing collector.
    gc: GcHeap,
    /// Canonical type ids of host-allocated GC objects, each pinned with one type-registration
    /// (decref'd on store drop) so a host object outliving its `StructType` handle keeps its type
    /// (#27i; mirrors wasmtime's `gc_host_alloc_types`).
    gc_host_alloc_types: std::collections::HashSet<CanonicalTypeId>,
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
    pending_exception: Option<Rooted<ExnRef>>,
    /// Live wasm frames during a host call, for `WasmBacktrace::capture` (#29d).
    host_frames: Vec<HostFrame>,
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
            tags: Arena::default(),
            exns: Arena::default(),
            instances: Arena::default(),
            externrefs: ExternRefs::default(),
            gc,
            gc_host_alloc_types: std::collections::HashSet::new(),
            fuel: 0,
            fuel_reserve: 0,
            fuel_yield_interval: None,
            epoch_deadline: u64::MAX,
            pending_exception: None,
            host_frames: Vec::new(),
        }
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

    /// `extern.convert_any`: an internal `anyref` becomes an `externref`. A wrapper of a host
    /// extern unwraps to that extern; any other ref is wrapped in a fresh `Internal` entry. A
    /// host-provided externref used in an `any` position (e.g. a `ref.host` argument) is already
    /// external — passed through unchanged.
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

    /// `any.convert_extern`: an `externref` becomes an `anyref`. An internalized entry recovers
    /// its original ref; a host extern is wrapped in a fresh `Extern` GC object. A value already
    /// in the `any` representation (a host ref handed in as `any`) is passed through.
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
        let slot = self.gc.alloc(GcObject::extern_wrapper(idx))?;
        Ok(anyref_value(anyref_handle_slot(slot)))
    }

    /// Allocates a managed `struct`/`array`, returning its heap slot index; traps on exhaustion.
    pub(crate) fn alloc_gc(&mut self, object: GcObject) -> crate::Result<u32> {
        self.gc.alloc(object)
    }

    /// Pins a host-allocated GC object's type for the store's lifetime (one registration per
    /// distinct type), so the object's bare `type_id` stays valid even if the embedder drops its
    /// `StructType`/`ArrayType`. Idempotent per type.
    pub(crate) fn pin_gc_type(&mut self, id: CanonicalTypeId) {
        if self.gc_host_alloc_types.insert(id) {
            self.engine.incref_type(id);
        }
    }

    /// Traps unless an aggregate of `extra_bytes` would still fit under the GC-heap ceiling
    /// (a pre-check for large `array.new*` before its backing `Vec` is built).
    pub(crate) fn gc_check_capacity(&self, extra_bytes: usize) -> crate::Result<()> {
        self.gc.check_capacity(extra_bytes)
    }

    pub(crate) fn gc_object(&self, index: u32) -> Option<&GcObject> {
        self.gc.get(index)
    }

    pub(crate) fn gc_object_mut(&mut self, index: u32) -> Option<&mut GcObject> {
        self.gc.get_mut(index)
    }

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
