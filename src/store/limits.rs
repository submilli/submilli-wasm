//! Resource limiting: the `ResourceLimiter` trait and `StoreLimits[Builder]`.

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;

use crate::{Error, Result};

const DEFAULT_LIMIT: usize = 10_000;

/// The finite default ceiling on a single memory's size (bytes), applied **only when no
/// [`ResourceLimiter`] is installed** — with a limiter, the limiter is the sole bound. It equals the
/// 32-bit architectural maximum (4 GiB), so `memory32` behavior is unchanged while a `memory64`
/// (whose architectural ceiling is 2^48 pages) is held to a finite multi-tenant default rather than
/// being effectively unbounded. Tunable policy knob (see `docs/SECURITY.md`); the GC heap's analog
/// is [`super::gc::ABORT_SAFETY_CAP`].
pub(crate) const DEFAULT_MEMORY_CEILING_BYTES: usize = 1usize << 32;

/// The finite default ceiling on a single table's element count, applied **only when no
/// [`ResourceLimiter`] is installed**. Equals the 32-bit architectural maximum (`u32::MAX`), so
/// `table32` is unchanged while a `table64` (architectural ceiling `u64::MAX`) is held finite.
pub(crate) const DEFAULT_TABLE_CEILING_ELEMS: usize = u32::MAX as usize;

/// Controls resource growth (memory/table) and entity counts for a store.
pub trait ResourceLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool>;

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool>;

    fn memory_grow_failed(&mut self, error: Error) -> Result<()> {
        Ok(())
    }

    fn table_grow_failed(&mut self, error: Error) -> Result<()> {
        Ok(())
    }

    fn instances(&self) -> usize {
        DEFAULT_LIMIT
    }

    fn tables(&self) -> usize {
        DEFAULT_LIMIT
    }

    fn memories(&self) -> usize {
        DEFAULT_LIMIT
    }
}

/// Async sibling of [`ResourceLimiter`]: growth decisions may `.await` (e.g. consult an
/// async quota service). Used via [`Store::limiter_async`](crate::Store::limiter_async).
#[cfg(feature = "async")]
#[async_trait::async_trait]
pub trait ResourceLimiterAsync: Send {
    async fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool>;

    async fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool>;

    fn memory_grow_failed(&mut self, error: Error) -> Result<()> {
        Ok(())
    }

    fn table_grow_failed(&mut self, error: Error) -> Result<()> {
        Ok(())
    }

    fn instances(&self) -> usize {
        DEFAULT_LIMIT
    }

    fn tables(&self) -> usize {
        DEFAULT_LIMIT
    }

    fn memories(&self) -> usize {
        DEFAULT_LIMIT
    }
}

/// A ready-made [`ResourceLimiter`] configured by [`StoreLimitsBuilder`].
#[derive(Clone, Debug)]
pub struct StoreLimits {
    memory_size: Option<usize>,
    table_elements: Option<usize>,
    instances: usize,
    tables: usize,
    memories: usize,
    trap_on_grow_failure: bool,
}

impl Default for StoreLimits {
    fn default() -> Self {
        StoreLimits {
            memory_size: None,
            table_elements: None,
            instances: DEFAULT_LIMIT,
            tables: DEFAULT_LIMIT,
            memories: DEFAULT_LIMIT,
            trap_on_grow_failure: false,
        }
    }
}

impl ResourceLimiter for StoreLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool> {
        let within_config = self.memory_size.is_none_or(|limit| desired <= limit);
        let within_max = maximum.is_none_or(|max| desired <= max);
        Ok(within_config && within_max)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool> {
        let within_config = self.table_elements.is_none_or(|limit| desired <= limit);
        let within_max = maximum.is_none_or(|max| desired <= max);
        Ok(within_config && within_max)
    }

    fn memory_grow_failed(&mut self, error: Error) -> Result<()> {
        if self.trap_on_grow_failure {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn table_grow_failed(&mut self, error: Error) -> Result<()> {
        if self.trap_on_grow_failure {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn instances(&self) -> usize {
        self.instances
    }

    fn tables(&self) -> usize {
        self.tables
    }

    fn memories(&self) -> usize {
        self.memories
    }
}

/// Builder for [`StoreLimits`].
#[derive(Debug)]
pub struct StoreLimitsBuilder(StoreLimits);

impl StoreLimitsBuilder {
    pub fn new() -> Self {
        StoreLimitsBuilder(StoreLimits::default())
    }

    pub fn memory_size(mut self, limit: usize) -> Self {
        self.0.memory_size = Some(limit);
        self
    }

    pub fn table_elements(mut self, limit: usize) -> Self {
        self.0.table_elements = Some(limit);
        self
    }

    pub fn instances(mut self, limit: usize) -> Self {
        self.0.instances = limit;
        self
    }

    pub fn tables(mut self, limit: usize) -> Self {
        self.0.tables = limit;
        self
    }

    pub fn memories(mut self, limit: usize) -> Self {
        self.0.memories = limit;
        self
    }

    pub fn trap_on_grow_failure(mut self, trap: bool) -> Self {
        self.0.trap_on_grow_failure = trap;
        self
    }

    pub fn build(self) -> StoreLimits {
        self.0
    }
}

impl Default for StoreLimitsBuilder {
    fn default() -> Self {
        StoreLimitsBuilder::new()
    }
}
