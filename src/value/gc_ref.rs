//! GC reference handles. `externref` is real (a store-side host-payload arena);
//! the managed-GC refs (`anyref`/struct/array/exn) stay stubs until garbage collection lands.

use core::any::Any;
use core::marker::PhantomData;

use crate::store::{
    decode_anyref_handle, AnyRefHandle, AsContext, AsContextMut, GcObject, ObjKind, StoreContext,
    StoreContextMut,
};
use crate::{Error, Result};

use super::gc_aggregate::{ArrayRef, StructRef};

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

impl<T> Rooted<T> {
    /// Wraps a raw handle/index (an `anyref` handle for `AnyRef`, an arena index for
    /// `ExternRef`). Internal — the run loop builds reference values from raw handles.
    pub(crate) fn from_raw(index: u32) -> Self {
        Rooted {
            index,
            _marker: PhantomData,
        }
    }

    /// The raw handle/index behind this rooted reference.
    pub(crate) fn raw(self) -> u32 {
        self.index
    }

    /// Whether `a` and `b` refer to the same GC object (reference identity). Under the grow-only
    /// arena each live object has a unique handle, so this is handle equality. An associated
    /// function (not a method), matching `wasmtime::Rooted::ref_eq`'s `(store, a, b)` shape.
    pub fn ref_eq<U>(_store: impl AsContext, a: &Rooted<T>, b: &Rooted<U>) -> Result<bool> {
        Ok(a.index == b.index)
    }
}

impl<T> core::fmt::Debug for Rooted<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rooted").finish_non_exhaustive()
    }
}

/// A scope bounding the lifetime of [`Rooted`] references. It delegates store access to
/// the wrapped store, so it's usable anywhere an `AsContext[Mut]` is. Reclamation on
/// drop is a no-op for now (the arena is grow-only; a tracing collector adds reclamation).
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

    /// Mutably borrows the host payload behind this `externref`. Mirrors
    /// `wasmtime::ExternRef::data_mut`.
    pub fn data_mut<'a, T>(
        &self,
        store: impl Into<StoreContextMut<'a, T>>,
    ) -> Result<Option<&'a mut (dyn Any + Send + Sync)>>
    where
        T: 'static,
    {
        Ok(store.into().into_inner_mut().externref_mut(self.index))
    }
}

/// A managed GC reference under the `any` hierarchy (`anyref`). The handle lives in the wrapping
/// [`Rooted`]; the inspection/cast methods are on `Rooted<AnyRef>` (the `Rooted<ExternRef>::data`
/// pattern — keeps us `unsafe`-free, and `rooted.method()` still matches wasmtime call sites).
#[derive(Debug)]
pub struct AnyRef {
    _private: (),
}

impl Rooted<AnyRef> {
    /// Reinterprets this `anyref` as a `structref`, erroring if it isn't one.
    pub fn unwrap_struct(&self, store: impl AsContext) -> Result<Rooted<StructRef>> {
        self.as_struct(store)?
            .ok_or_else(|| Error::msg("anyref is not a struct"))
    }

    /// This `anyref` as a `structref` if it is one, else `None`.
    pub fn as_struct(&self, store: impl AsContext) -> Result<Option<Rooted<StructRef>>> {
        Ok(
            if obj_kind(store.as_context().inner(), self.raw())? == Some(ObjKind::Struct) {
                Some(Rooted::from_raw(self.raw()))
            } else {
                None
            },
        )
    }

    /// Reinterprets this `anyref` as an `arrayref`, erroring if it isn't one.
    pub fn unwrap_array(&self, store: impl AsContext) -> Result<Rooted<ArrayRef>> {
        self.as_array(store)?
            .ok_or_else(|| Error::msg("anyref is not an array"))
    }

    /// This `anyref` as an `arrayref` if it is one, else `None`.
    pub fn as_array(&self, store: impl AsContext) -> Result<Option<Rooted<ArrayRef>>> {
        Ok(
            if obj_kind(store.as_context().inner(), self.raw())? == Some(ObjKind::Array) {
                Some(Rooted::from_raw(self.raw()))
            } else {
                None
            },
        )
    }
}

/// An exception reference (`exnref`).
#[derive(Debug)]
pub struct ExnRef {
    _private: (),
}

/// Decodes an `anyref` handle to a heap slot, erroring on an `i31` (which isn't a heap object).
pub(super) fn gc_slot(inner: &crate::store::StoreInner, handle: u32) -> Result<u32> {
    match decode_anyref_handle(handle) {
        AnyRefHandle::Slot(i) => Ok(i),
        AnyRefHandle::I31(_) => Err(Error::msg("reference is an i31, not a heap object")),
    }
    .and_then(|i| {
        gc_object(inner, i)?;
        Ok(i)
    })
}

/// The object at a heap slot, erroring if the handle dangles.
pub(super) fn gc_object(inner: &crate::store::StoreInner, slot: u32) -> Result<&GcObject> {
    inner
        .gc_object(slot)
        .ok_or_else(|| Error::msg("dangling gc reference"))
}

/// The `ObjKind` of the object an `anyref` handle points at (`None` for an `i31`).
fn obj_kind(inner: &crate::store::StoreInner, handle: u32) -> Result<Option<ObjKind>> {
    match decode_anyref_handle(handle) {
        AnyRefHandle::I31(_) => Ok(None),
        AnyRefHandle::Slot(i) => Ok(Some(gc_object(inner, i)?.header.kind)),
    }
}
