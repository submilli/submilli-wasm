//! Resource-limiter consultation + memory/table growth for `Store<T>`.
//!
//! Split from `mod.rs` to keep it slim. Sync paths (`grow_memory`/`grow_table`,
//! `limiter_allows_*`, the count helpers) error if an async limiter is installed; the
//! `*_async` siblings `.await` it (used by the async driver and `*_async` constructors).

use super::limits::{DEFAULT_MEMORY_CEILING_BYTES, DEFAULT_TABLE_CEILING_ELEMS};
use super::{ResourceLimiter, ResourceLimiterInner, Store, PAGE_SIZE};
use crate::extern_::{Memory, Table};
use crate::value::Ref;
use crate::{Error, Result};

/// Wasm pages → bytes (the limiter works in bytes for memory). Saturating so a hostile `memory64`
/// page count (up to 2^48) can't overflow-panic the limiter check — a saturated `usize::MAX` simply
/// exceeds any finite limit/ceiling and is denied.
fn bytes(pages: u64) -> usize {
    (pages as usize).saturating_mul(PAGE_SIZE)
}

impl<T: 'static> Store<T> {
    /// Runs `f` against an installed *sync* limiter. `Ok(None)` = no limiter; an async
    /// limiter on a sync path is an error (use the `*_async` API).
    // The `Err` arm only exists under `async`; without it the `Result` looks redundant.
    #[allow(clippy::unnecessary_wraps)]
    fn with_sync_limiter<R>(
        &mut self,
        f: impl FnOnce(&mut dyn ResourceLimiter) -> R,
    ) -> Result<Option<R>> {
        match self.limiter.as_mut() {
            None => Ok(None),
            Some(ResourceLimiterInner::Sync(l)) => Ok(Some(f(l(&mut self.data)))),
            #[cfg(feature = "async")]
            Some(ResourceLimiterInner::Async(_)) => Err(Error::msg(
                "an async resource limiter requires an async context (use the `*_async` API)",
            )),
        }
    }

    pub(crate) fn limiter_memories(&mut self) -> Option<usize> {
        match self.limiter.as_mut()? {
            ResourceLimiterInner::Sync(l) => Some(l(&mut self.data).memories()),
            #[cfg(feature = "async")]
            ResourceLimiterInner::Async(l) => Some(l(&mut self.data).memories()),
        }
    }

    pub(crate) fn limiter_tables(&mut self) -> Option<usize> {
        match self.limiter.as_mut()? {
            ResourceLimiterInner::Sync(l) => Some(l(&mut self.data).tables()),
            #[cfg(feature = "async")]
            ResourceLimiterInner::Async(l) => Some(l(&mut self.data).tables()),
        }
    }

    pub(crate) fn limiter_instances(&mut self) -> Option<usize> {
        match self.limiter.as_mut()? {
            ResourceLimiterInner::Sync(l) => Some(l(&mut self.data).instances()),
            #[cfg(feature = "async")]
            ResourceLimiterInner::Async(l) => Some(l(&mut self.data).instances()),
        }
    }

    /// Notifies the limiter of a failed memory grow (`trap_on_grow_failure` → `Err`).
    /// `*_grow_failed` is sync on both limiter kinds.
    fn memory_grow_failed(&mut self) -> Result<()> {
        let err = Error::msg("failed to grow memory");
        match self.limiter.as_mut() {
            None => Ok(()),
            Some(ResourceLimiterInner::Sync(l)) => l(&mut self.data).memory_grow_failed(err),
            #[cfg(feature = "async")]
            Some(ResourceLimiterInner::Async(l)) => l(&mut self.data).memory_grow_failed(err),
        }
    }

    /// Grows memory `handle` by `delta` pages, consulting the limiter and the
    /// declared/architectural maximum. `Ok(Some(old))` grew; `Ok(None)` is a soft
    /// failure (return `-1`); `Err` is a trap (`trap_on_grow_failure` / async limiter).
    pub(crate) fn grow_memory(&mut self, handle: Memory, delta: u64) -> Result<Option<u64>> {
        let (current, max) = {
            let e = self.inner.memory(handle);
            (e.size_pages(), e.ty.maximum())
        };
        let desired = current.saturating_add(delta);
        let allowed = self
            .with_sync_limiter(|l| {
                l.memory_growing(bytes(current), bytes(desired), max.map(bytes))
            })?
            .transpose()?
            .unwrap_or(bytes(desired) <= DEFAULT_MEMORY_CEILING_BYTES);
        if allowed {
            if let Some(old) = self.inner.memory_mut(handle).grow(delta) {
                return Ok(Some(old));
            }
        }
        self.memory_grow_failed()?;
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
            .with_sync_limiter(|l| l.table_growing(current, desired, max))?
            .transpose()?
            .unwrap_or(desired <= DEFAULT_TABLE_CEILING_ELEMS);
        if allowed {
            if let Some(old) = self.inner.table_mut(handle).grow(delta, init) {
                return Ok(Some(old));
            }
        }
        let err = Error::msg("failed to grow table");
        match self.limiter.as_mut() {
            None => {}
            Some(ResourceLimiterInner::Sync(l)) => l(&mut self.data).table_grow_failed(err)?,
            #[cfg(feature = "async")]
            Some(ResourceLimiterInner::Async(l)) => l(&mut self.data).table_grow_failed(err)?,
        }
        Ok(None)
    }

    /// Grows the store's GC reservation to `target` bytes, consulting the limiter. The GC heap is
    /// accounted like a memory (as in wasmtime) but has **no declared maximum** — that argument is
    /// `None`; the limiter is the sole growth bound. A denial traps (guest GC allocation has no
    /// soft-fail path). Updates the engine-wide committed-bytes counter that drives the GC-pressure
    /// axis. The reservation flow only calls this when `target` exceeds the current reservation.
    pub(crate) fn grow_gc_reservation(&mut self, target: usize, bytes_needed: u64) -> Result<()> {
        let current = self.inner.gc.reserved();
        if target <= current {
            return Ok(());
        }
        // A limiter is the sole bound when installed (the GC heap may grow past the abort cap);
        // with no limiter, the abort-safety cap is the bound (the no-limiter finite ceiling).
        let allowed = match self.with_sync_limiter(|l| l.memory_growing(current, target, None))? {
            Some(decision) => decision?,
            None => target <= super::gc::ABORT_SAFETY_CAP,
        };
        if !allowed {
            // Matches wasmtime: a GC allocation the heap can't grow to satisfy is `GcHeapOutOfMemory`,
            // not a generic `AllocationTooLarge` trap.
            return Err(crate::gc::GcHeapOutOfMemory::new((), bytes_needed).into());
        }
        let granted = self.inner.gc.grant(target);
        self.inner.engine().add_gc_committed(granted);
        Ok(())
    }

    /// Ensures the GC heap can hold a `charge`-byte **host-built** object (`StructRef`/`ArrayRef::new`):
    /// collect-then-grow through the limiter, **synchronously**. Unlike the guest path
    /// (`Execution::gc_reserve`, which suspends to reach the `T`-generic limiter), host code already
    /// holds the `Store<T>`, so it consults the limiter inline. This is a safe point because
    /// `invoke_host` parks the live guest operands (and the call's params) in `gc_roots` for the
    /// call's duration, so a collection here sees them as roots. Mirrors `gc_reserve`: growth within
    /// the pre-authorized free budget is granted directly (no collection, no limiter); only growth
    /// beyond it collects first, then grows through the limiter (a denial traps). Kept in lockstep
    /// with `Execution::gc_reserve`.
    pub(crate) fn gc_reserve_host(&mut self, charge: usize) -> Result<()> {
        if self.inner.gc.fits(charge) {
            return Ok(());
        }
        if !self
            .inner
            .gc
            .is_free_grant(self.inner.gc.desired_reservation(charge))
            && self.inner.gc.is_collecting()
        {
            // The guest's operands live on the parked execution (this runs inside a host call);
            // seed the collection from there so they survive (#27g).
            let roots = self.inner.exec_roots();
            self.inner.gc_collect(&roots);
            if self.inner.gc.fits(charge) {
                return Ok(());
            }
        }
        let target = self.inner.gc.desired_reservation(charge);
        if self.inner.gc.is_free_grant(target) {
            let granted = self.inner.gc.grant(target);
            self.inner.engine().add_gc_committed(granted);
            return Ok(());
        }
        self.grow_gc_reservation(target, charge as u64)
    }

    /// Host `ExternRef::new`: reserve the entry's footprint through the limiter, then store it
    /// (charging the GC budget). Bounded + reclaimable like the GC object heap (#27g).
    pub(crate) fn gc_alloc_externref(
        &mut self,
        value: Box<dyn core::any::Any + Send + Sync>,
    ) -> Result<u32> {
        self.gc_reserve_host(super::entity::extern_charge())?;
        self.inner.alloc_externref(value)
    }

    /// Host `ExnRef::new`: reserve the exception's footprint through the limiter, then store it.
    pub(crate) fn gc_alloc_exn(
        &mut self,
        entity: super::ExnEntity,
    ) -> Result<crate::value::Rooted<crate::value::ExnRef>> {
        self.gc_reserve_host(entity.byte_size())?;
        self.inner.alloc_exn(entity)
    }

    /// Checks the limiter for a brand-new memory of `initial` pages (`Memory::new` or a defined
    /// memory at instantiation). With no limiter, the finite default ceiling is the bound.
    pub(crate) fn limiter_allows_memory(&mut self, initial: u64, max: Option<u64>) -> Result<bool> {
        Ok(self
            .with_sync_limiter(|l| l.memory_growing(0, bytes(initial), max.map(bytes)))?
            .transpose()?
            .unwrap_or(bytes(initial) <= DEFAULT_MEMORY_CEILING_BYTES))
    }

    /// Checks the limiter for a brand-new table of `initial` elements (`Table::new` or a defined
    /// table at instantiation). With no limiter, the finite default ceiling is the bound.
    pub(crate) fn limiter_allows_table(&mut self, initial: u64, max: Option<u64>) -> Result<bool> {
        Ok(self
            .with_sync_limiter(|l| l.table_growing(0, initial as usize, max.map(|m| m as usize)))?
            .transpose()?
            .unwrap_or(initial as usize <= DEFAULT_TABLE_CEILING_ELEMS))
    }
}

#[cfg(feature = "async")]
impl<T: 'static> Store<T> {
    /// Consults the limiter for a memory grow `current → desired` bytes, awaiting an
    /// async limiter. No limiter ⇒ allowed.
    async fn memory_growing_async(
        &mut self,
        current: usize,
        desired: usize,
        max: Option<usize>,
    ) -> Result<bool> {
        match &mut self.limiter {
            None => Ok(desired <= DEFAULT_MEMORY_CEILING_BYTES),
            Some(ResourceLimiterInner::Sync(l)) => {
                l(&mut self.data).memory_growing(current, desired, max)
            }
            Some(ResourceLimiterInner::Async(l)) => {
                l(&mut self.data)
                    .memory_growing(current, desired, max)
                    .await
            }
        }
    }

    /// Async sibling of [`grow_memory`](Self::grow_memory): awaits the limiter.
    pub(crate) async fn grow_memory_async(
        &mut self,
        handle: Memory,
        delta: u64,
    ) -> Result<Option<u64>> {
        let (current, max) = {
            let e = self.inner.memory(handle);
            (e.size_pages(), e.ty.maximum())
        };
        let desired = current.saturating_add(delta);
        let allowed = self
            .memory_growing_async(bytes(current), bytes(desired), max.map(bytes))
            .await?;
        if allowed {
            if let Some(old) = self.inner.memory_mut(handle).grow(delta) {
                return Ok(Some(old));
            }
        }
        self.memory_grow_failed()?;
        Ok(None)
    }

    /// Async sibling of [`limiter_allows_memory`](Self::limiter_allows_memory) (`Memory::new_async`).
    pub(crate) async fn limiter_allows_memory_async(
        &mut self,
        initial: u64,
        max: Option<u64>,
    ) -> Result<bool> {
        self.memory_growing_async(0, bytes(initial), max.map(bytes))
            .await
    }

    /// Consults the limiter for a table grow `current → desired` elements, awaiting an
    /// async limiter. No limiter ⇒ allowed.
    async fn table_growing_async(
        &mut self,
        current: usize,
        desired: usize,
        max: Option<usize>,
    ) -> Result<bool> {
        match &mut self.limiter {
            None => Ok(desired <= DEFAULT_TABLE_CEILING_ELEMS),
            Some(ResourceLimiterInner::Sync(l)) => {
                l(&mut self.data).table_growing(current, desired, max)
            }
            Some(ResourceLimiterInner::Async(l)) => {
                l(&mut self.data).table_growing(current, desired, max).await
            }
        }
    }

    /// Async sibling of [`limiter_allows_table`](Self::limiter_allows_table) (`Table::new_async`).
    pub(crate) async fn limiter_allows_table_async(
        &mut self,
        initial: u64,
        max: Option<u64>,
    ) -> Result<bool> {
        self.table_growing_async(0, initial as usize, max.map(|m| m as usize))
            .await
    }

    /// Async sibling of [`grow_table`](Self::grow_table): awaits the limiter.
    pub(crate) async fn grow_table_async(
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
        let allowed = self.table_growing_async(current, desired, max).await?;
        if allowed {
            if let Some(old) = self.inner.table_mut(handle).grow(delta, init) {
                return Ok(Some(old));
            }
        }
        let err = Error::msg("failed to grow table");
        match self.limiter.as_mut() {
            None => {}
            Some(ResourceLimiterInner::Sync(l)) => l(&mut self.data).table_grow_failed(err)?,
            Some(ResourceLimiterInner::Async(l)) => l(&mut self.data).table_grow_failed(err)?,
        }
        Ok(None)
    }

    /// Async sibling of [`gc_reserve_host`](Self::gc_reserve_host): awaits an async limiter when
    /// host code allocates GC objects from an async context.
    pub(crate) async fn gc_reserve_host_async(&mut self, charge: usize) -> Result<()> {
        if self.inner.gc.fits(charge) {
            return Ok(());
        }
        if !self
            .inner
            .gc
            .is_free_grant(self.inner.gc.desired_reservation(charge))
            && self.inner.gc.is_collecting()
        {
            let roots = self.inner.exec_roots();
            self.inner.gc_collect(&roots);
            if self.inner.gc.fits(charge) {
                return Ok(());
            }
        }
        let target = self.inner.gc.desired_reservation(charge);
        if self.inner.gc.is_free_grant(target) {
            let granted = self.inner.gc.grant(target);
            self.inner.engine().add_gc_committed(granted);
            return Ok(());
        }

        let current = self.inner.gc.reserved();
        let allowed = match &mut self.limiter {
            None => target <= super::gc::ABORT_SAFETY_CAP,
            Some(ResourceLimiterInner::Sync(l)) => {
                l(&mut self.data).memory_growing(current, target, None)?
            }
            Some(ResourceLimiterInner::Async(l)) => {
                l(&mut self.data)
                    .memory_growing(current, target, None)
                    .await?
            }
        };
        if !allowed {
            return Err(crate::gc::GcHeapOutOfMemory::new((), charge as u64).into());
        }
        let granted = self.inner.gc.grant(target);
        self.inner.engine().add_gc_committed(granted);
        Ok(())
    }
}
