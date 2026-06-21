//! `AsContext`/`AsContextMut` and the `StoreContext[Mut]` borrow wrappers.

use crate::engine::Engine;
use crate::exception::ThrownException;
use crate::store::{Store, StoreInner};
use crate::value::{ExnRef, Rooted};
use crate::Result;

/// Read-only access to a store, implemented by `Store`, `Caller`, `StoreContext`, and refs.
pub trait AsContext {
    type Data: 'static;

    fn as_context(&self) -> StoreContext<'_, Self::Data>;
}

/// Mutable access to a store, implemented by `Store`, `Caller`, `StoreContextMut`, and `&mut` refs.
pub trait AsContextMut: AsContext {
    fn as_context_mut(&mut self) -> StoreContextMut<'_, Self::Data>;
}

/// A shared borrow of a [`Store`].
pub struct StoreContext<'a, T: 'static>(pub(crate) &'a Store<T>);

/// A mutable borrow of a [`Store`].
pub struct StoreContextMut<'a, T: 'static>(pub(crate) &'a mut Store<T>);

impl<T: 'static> core::fmt::Debug for StoreContext<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StoreContext").finish_non_exhaustive()
    }
}

impl<T: 'static> core::fmt::Debug for StoreContextMut<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StoreContextMut").finish_non_exhaustive()
    }
}

impl<'a, T: 'static> StoreContext<'a, T> {
    pub fn engine(&self) -> &Engine {
        self.0.engine()
    }

    pub fn data(&self) -> &'a T {
        &self.0.data
    }

    pub fn get_fuel(&self) -> Result<u64> {
        self.0.get_fuel()
    }

    pub(crate) fn inner(&self) -> &'a StoreInner {
        &self.0.inner
    }
}

impl<'a, T: 'static> StoreContextMut<'a, T> {
    pub fn engine(&self) -> &Engine {
        self.0.engine()
    }

    pub fn data(&self) -> &T {
        self.0.data()
    }

    pub fn data_mut(&mut self) -> &mut T {
        self.0.data_mut()
    }

    pub fn get_fuel(&self) -> Result<u64> {
        self.0.get_fuel()
    }

    /// Throws `exception` from a host function (see [`Store::throw`](crate::Store::throw)).
    pub fn throw<R>(
        &mut self,
        exception: Rooted<ExnRef>,
    ) -> core::result::Result<R, ThrownException> {
        self.0.throw(exception)
    }

    /// Takes the pending exception (see [`Store::take_pending_exception`]).
    pub fn take_pending_exception(&mut self) -> Option<Rooted<ExnRef>> {
        self.0.take_pending_exception()
    }

    pub(crate) fn inner(&self) -> &StoreInner {
        &self.0.inner
    }

    pub(crate) fn inner_mut(&mut self) -> &mut StoreInner {
        &mut self.0.inner
    }

    /// The underlying `Store<T>` (for host-function storage + the generic driver).
    pub(crate) fn store_mut(&mut self) -> &mut Store<T> {
        self.0
    }

    /// Consumes the context to yield the full-`'a` mutable borrow (for `Memory::data_mut`).
    pub(crate) fn into_inner_mut(self) -> &'a mut StoreInner {
        &mut self.0.inner
    }
}

impl<T: 'static> AsContext for Store<T> {
    type Data = T;

    fn as_context(&self) -> StoreContext<'_, T> {
        StoreContext(self)
    }
}

impl<T: 'static> AsContextMut for Store<T> {
    fn as_context_mut(&mut self) -> StoreContextMut<'_, T> {
        StoreContextMut(self)
    }
}

impl<T: 'static> AsContext for StoreContext<'_, T> {
    type Data = T;

    fn as_context(&self) -> StoreContext<'_, T> {
        StoreContext(self.0)
    }
}

impl<T: 'static> AsContext for StoreContextMut<'_, T> {
    type Data = T;

    fn as_context(&self) -> StoreContext<'_, T> {
        StoreContext(&*self.0)
    }
}

impl<T: 'static> AsContextMut for StoreContextMut<'_, T> {
    fn as_context_mut(&mut self) -> StoreContextMut<'_, T> {
        StoreContextMut(&mut *self.0)
    }
}

impl<T: AsContext> AsContext for &T {
    type Data = T::Data;

    fn as_context(&self) -> StoreContext<'_, T::Data> {
        T::as_context(*self)
    }
}

impl<T: AsContext> AsContext for &mut T {
    type Data = T::Data;

    fn as_context(&self) -> StoreContext<'_, T::Data> {
        T::as_context(*self)
    }
}

impl<T: AsContextMut> AsContextMut for &mut T {
    fn as_context_mut(&mut self) -> StoreContextMut<'_, T::Data> {
        T::as_context_mut(*self)
    }
}

impl<'a, T: 'static> From<StoreContextMut<'a, T>> for StoreContext<'a, T> {
    fn from(s: StoreContextMut<'a, T>) -> Self {
        StoreContext(&*s.0)
    }
}

impl<'a, T: AsContext> From<&'a T> for StoreContext<'a, T::Data> {
    fn from(t: &'a T) -> Self {
        <T as AsContext>::as_context(t)
    }
}

impl<'a, T: AsContext> From<&'a mut T> for StoreContext<'a, T::Data> {
    fn from(t: &'a mut T) -> Self {
        <T as AsContext>::as_context(t)
    }
}

impl<'a, T: AsContextMut> From<&'a mut T> for StoreContextMut<'a, T::Data> {
    fn from(t: &'a mut T) -> Self {
        <T as AsContextMut>::as_context_mut(t)
    }
}
