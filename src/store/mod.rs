//! `Store<T>` — owns runtime entities and host state; the context spine.

mod context;
mod entity;
mod inner;
mod limits;

pub use context::{AsContext, AsContextMut, StoreContext, StoreContextMut};
pub(crate) use entity::{
    FuncEntity, GlobalEntity, InstanceEntity, MemoryEntity, TableEntity, PAGE_SIZE,
};
pub(crate) use inner::StoreInner;
pub use limits::{ResourceLimiter, StoreLimits, StoreLimitsBuilder};

use std::sync::Arc;

use crate::engine::Engine;
use crate::extern_::{Memory, Table};
use crate::func::Caller;
use crate::value::{Ref, Val};
use crate::{Error, Result};

/// The store's resource limiter: projects host state `T` to a `ResourceLimiter`.
pub(crate) type Limiter<T> = Box<dyn FnMut(&mut T) -> &mut (dyn ResourceLimiter) + Send + Sync>;

/// Wasm pages → bytes (the limiter works in bytes for memory).
fn bytes(pages: u64) -> usize {
    pages as usize * PAGE_SIZE
}

/// A host-function closure, type-erased only over arity — kept generic in `T`.
pub(crate) type HostFunc<T> =
    Arc<dyn Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<()> + Send + Sync>;

/// An epoch-deadline callback (`None` = trap on deadline). `T`-generic, so it
/// lives on `Store<T>` and is applied by the generic execution driver.
pub(crate) type EpochCallback<T> =
    Box<dyn FnMut(StoreContextMut<'_, T>) -> Result<UpdateDeadline> + Send + Sync>;

/// A collection of instantiated WebAssembly objects plus host state `T`.
pub struct Store<T: 'static> {
    pub(crate) inner: StoreInner,
    pub(crate) data: T,
    /// Host closures created via `Func::new`/`wrap`; `FuncEntity::Host` indexes here.
    pub(crate) host_funcs: Vec<HostFunc<T>>,
    /// Action when the epoch deadline is reached; `None` traps.
    pub(crate) epoch_callback: Option<EpochCallback<T>>,
    /// Resource limiter (memory/table growth + entity counts); `None` = no limits
    /// beyond module-declared maxima.
    pub(crate) limiter: Option<Limiter<T>>,
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
            host_funcs: Vec::new(),
            epoch_callback: None,
            limiter: None,
        }
    }

    /// Registers a host closure, returning its index (stored in `FuncEntity::Host`).
    pub(crate) fn push_host_func(&mut self, f: HostFunc<T>) -> u32 {
        let index = self.host_funcs.len() as u32;
        self.host_funcs.push(f);
        index
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
        if !self.inner.engine().consume_fuel() {
            return Err(Error::msg(
                "fuel is not configured; set `Config::consume_fuel(true)`",
            ));
        }
        self.inner.set_fuel(fuel);
        Ok(())
    }

    pub fn get_fuel(&self) -> Result<u64> {
        if !self.inner.engine().consume_fuel() {
            return Err(Error::msg(
                "fuel is not configured; set `Config::consume_fuel(true)`",
            ));
        }
        Ok(self.inner.fuel())
    }

    pub fn set_epoch_deadline(&mut self, ticks_beyond_current: u64) {
        let deadline = self
            .inner
            .engine()
            .current_epoch()
            .saturating_add(ticks_beyond_current);
        self.inner.set_epoch_deadline(deadline);
    }

    pub fn epoch_deadline_trap(&mut self) {
        self.epoch_callback = None;
    }

    pub fn epoch_deadline_callback(
        &mut self,
        callback: impl FnMut(StoreContextMut<'_, T>) -> Result<UpdateDeadline> + Send + Sync + 'static,
    ) {
        self.epoch_callback = Some(Box::new(callback));
    }

    pub fn limiter(
        &mut self,
        limiter: impl (FnMut(&mut T) -> &mut dyn ResourceLimiter) + Send + Sync + 'static,
    ) {
        self.limiter = Some(Box::new(limiter));
    }

    /// Runs `f` against the installed limiter, if any (splits the disjoint
    /// `limiter`/`data` fields).
    fn with_limiter<R>(&mut self, f: impl FnOnce(&mut dyn ResourceLimiter) -> R) -> Option<R> {
        self.limiter.as_mut().map(|l| f(l(&mut self.data)))
    }

    pub(crate) fn limiter_memories(&mut self) -> Option<usize> {
        self.with_limiter(|l| l.memories())
    }

    pub(crate) fn limiter_tables(&mut self) -> Option<usize> {
        self.with_limiter(|l| l.tables())
    }

    pub(crate) fn limiter_instances(&mut self) -> Option<usize> {
        self.with_limiter(|l| l.instances())
    }

    /// Grows memory `handle` by `delta` pages, consulting the limiter and the
    /// declared/architectural maximum. `Ok(Some(old))` grew; `Ok(None)` is a soft
    /// failure (return `-1`); `Err` is a trap (`trap_on_grow_failure`).
    pub(crate) fn grow_memory(&mut self, handle: Memory, delta: u64) -> Result<Option<u64>> {
        let (current, max) = {
            let e = self.inner.memory(handle);
            (e.size_pages(), e.ty.maximum())
        };
        let desired = current.saturating_add(delta);
        let allowed = self
            .with_limiter(|l| l.memory_growing(bytes(current), bytes(desired), max.map(bytes)))
            .transpose()?
            .unwrap_or(true);
        if allowed {
            if let Some(old) = self.inner.memory_mut(handle).grow(delta) {
                return Ok(Some(old));
            }
        }
        self.with_limiter(|l| l.memory_grow_failed(Error::msg("failed to grow memory")))
            .transpose()?;
        Ok(None)
    }

    /// Grows table `handle` by `delta` elements (filled with `init`); same result
    /// convention as [`grow_memory`](Self::grow_memory).
    pub(crate) fn grow_table(
        &mut self,
        handle: Table,
        delta: u64,
        init: Ref,
    ) -> Result<Option<u64>> {
        let (current, max) = {
            let e = self.inner.table(handle);
            (e.size() as usize, e.ty.maximum().map(|m| m as usize))
        };
        let desired = current.saturating_add(delta as usize);
        let allowed = self
            .with_limiter(|l| l.table_growing(current, desired, max))
            .transpose()?
            .unwrap_or(true);
        if allowed {
            if let Some(old) = self.inner.table_mut(handle).grow(delta, init) {
                return Ok(Some(old));
            }
        }
        self.with_limiter(|l| l.table_grow_failed(Error::msg("failed to grow table")))
            .transpose()?;
        Ok(None)
    }

    /// Checks the limiter for a brand-new memory of `initial` pages (`Memory::new`).
    pub(crate) fn limiter_allows_memory(&mut self, initial: u64, max: Option<u64>) -> Result<bool> {
        Ok(self
            .with_limiter(|l| l.memory_growing(0, bytes(initial), max.map(bytes)))
            .transpose()?
            .unwrap_or(true))
    }

    /// Checks the limiter for a brand-new table of `initial` elements (`Table::new`).
    pub(crate) fn limiter_allows_table(&mut self, initial: u64, max: Option<u64>) -> Result<bool> {
        Ok(self
            .with_limiter(|l| l.table_growing(0, initial as usize, max.map(|m| m as usize)))
            .transpose()?
            .unwrap_or(true))
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
