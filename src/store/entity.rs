//! Runtime entity *data* owned by the store (the contents of `StoreInner`'s arenas).
//!
//! Kept separate from the public handle types (`Memory`/`Global`/`Table`/`Func`/
//! `Instance`), which live in their own modules — those are the embedder-facing
//! API; these are interpreter internals. Behavior on the handles
//! reads/writes these through `StoreInner`.

use crate::extern_::{Global, Memory, Table};
use crate::func::Func;
use crate::instance::Instance;
use crate::module::Module;
use crate::value::{FuncType, GlobalType, MemoryType, Ref, TableType, Val};

/// Size of a WebAssembly memory page, in bytes.
pub(crate) const PAGE_SIZE: usize = 64 * 1024;
/// Maximum addressable pages for a 32-bit memory (4 GiB).
const MAX_PAGES: u64 = 1 << 16;

/// Runtime data backing a [`Memory`](crate::Memory).
#[derive(Debug)]
pub(crate) struct MemoryEntity {
    pub bytes: Vec<u8>,
    pub ty: MemoryType,
}

/// Runtime data backing a [`Global`](crate::Global).
#[derive(Debug)]
pub(crate) struct GlobalEntity {
    pub value: Val,
    pub ty: GlobalType,
}

/// Runtime data backing a [`Table`](crate::Table).
#[derive(Debug)]
pub(crate) struct TableEntity {
    pub elems: Vec<Ref>,
    pub ty: TableType,
}

/// Runtime data backing a [`Func`](crate::Func): either a defined wasm function
/// (its defining instance + module-space index) or a host function (its dynamic
/// signature + an index into the owning `Store<T>`'s typed `host_funcs`).
#[derive(Debug)]
pub(crate) enum FuncEntity {
    Wasm {
        instance: Instance,
        func_index: u32,
    },
    Host {
        ty: FuncType,
        host_index: u32,
    },
    /// An async host function: signature + index into the store's `async_host_funcs`.
    /// The closure returns a future the async driver awaits before resuming.
    #[cfg(feature = "async")]
    HostAsync {
        ty: FuncType,
        host_index: u32,
    },
}

/// Runtime data backing an [`Instance`](crate::Instance): its resolved index
/// spaces.
#[derive(Debug)]
pub(crate) struct InstanceEntity {
    pub module: Module,
    pub funcs: Vec<Func>,
    pub memories: Vec<Memory>,
    pub globals: Vec<Global>,
    pub tables: Vec<Table>,
    /// Per-data-segment "dropped" flag (one bool per module data segment). Active
    /// segments are marked dropped right after instantiation; `data.drop` marks
    /// passive ones. A `memory.init` from a dropped segment with `len > 0` traps.
    pub dropped_data: Vec<bool>,
    /// Per-element-segment "dropped" flag (one bool per module element segment).
    /// Active/declared segments start dropped; `elem.drop` marks passive ones. A
    /// `table.init` from a dropped segment with `len > 0` traps.
    pub dropped_elems: Vec<bool>,
}

impl MemoryEntity {
    pub(crate) fn new(ty: MemoryType) -> Self {
        let bytes = vec![0; ty.minimum() as usize * PAGE_SIZE];
        MemoryEntity { bytes, ty }
    }

    pub(crate) fn size_pages(&self) -> u64 {
        (self.bytes.len() / PAGE_SIZE) as u64
    }

    /// Grows by `delta` pages; returns the previous page count, or `None` if it
    /// would exceed the declared/architectural maximum.
    pub(crate) fn grow(&mut self, delta: u64) -> Option<u64> {
        let old = self.size_pages();
        let new = old.checked_add(delta)?;
        let max = self.ty.maximum().unwrap_or(MAX_PAGES).min(MAX_PAGES);
        if new > max {
            return None;
        }
        self.bytes.resize(new as usize * PAGE_SIZE, 0);
        Some(old)
    }
}

impl TableEntity {
    pub(crate) fn new(ty: TableType, init: Ref) -> Self {
        let elems = vec![init; ty.minimum() as usize];
        TableEntity { elems, ty }
    }

    pub(crate) fn size(&self) -> u64 {
        self.elems.len() as u64
    }

    pub(crate) fn get(&self, index: u64) -> Option<Ref> {
        self.elems.get(index as usize).cloned()
    }

    pub(crate) fn set(&mut self, index: u64, val: Ref) -> bool {
        match self.elems.get_mut(index as usize) {
            Some(slot) => {
                *slot = val;
                true
            }
            None => false,
        }
    }

    /// Grows by `delta` elements (filled with `init`); returns the previous size,
    /// or `None` if it would exceed the declared maximum.
    pub(crate) fn grow(&mut self, delta: u64, init: Ref) -> Option<u64> {
        let old = self.size();
        let new = old.checked_add(delta)?;
        let max = self.ty.maximum().unwrap_or(u64::from(u32::MAX));
        if new > max {
            return None;
        }
        self.elems.resize(new as usize, init);
        Some(old)
    }

    /// Fills `len` elements from `dst` with `val`; returns false if out of bounds.
    pub(crate) fn fill(&mut self, dst: u64, val: Ref, len: u64) -> bool {
        let end = match dst.checked_add(len) {
            Some(end) if end <= self.size() => end,
            _ => return false,
        };
        for i in dst..end {
            self.elems[i as usize] = val.clone();
        }
        true
    }
}
