//! Garbage-collected heap: handle table + non-moving stop-the-world mark-sweep.

use core::fmt;

/// The GC heap is at capacity and an allocation could not be satisfied.
///
/// Mirrors [`wasmtime::GcHeapOutOfMemory`]: carried inside an [`anyhow::Error`]
/// (recover via `err.downcast_ref::<GcHeapOutOfMemory<()>>()`) and, for failed
/// `externref` allocations, holds the host value (`inner`) that could not be
/// allocated so it can be recovered and retried after a GC. For non-`externref`
/// allocations `T` is the unit type `()`.
pub struct GcHeapOutOfMemory<T> {
    inner: T,
    bytes_needed: u64,
}

impl<T> fmt::Debug for GcHeapOutOfMemory<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<T> fmt::Display for GcHeapOutOfMemory<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GC heap out of memory: no capacity for allocation of {} bytes",
            self.bytes_needed
        )
    }
}

impl<T> std::error::Error for GcHeapOutOfMemory<T> {}

impl<T> GcHeapOutOfMemory<T> {
    pub(crate) fn new(inner: T, bytes_needed: u64) -> Self {
        Self {
            inner,
            bytes_needed,
        }
    }

    /// The number of bytes the failed allocation needed.
    pub fn bytes_needed(&self) -> u64 {
        self.bytes_needed
    }

    /// Recover this error's inner host value.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Take this error's inner host value, retaining the `GcHeapOutOfMemory`
    /// with `T` replaced by `()`.
    pub fn take_inner(self) -> (T, GcHeapOutOfMemory<()>) {
        (self.inner, GcHeapOutOfMemory::new((), self.bytes_needed))
    }
}
