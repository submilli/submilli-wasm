//! `Store<T>` — owns runtime entities and host state; the context spine.

mod context;
mod entity;
mod inner;
mod limits;

pub use context::{AsContext, AsContextMut, StoreContext, StoreContextMut};
pub(crate) use entity::{FuncEntity, GlobalEntity, InstanceEntity, MemoryEntity, TableEntity};
pub(crate) use inner::StoreInner;
pub use limits::{ResourceLimiter, StoreLimits, StoreLimitsBuilder};

use crate::engine::Engine;
use crate::Result;

/// A collection of instantiated WebAssembly objects plus host state `T`.
pub struct Store<T: 'static> {
    pub(crate) inner: StoreInner,
    pub(crate) data: T,
}

impl<T: 'static> core::fmt::Debug for Store<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl<T: 'static> Store<T> {
    /// Creates a new store associated with `engine`, carrying host state `data`.
    pub fn new(engine: &Engine, data: T) -> Self {
        Store {
            inner: StoreInner::new(engine.clone()),
            data,
        }
    }

    pub fn data(&self) -> &T {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut T {
        &mut self.data
    }

    pub fn into_data(self) -> T {
        self.data
    }

    pub fn engine(&self) -> &Engine {
        self.inner.engine()
    }

    pub fn set_fuel(&mut self, fuel: u64) -> Result<()> {
        todo!()
    }

    pub fn get_fuel(&self) -> Result<u64> {
        todo!()
    }

    pub fn set_epoch_deadline(&mut self, ticks_beyond_current: u64) {
        todo!()
    }

    pub fn epoch_deadline_trap(&mut self) {
        todo!()
    }

    pub fn epoch_deadline_callback(
        &mut self,
        callback: impl FnMut(StoreContextMut<'_, T>) -> Result<UpdateDeadline> + Send + Sync + 'static,
    ) {
        todo!()
    }

    pub fn limiter(
        &mut self,
        limiter: impl (FnMut(&mut T) -> &mut dyn ResourceLimiter) + Send + Sync + 'static,
    ) {
        todo!()
    }

    pub fn as_context(&self) -> StoreContext<'_, T> {
        StoreContext(self)
    }

    pub fn as_context_mut(&mut self) -> StoreContextMut<'_, T> {
        StoreContextMut(self)
    }

    #[cfg(feature = "async")]
    pub fn fuel_async_yield_interval(&mut self, interval: Option<u64>) -> Result<()> {
        todo!()
    }
}

/// Action to take when an epoch deadline is reached (see `Store::epoch_deadline_callback`).
#[non_exhaustive]
#[derive(Debug)]
pub enum UpdateDeadline {
    Interrupt,
    Continue(u64),
    #[cfg(feature = "async")]
    Yield(u64),
}

/// A host/wasm boundary-crossing event (see `Store::call_hook`).
#[derive(Copy, Clone, Debug)]
pub enum CallHook {
    CallingWasm,
    ReturningFromWasm,
    CallingHost,
    ReturningFromHost,
}
