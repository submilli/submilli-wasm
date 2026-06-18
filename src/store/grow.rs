//! Resource-limiter consultation + memory/table growth for `Store<T>`.
//!
//! Split from `mod.rs` to keep it slim. Sync paths (`grow_memory`/`grow_table`,
//! `limiter_allows_*`, the count helpers) error if an async limiter is installed; the
//! `*_async` siblings `.await` it (used by the async driver and `*_async` constructors).

use super::{ResourceLimiter, ResourceLimiterInner, Store, PAGE_SIZE};
use crate::extern_::{Memory, Table};
use crate::value::Ref;
use crate::{Error, Result};

/// Wasm pages → bytes (the limiter works in bytes for memory).
fn bytes(pages: u64) -> usize {
    pages as usize * PAGE_SIZE
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
            .unwrap_or(true);
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
            .unwrap_or(true);
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

    /// Checks the limiter for a brand-new memory of `initial` pages (`Memory::new`).
    pub(crate) fn limiter_allows_memory(&mut self, initial: u64, max: Option<u64>) -> Result<bool> {
        Ok(self
            .with_sync_limiter(|l| l.memory_growing(0, bytes(initial), max.map(bytes)))?
            .transpose()?
            .unwrap_or(true))
    }

    /// Checks the limiter for a brand-new table of `initial` elements (`Table::new`).
    pub(crate) fn limiter_allows_table(&mut self, initial: u64, max: Option<u64>) -> Result<bool> {
        Ok(self
            .with_sync_limiter(|l| l.table_growing(0, initial as usize, max.map(|m| m as usize)))?
            .transpose()?
            .unwrap_or(true))
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
            None => Ok(true),
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

    /// Async sibling of [`limiter_allows_table`](Self::limiter_allows_table) (`Table::new_async`).
    pub(crate) async fn limiter_allows_table_async(
        &mut self,
        initial: u64,
        max: Option<u64>,
    ) -> Result<bool> {
        let (current, desired, max) = (0, initial as usize, max.map(|m| m as usize));
        match &mut self.limiter {
            None => Ok(true),
            Some(ResourceLimiterInner::Sync(l)) => {
                l(&mut self.data).table_growing(current, desired, max)
            }
            Some(ResourceLimiterInner::Async(l)) => {
                l(&mut self.data).table_growing(current, desired, max).await
            }
        }
    }
}
