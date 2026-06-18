//! GC reference handles. `externref` is real (a store-side host-payload arena, #26c);
//! the managed-GC refs (`anyref`/struct/array/exn) stay stubs until Phase 5.

use core::any::Any;
use core::marker::PhantomData;

use crate::store::{AsContext, AsContextMut, StoreContext, StoreContextMut};
use crate::value::gc_type::{ArrayType, StructType};
use crate::value::Val;
use crate::Result;

/// A rooted handle to a GC value, keeping it alive within a [`RootScope`].
/// `Copy`, regardless of the referent type (mirrors `wasmtime::Rooted`).
pub struct Rooted<T> {
    index: u32,
    _marker: PhantomData<T>,
}

impl<T> Clone for Rooted<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Rooted<T> {}

impl<T> core::fmt::Debug for Rooted<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rooted").finish_non_exhaustive()
    }
}

/// A scope bounding the lifetime of [`Rooted`] references. It delegates store access to
/// the wrapped store, so it's usable anywhere an `AsContext[Mut]` is. Reclamation on
/// drop is a no-op until the GC phase (the arena is grow-only; #27g adds collection).
pub struct RootScope<S> {
    store: S,
}

impl<S: AsContextMut> RootScope<S> {
    pub fn new(store: S) -> Self {
        RootScope { store }
    }
}

impl<S: AsContext> AsContext for RootScope<S> {
    type Data = S::Data;

    fn as_context(&self) -> StoreContext<'_, S::Data> {
        self.store.as_context()
    }
}

impl<S: AsContextMut> AsContextMut for RootScope<S> {
    fn as_context_mut(&mut self) -> StoreContextMut<'_, S::Data> {
        self.store.as_context_mut()
    }
}

impl<S> core::fmt::Debug for RootScope<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RootScope").finish_non_exhaustive()
    }
}

/// An opaque host reference (`externref`).
#[derive(Debug)]
pub struct ExternRef {
    _private: (),
}

impl ExternRef {
    /// Wraps a host `value` as an `externref`, stored in the store's externref arena.
    pub fn new<T>(mut store: impl AsContextMut, value: T) -> Result<Rooted<ExternRef>>
    where
        T: Any + Send + Sync + 'static,
    {
        let index = store
            .as_context_mut()
            .inner_mut()
            .alloc_externref(Box::new(value));
        Ok(Rooted {
            index,
            _marker: PhantomData,
        })
    }
}

impl Rooted<ExternRef> {
    /// Borrows the host payload behind this `externref` (downcast with `Any`). `None` if
    /// the referent carries no host data. Mirrors `wasmtime::ExternRef::data` (reached
    /// there via `Rooted`'s `Deref`; we expose it directly to stay `unsafe`-free).
    pub fn data<'a, T>(
        &self,
        store: impl Into<StoreContext<'a, T>>,
    ) -> Result<Option<&'a (dyn Any + Send + Sync)>>
    where
        T: 'static,
    {
        Ok(store.into().inner().externref(self.index))
    }
}

/// A managed GC reference under the `any` hierarchy (`anyref`).
#[derive(Debug)]
pub struct AnyRef {
    _private: (),
}

impl AnyRef {
    /// Reinterprets this `anyref` as a `structref`, if it is one.
    pub fn unwrap_struct(&self, store: impl AsContext) -> Result<Rooted<StructRef>> {
        let _ = store;
        todo!()
    }

    /// Reinterprets this `anyref` as an `arrayref`, if it is one.
    pub fn unwrap_array(&self, store: impl AsContext) -> Result<Rooted<ArrayRef>> {
        let _ = store;
        todo!()
    }
}

/// An exception reference (`exnref`).
#[derive(Debug)]
pub struct ExnRef {
    _private: (),
}

/// A GC struct instance (`structref`).
#[derive(Debug)]
pub struct StructRef {
    _private: (),
}

/// Pre-allocation handle for [`StructRef::new`] (amortizes type lookup, like wasmtime).
#[derive(Debug)]
pub struct StructRefPre {
    _private: (),
}

impl StructRefPre {
    pub fn new(store: impl AsContextMut, ty: StructType) -> Self {
        let _ = (store, ty);
        StructRefPre { _private: () }
    }
}

impl StructRef {
    /// Allocates a struct from `allocator` with the given field values.
    pub fn new(
        store: impl AsContextMut,
        allocator: &StructRefPre,
        fields: &[Val],
    ) -> Result<Rooted<StructRef>> {
        let _ = (store, allocator, fields);
        todo!()
    }

    /// Reads field `index`.
    pub fn field(&self, store: impl AsContextMut, index: usize) -> Result<Val> {
        let _ = (store, index);
        todo!()
    }
}

impl Rooted<StructRef> {
    /// Upcasts this `structref` to an `anyref`.
    pub fn to_anyref(self) -> Rooted<AnyRef> {
        todo!()
    }
}

/// A GC array instance (`arrayref`).
#[derive(Debug)]
pub struct ArrayRef {
    _private: (),
}

/// Pre-allocation handle for [`ArrayRef::new`].
#[derive(Debug)]
pub struct ArrayRefPre {
    _private: (),
}

impl ArrayRefPre {
    pub fn new(store: impl AsContextMut, ty: ArrayType) -> Self {
        let _ = (store, ty);
        ArrayRefPre { _private: () }
    }
}

impl ArrayRef {
    /// Allocates an array of `len` copies of `elem`.
    pub fn new(
        store: impl AsContextMut,
        allocator: &ArrayRefPre,
        elem: &Val,
        len: u32,
    ) -> Result<Rooted<ArrayRef>> {
        let _ = (store, allocator, elem, len);
        todo!()
    }

    /// Allocates a fixed array from the given elements.
    pub fn new_fixed(
        store: impl AsContextMut,
        allocator: &ArrayRefPre,
        elems: &[Val],
    ) -> Result<Rooted<ArrayRef>> {
        let _ = (store, allocator, elems);
        todo!()
    }

    /// The number of elements.
    pub fn len(&self, store: impl AsContext) -> Result<u32> {
        let _ = store;
        todo!()
    }

    /// Reads element `index`.
    pub fn get(&self, store: impl AsContextMut, index: u32) -> Result<Val> {
        let _ = (store, index);
        todo!()
    }
}

impl Rooted<ArrayRef> {
    /// Upcasts this `arrayref` to an `anyref`.
    pub fn to_anyref(self) -> Rooted<AnyRef> {
        todo!()
    }
}
