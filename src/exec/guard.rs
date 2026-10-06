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
pub(crate) fn restore_after_panic(
    inner: &mut crate::store::StoreInner,
    roots_mark: usize,
    pending: Option<crate::value::Rooted<crate::value::ExnRef>>,
    pending_generation: u64,
) {
    inner.clear_internal_error();
    #[cfg(feature = "async")]
    inner.recover_cancelled_async_calls();
    let mut exec = inner.take_exec();
    match exec.discard_current_call() {
        Ok((value_base, stop_depth)) => {
            #[cfg(feature = "async")]
            inner.abandon_async_call_boundary(value_base, stop_depth);
            if stop_depth != 0 {
                inner.park_exec(exec);
            }
        }
        Err(error) => {
            if let Some(error) = error.downcast_ref::<crate::error::InternalError>() {
                inner.latch_internal_error(*error);
            }
        }
    }
    inner.gc_roots_truncate(roots_mark);
    inner.restore_pending_exception(pending, pending_generation);
}

/// Restores resources owned by a directly-called host function. There is no execution boundary to
/// discard; an execution parked by an outer callback must remain intact.
pub(crate) fn restore_direct_after_panic(
    inner: &mut crate::store::StoreInner,
    roots_mark: usize,
    pending: Option<crate::value::Rooted<crate::value::ExnRef>>,
    pending_generation: u64,
) {
    #[cfg(feature = "async")]
    inner.recover_cancelled_async_calls();
    inner.gc_roots_truncate(roots_mark);
    inner.restore_pending_exception(pending, pending_generation);
}

/// Marks a suspended async host-call cleanup record if its owning future is dropped while pending.
/// The next mutable store access performs the actual safe cleanup after the callback future has
/// released its store borrow.
#[cfg(feature = "async")]
pub(crate) struct AsyncCancellation {
    token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    armed: bool,
}

#[cfg(feature = "async")]
impl AsyncCancellation {
    pub(crate) fn new(token: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        AsyncCancellation { token, armed: true }
    }

    pub(crate) fn token(&self) -> &std::sync::Arc<std::sync::atomic::AtomicBool> {
        &self.token
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(feature = "async")]
impl Drop for AsyncCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.token.store(true, std::sync::atomic::Ordering::Release);
        }
    }
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
