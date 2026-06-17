//! GC reference stubs. Types exist so `Val` matches wasmtime; real GC is Phase 5.

use core::any::Any;
use core::marker::PhantomData;

use crate::store::{AsContext, AsContextMut};
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

/// An exception reference (`exnref`).
#[derive(Debug)]
pub struct ExnRef {
    _private: (),
}
