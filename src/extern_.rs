//! External entities: `Memory`, `Global`, `Table`, and the `Extern` enum.
//!
//! These are the public, store-bound handles; their data lives in `StoreInner`
//! (see `store::entity`). Methods resolve the entity through the store and delegate.

use crate::func::Func;
use crate::store::{
    AsContext, AsContextMut, GlobalEntity, MemoryEntity, StoreContext, StoreContextMut,
    TableEntity, TagEntity,
};
use crate::value::{GlobalType, MemoryType, Mutability, Ref, TableType, TagType, Val, ValType};
use crate::{Error, Result};

/// An external value importable into / exportable from a module.
#[derive(Clone, Debug)]
pub enum Extern {
    Func(Func),
    Global(Global),
    Table(Table),
    Memory(Memory),
    Tag(Tag),
}

impl From<Func> for Extern {
    fn from(f: Func) -> Extern {
        Extern::Func(f)
    }
}

impl From<Global> for Extern {
    fn from(g: Global) -> Extern {
        Extern::Global(g)
    }
}

impl From<Table> for Extern {
    fn from(t: Table) -> Extern {
        Extern::Table(t)
    }
}

impl From<Memory> for Extern {
    fn from(m: Memory) -> Extern {
        Extern::Memory(m)
    }
}

impl From<Tag> for Extern {
    fn from(t: Tag) -> Extern {
        Extern::Tag(t)
    }
}

/// A linear memory instance.
#[derive(Copy, Clone, Debug)]
pub struct Memory {
    pub(crate) index: u32,
}

impl Memory {
    pub fn new(mut store: impl AsContextMut, ty: MemoryType) -> Result<Memory> {
        let mut ctx = store.as_context_mut();
        let s = ctx.store_mut();
        if !s.limiter_allows_memory(ty.minimum(), ty.maximum())? {
            return Err(Error::msg("memory minimum size exceeds the store limit"));
        }
        Ok(s.inner.alloc_memory(MemoryEntity::new(ty)))
    }

    /// Async sibling of [`new`](Memory::new): awaits an async resource limiter on the
    /// initial-allocation check.
    #[cfg(feature = "async")]
    pub async fn new_async(mut store: impl AsContextMut, ty: MemoryType) -> Result<Memory> {
        let mut ctx = store.as_context_mut();
        let s = ctx.store_mut();
        if !s
            .limiter_allows_memory_async(ty.minimum(), ty.maximum())
            .await?
        {
            return Err(Error::msg("memory minimum size exceeds the store limit"));
        }
        Ok(s.inner.alloc_memory(MemoryEntity::new(ty)))
    }

    pub fn ty(&self, store: impl AsContext) -> MemoryType {
        store.as_context().inner().memory(*self).ty.clone()
    }

    pub fn data<'a, T: 'static>(&self, store: impl Into<StoreContext<'a, T>>) -> &'a [u8] {
        let ctx: StoreContext<'a, T> = store.into();
        ctx.inner().memory(*self).bytes.as_slice()
    }

    pub fn data_mut<'a, T: 'static>(
        &self,
        store: impl Into<StoreContextMut<'a, T>>,
    ) -> &'a mut [u8] {
        let ctx: StoreContextMut<'a, T> = store.into();
        ctx.into_inner_mut().memory_mut(*self).bytes.as_mut_slice()
    }

    pub fn data_ptr(&self, store: impl AsContext) -> *mut u8 {
        store
            .as_context()
            .inner()
            .memory(*self)
            .bytes
            .as_ptr()
            .cast_mut()
    }

    pub fn data_size(&self, store: impl AsContext) -> usize {
        store.as_context().inner().memory(*self).bytes.len()
    }

    pub fn size(&self, store: impl AsContext) -> u64 {
        store.as_context().inner().memory(*self).size_pages()
    }

    pub fn grow(&self, mut store: impl AsContextMut, delta: u64) -> Result<u64> {
        match store
            .as_context_mut()
            .store_mut()
            .grow_memory(*self, delta)?
        {
            Some(old) => Ok(old),
            None => Err(Error::msg("failed to grow memory")),
        }
    }

    pub fn read(
        &self,
        store: impl AsContext,
        offset: usize,
        buffer: &mut [u8],
    ) -> core::result::Result<(), MemoryAccessError> {
        let ctx = store.as_context();
        let bytes = &ctx.inner().memory(*self).bytes;
        let end = offset
            .checked_add(buffer.len())
            .ok_or_else(MemoryAccessError::oob)?;
        let slice = bytes.get(offset..end).ok_or_else(MemoryAccessError::oob)?;
        buffer.copy_from_slice(slice);
        Ok(())
    }

    pub fn write(
        &self,
        mut store: impl AsContextMut,
        offset: usize,
        buffer: &[u8],
    ) -> core::result::Result<(), MemoryAccessError> {
        let mut ctx = store.as_context_mut();
        let bytes = &mut ctx.inner_mut().memory_mut(*self).bytes;
        let end = offset
            .checked_add(buffer.len())
            .ok_or_else(MemoryAccessError::oob)?;
        let slice = bytes
            .get_mut(offset..end)
            .ok_or_else(MemoryAccessError::oob)?;
        slice.copy_from_slice(buffer);
        Ok(())
    }
}

/// Error returned by [`Memory::read`]/[`Memory::write`] on an out-of-bounds access.
#[derive(Debug)]
#[non_exhaustive]
pub struct MemoryAccessError {
    _private: (),
}

impl MemoryAccessError {
    pub(crate) fn oob() -> Self {
        MemoryAccessError { _private: () }
    }
}

impl core::fmt::Display for MemoryAccessError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("out of bounds memory access")
    }
}

impl std::error::Error for MemoryAccessError {}

/// A global variable instance.
#[derive(Copy, Clone, Debug)]
pub struct Global {
    pub(crate) index: u32,
}

impl Global {
    pub fn new(mut store: impl AsContextMut, ty: GlobalType, val: Val) -> Result<Global> {
        Ok(store
            .as_context_mut()
            .inner_mut()
            .alloc_global(GlobalEntity { value: val, ty }))
    }

    pub fn ty(&self, store: impl AsContext) -> GlobalType {
        store.as_context().inner().global(*self).ty.clone()
    }

    pub fn get(&self, mut store: impl AsContextMut) -> Val {
        store.as_context_mut().inner().global(*self).value
    }

    pub fn set(&self, mut store: impl AsContextMut, val: Val) -> Result<()> {
        let mut ctx = store.as_context_mut();
        let entity = ctx.inner_mut().global_mut(*self);
        if entity.ty.mutability() == Mutability::Const {
            return Err(Error::msg("cannot set the value of a const global"));
        }
        if !val_matches(&val, entity.ty.content()) {
            return Err(Error::msg("global type mismatch"));
        }
        entity.value = val;
        Ok(())
    }
}

/// A table instance.
#[derive(Copy, Clone, Debug)]
pub struct Table {
    pub(crate) index: u32,
}

impl Table {
    pub fn new(mut store: impl AsContextMut, ty: TableType, init: Ref) -> Result<Table> {
        let mut ctx = store.as_context_mut();
        let s = ctx.store_mut();
        if !s.limiter_allows_table(ty.minimum(), ty.maximum())? {
            return Err(Error::msg("table minimum size exceeds the store limit"));
        }
        Ok(s.inner.alloc_table(TableEntity::new(ty, init)))
    }

    /// Async sibling of [`new`](Table::new): awaits an async resource limiter on the
    /// initial-allocation check.
    #[cfg(feature = "async")]
    pub async fn new_async(
        mut store: impl AsContextMut,
        ty: TableType,
        init: Ref,
    ) -> Result<Table> {
        let mut ctx = store.as_context_mut();
        let s = ctx.store_mut();
        if !s
            .limiter_allows_table_async(ty.minimum(), ty.maximum())
            .await?
        {
            return Err(Error::msg("table minimum size exceeds the store limit"));
        }
        Ok(s.inner.alloc_table(TableEntity::new(ty, init)))
    }

    pub fn ty(&self, store: impl AsContext) -> TableType {
        store.as_context().inner().table(*self).ty.clone()
    }

    pub fn get(&self, mut store: impl AsContextMut, index: u64) -> Option<Ref> {
        store.as_context_mut().inner().table(*self).get(index)
    }

    pub fn set(&self, mut store: impl AsContextMut, index: u64, val: Ref) -> Result<()> {
        if store
            .as_context_mut()
            .inner_mut()
            .table_mut(*self)
            .set(index, val)
        {
            Ok(())
        } else {
            Err(Error::msg("table index out of bounds"))
        }
    }

    pub fn size(&self, store: impl AsContext) -> u64 {
        store.as_context().inner().table(*self).size()
    }

    pub fn grow(&self, mut store: impl AsContextMut, delta: u64, init: Ref) -> Result<u64> {
        match store
            .as_context_mut()
            .store_mut()
            .grow_table(*self, delta, init)?
        {
            Some(old) => Ok(old),
            None => Err(Error::msg("failed to grow table")),
        }
    }

    pub fn fill(&self, mut store: impl AsContextMut, dst: u64, val: Ref, len: u64) -> Result<()> {
        if store
            .as_context_mut()
            .inner_mut()
            .table_mut(*self)
            .fill(dst, val, len)
        {
            Ok(())
        } else {
            Err(Error::msg("table fill out of bounds"))
        }
    }
}

/// An exception tag instance. Identity is the store slot (store address): two instantiations of
/// the same module produce distinct tags, while an imported tag is shared. Catch-clause matching
/// (#28e) compares these handles; the `ty` is the exception signature.
#[derive(Copy, Clone, Debug)]
pub struct Tag {
    pub(crate) index: u32,
}

impl Tag {
    pub fn new(mut store: impl AsContextMut, ty: &TagType) -> Result<Tag> {
        Ok(store
            .as_context_mut()
            .inner_mut()
            .alloc_tag(TagEntity { ty: ty.clone() }))
    }

    pub fn ty(&self, store: impl AsContext) -> TagType {
        store.as_context().inner().tag(*self).ty.clone()
    }
}

/// Coarse value/type compatibility check (numeric exact; any reference matches a
/// reference type). Precise reference-type checking is left to validation.
pub(crate) fn val_matches(val: &Val, ty: &ValType) -> bool {
    matches!(
        (val, ty),
        (Val::I32(_), ValType::I32)
            | (Val::I64(_), ValType::I64)
            | (Val::F32(_), ValType::F32)
            | (Val::F64(_), ValType::F64)
            | (Val::V128(_), ValType::V128)
            | (
                Val::FuncRef(_) | Val::ExternRef(_) | Val::AnyRef(_) | Val::ExnRef(_),
                ValType::Ref(_),
            )
    )
}

#[cfg(test)]
#[path = "extern_tests.rs"]
mod tests;
