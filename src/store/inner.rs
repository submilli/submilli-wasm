//! `StoreInner` — the non-generic entity storage the runtime operates on, plus a
//! simple index arena. Public handles (`Memory`/`Global`/… ) are indices into these.

use crate::engine::Engine;
use crate::extern_::{Global, Memory, Table};
use crate::func::Func;
use crate::instance::Instance;

use super::{FuncEntity, GlobalEntity, InstanceEntity, MemoryEntity, TableEntity};

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

/// Non-generic storage for all of a store's runtime entities.
#[derive(Debug)]
pub(crate) struct StoreInner {
    engine: Engine,
    funcs: Arena<FuncEntity>,
    memories: Arena<MemoryEntity>,
    tables: Arena<TableEntity>,
    globals: Arena<GlobalEntity>,
    instances: Arena<InstanceEntity>,
    /// Remaining fuel (meaningful only when `engine.consume_fuel()`).
    fuel: u64,
    /// Absolute epoch value at/after which execution interrupts (`u64::MAX` = none).
    epoch_deadline: u64,
}

impl StoreInner {
    pub(crate) fn new(engine: Engine) -> Self {
        StoreInner {
            engine,
            funcs: Arena::default(),
            memories: Arena::default(),
            tables: Arena::default(),
            globals: Arena::default(),
            instances: Arena::default(),
            fuel: 0,
            epoch_deadline: u64::MAX,
        }
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub(crate) fn fuel(&self) -> u64 {
        self.fuel
    }

    pub(crate) fn set_fuel(&mut self, fuel: u64) {
        self.fuel = fuel;
    }

    /// Charges one unit of fuel; returns false (without underflowing) if empty.
    pub(crate) fn try_consume_fuel(&mut self) -> bool {
        if self.fuel == 0 {
            false
        } else {
            self.fuel -= 1;
            true
        }
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
