//! Handle → entity accessors for [`StoreInner`]: bounds- and provenance-checked (#34) reads
//! of the store's arenas, split from `inner.rs` for the file-size cap. All are `#[inline]` —
//! they sit on the interpreter's per-op paths, and the dispatch loop monomorphizes in the
//! consumer crate (cross-crate inlining needs the attribute).

use super::entity::{
    FuncEntity, GlobalEntity, InstanceEntity, MemoryEntity, TableEntity, TagEntity,
};
use super::inner::StoreInner;
use crate::extern_::{Global, Memory, Table, Tag};
use crate::func::Func;
use crate::instance::Instance;

impl StoreInner {
    #[inline]
    pub(crate) fn func(&self, handle: Func) -> &FuncEntity {
        self.check_handle(handle.store);
        self.funcs.get(handle.index)
    }

    #[inline]
    pub(crate) fn memory(&self, handle: Memory) -> &MemoryEntity {
        self.check_handle(handle.store);
        self.memories.get(handle.index)
    }

    #[inline]
    pub(crate) fn memory_mut(&mut self, handle: Memory) -> &mut MemoryEntity {
        self.check_handle(handle.store);
        self.memories.get_mut(handle.index)
    }

    #[inline]
    pub(crate) fn table(&self, handle: Table) -> &TableEntity {
        self.check_handle(handle.store);
        self.tables.get(handle.index)
    }

    #[inline]
    pub(crate) fn table_mut(&mut self, handle: Table) -> &mut TableEntity {
        self.check_handle(handle.store);
        self.tables.get_mut(handle.index)
    }

    #[inline]
    pub(crate) fn global(&self, handle: Global) -> &GlobalEntity {
        self.check_handle(handle.store);
        self.globals.get(handle.index)
    }

    #[inline]
    pub(crate) fn global_mut(&mut self, handle: Global) -> &mut GlobalEntity {
        self.check_handle(handle.store);
        self.globals.get_mut(handle.index)
    }

    #[inline]
    pub(crate) fn tag(&self, handle: Tag) -> &TagEntity {
        self.check_handle(handle.store);
        self.tags.get(handle.index)
    }

    #[inline]
    pub(crate) fn instance(&self, handle: Instance) -> &InstanceEntity {
        self.check_handle(handle.store);
        self.instances.get(handle.index)
    }

    #[inline]
    pub(crate) fn instance_mut(&mut self, handle: Instance) -> &mut InstanceEntity {
        self.check_handle(handle.store);
        self.instances.get_mut(handle.index)
    }
}
