//! GC reference stubs. Types exist so `Val` matches wasmtime; real GC is Phase 5.

use core::any::Any;
use core::marker::PhantomData;

use crate::store::{AsContext, AsContextMut};
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

/// A scope bounding the lifetime of [`Rooted`] references.
pub struct RootScope<S> {
    _store: S,
}

impl<S: AsContextMut> RootScope<S> {
    pub fn new(store: S) -> Self {
        RootScope { _store: store }
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
    pub fn new<T>(store: impl AsContextMut, value: T) -> Result<Rooted<ExternRef>>
    where
        T: Any + Send + Sync + 'static,
    {
        todo!()
    }

    pub fn data(&self, store: impl AsContext) -> Option<&dyn Any> {
        todo!()
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
