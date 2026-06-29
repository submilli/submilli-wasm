//! Host-call panic containment (#33).
//!
//! A panicking host function must not corrupt the store it ran in: without containment its caller's
//! cleanup — restoring the parked execution, scoped GC roots, and the pending-exception slot — would
//! be skipped on unwind, leaving the store unusable for its next call. We catch the unwind at the
//! boundary, let the caller restore store state, then **re-raise** — matching wasmtime, which catches
//! only to clean up and then resumes the unwind (it does *not* convert a host panic into a trap).
//!
//! Cross-tenant safety (one host-fn panic must not brick *other* stores on the shared engine) is
//! handled separately by the poison-recovering registry lock in `engine.rs`.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Runs a synchronous host closure, capturing a panic as `Err(payload)` instead of unwinding.
pub(crate) fn catch_host<R>(call: impl FnOnce() -> R) -> Result<R, Box<dyn Any + Send>> {
    catch_unwind(AssertUnwindSafe(call))
}

/// Re-raises a contained host-fn panic once the caller has restored store state.
pub(crate) fn reraise(payload: Box<dyn Any + Send>) -> ! {
    std::panic::resume_unwind(payload)
}

/// Restores the store state a re-entrant host call mutated — the parked execution (emptying
/// `exec_slot`), the scoped GC roots, and the pending-exception slot — so the store stays consistent
/// for its next use after a contained host-fn panic (#33), before the panic is [`reraise`]d.
pub(crate) fn restore_after_panic(inner: &mut crate::store::StoreInner, roots_mark: usize) {
    let _ = inner.take_exec();
    inner.gc_roots_truncate(roots_mark);
    inner.take_pending_exception();
}

/// Future adapter that contains a panic from polling `F` (an async host fn's boxed future), yielding
/// `Err(payload)` so the async driver can restore store state before [`reraise`]-ing. `F` is always a
/// `Pin<Box<dyn Future>>` here, hence `Unpin` — no `unsafe` pin projection needed.
#[cfg(feature = "async")]
pub(crate) struct CatchUnwind<F>(pub(crate) F);

#[cfg(feature = "async")]
impl<F: std::future::Future + Unpin> std::future::Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use std::task::Poll;
        let inner = &mut self.get_mut().0;
        match catch_unwind(AssertUnwindSafe(|| std::pin::Pin::new(inner).poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(v)) => Poll::Ready(Ok(v)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    }
}
