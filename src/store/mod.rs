//! `Store<T>` — owns runtime entities and host state; the context spine.

mod arena;
mod context;
mod entity;
mod gc;
mod gc_codec;
mod grow;
mod inner;
mod limits;

pub use context::{AsContext, AsContextMut, StoreContext, StoreContextMut};
pub(crate) use entity::{
    ExnEntity, FuncEntity, GlobalEntity, HostFrame, InstanceEntity, MemoryEntity, TableEntity,
    TagEntity, PAGE_SIZE,
};
pub(crate) use gc::{
    anyref_handle_i31, anyref_handle_slot, anyref_value, decode_anyref_handle, AnyRefHandle,
    GcObject, ObjKind,
};
pub(crate) use gc_codec::{
    default_for_slot, read_slot, read_slot_packed, slot_accepts, write_slot, NULL_REF,
};
pub(crate) use inner::{FuelStep, StoreInner};
#[cfg(feature = "async")]
pub use limits::ResourceLimiterAsync;
pub use limits::{ResourceLimiter, StoreLimits, StoreLimitsBuilder};

use std::sync::Arc;

use crate::engine::Engine;
use crate::exception::ThrownException;
use crate::func::Caller;
use crate::value::{ExnRef, Rooted, Val};
use crate::{Error, Result};

/// The store's resource limiter — a sync or (under `async`) async projection of host
/// state `T` to a limiter trait object. One slot: setting one kind replaces the other.
pub(crate) enum ResourceLimiterInner<T> {
    Sync(Box<dyn FnMut(&mut T) -> &mut (dyn ResourceLimiter) + Send + Sync>),
    #[cfg(feature = "async")]
    Async(Box<dyn FnMut(&mut T) -> &mut (dyn ResourceLimiterAsync) + Send + Sync>),
}

/// A host-function closure, type-erased only over arity — kept generic in `T`.
pub(crate) type HostFunc<T> =
    Arc<dyn Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<()> + Send + Sync>;

/// An async host-function closure: returns a boxed future the async driver awaits. The future is
/// `Send` (matching wasmtime), which keeps the driver's `call_async`/`execute_async` futures `Send`
/// so embedders can hold them across their own `Send`-bounded `.await` points.
#[cfg(feature = "async")]
pub(crate) type AsyncHostFunc<T> = Arc<
    dyn for<'a> Fn(
            Caller<'a, T>,
            &'a [Val],
            &'a mut [Val],
        )
            -> std::boxed::Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>
        + Send
        + Sync,
>;

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
    /// Async host closures (`Func::new_async`/`wrap_async`); `FuncEntity::HostAsync` indexes here.
    #[cfg(feature = "async")]
    pub(crate) async_host_funcs: Vec<AsyncHostFunc<T>>,
    /// Action when the epoch deadline is reached; `None` traps.
    pub(crate) epoch_callback: Option<EpochCallback<T>>,
    /// Resource limiter (memory/table growth + entity counts); `None` = no limits
    /// beyond module-declared maxima.
    pub(crate) limiter: Option<ResourceLimiterInner<T>>,
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
            #[cfg(feature = "async")]
            async_host_funcs: Vec::new(),
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

    /// Registers an async host closure, returning its index (in `FuncEntity::HostAsync`).
    #[cfg(feature = "async")]
    pub(crate) fn push_async_host_func(&mut self, f: AsyncHostFunc<T>) -> u32 {
        let index = self.async_host_funcs.len() as u32;
        self.async_host_funcs.push(f);
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

    /// The number of bytes backing the store's GC heap (mirrors `wasmtime::Store::gc_heap_capacity`).
    pub fn gc_heap_capacity(&self) -> usize {
        self.inner.gc.byte_size()
    }

    /// Throws `exception` from a host function so the guest's `try_table` can catch it (#28g).
    /// Returns `Err(ThrownException)`; the generic result lets it slot into any host-fn return type.
    pub fn throw<R>(
        &mut self,
        exception: Rooted<ExnRef>,
    ) -> core::result::Result<R, ThrownException> {
        self.inner.set_pending_exception(exception);
        Err(ThrownException)
    }

    /// Takes the exception that surfaced from the last call (or a host `throw`), if any.
    pub fn take_pending_exception(&mut self) -> Option<Rooted<ExnRef>> {
        self.inner.take_pending_exception()
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
        self.limiter = Some(ResourceLimiterInner::Sync(Box::new(limiter)));
    }

    /// Installs an async resource limiter (growth decisions may `.await`). Replaces any
    /// sync limiter; sync grow/alloc paths then error (use the async entry points).
    #[cfg(feature = "async")]
    pub fn limiter_async(
        &mut self,
        limiter: impl (FnMut(&mut T) -> &mut dyn ResourceLimiterAsync) + Send + Sync + 'static,
    ) {
        self.limiter = Some(ResourceLimiterInner::Async(Box::new(limiter)));
    }

    // Limiter consultation + memory/table growth live in `grow.rs`.

    pub fn as_context(&self) -> StoreContext<'_, T> {
        StoreContext(self)
    }

    pub fn as_context_mut(&mut self) -> StoreContextMut<'_, T> {
        StoreContextMut(self)
    }

    /// Configures the store to yield to the async executor every `interval` fuel units
    /// (rather than trapping). Total fuel (`set_fuel`) still bounds the run. Requires
    /// `consume_fuel` and an async store; `Some(0)` is rejected.
    #[cfg(feature = "async")]
    pub fn fuel_async_yield_interval(&mut self, interval: Option<u64>) -> Result<()> {
        if !self.inner.engine().consume_fuel() {
            return Err(Error::msg(
                "fuel is not configured; set `Config::consume_fuel(true)`",
            ));
        }
        if interval == Some(0) {
            return Err(Error::msg("fuel_async_yield_interval must not be 0"));
        }
        if !self.inner.engine().is_async() {
            return Err(Error::msg(
                "fuel_async_yield_interval requires `Config::async_support(true)`",
            ));
        }
        self.inner.set_fuel_yield_interval(interval);
        Ok(())
    }

    /// Configures the epoch deadline to yield to the async executor and then extend the
    /// deadline by `delta` ticks (instead of trapping). Requires an async store at run time.
    #[cfg(feature = "async")]
    pub fn epoch_deadline_async_yield_and_update(&mut self, delta: u64) {
        self.epoch_callback = Some(Box::new(move |_| Ok(UpdateDeadline::Yield(delta))));
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
