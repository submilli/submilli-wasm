//! Epoch-deadline servicing for the execution driver: when `run` suspends on an epoch deadline,
//! consult the store's callback and either trap, extend-and-continue, or (async) yield. Split out of
//! the driver (`host`) to keep that file small.

use super::Execution;
use crate::store::{Store, UpdateDeadline};
use crate::trap::Trap;
use crate::Result;

/// A one-shot yield to the async executor: returns `Pending` once (waking immediately),
/// then `Ready`, giving the executor a chance to poll other tasks.
#[cfg(feature = "async")]
pub(super) async fn yield_now() {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct YieldNow(bool);
    impl Future for YieldNow {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
    YieldNow(false).await;
}

/// Invokes the store's epoch-deadline callback (defaulting to `Interrupt`), leaving it
/// reinstalled. The returned `UpdateDeadline` is acted on by the (sync/async) caller.
///
/// The callback is host code holding the store context, so it gets the full `invoke_host`
/// treatment: park the execution (a GC allocation in the callback collects with the guest's
/// live operands as roots — without parking they are invisible and would be freed), scope
/// the GC roots it creates, and contain a panic (#33).
fn take_epoch_action<T>(exec: &mut Execution, store: &mut Store<T>) -> Result<UpdateDeadline> {
    let mut cb = store.epoch_callback.take();
    let Some(f) = cb.as_mut() else {
        return Ok(UpdateDeadline::Interrupt);
    };
    let roots_mark = store.inner.gc_roots_mark();
    let pending = store.inner.pending_exception();
    let pending_generation = store.inner.pending_exception_generation();
    if let Some(exception) = pending {
        store
            .inner
            .push_gc_root(exception.raw(), crate::canon::RefKind::Exn);
    }
    store.inner.swap_exec(exec); // park (see `invoke_host`)
    let action = match super::guard::catch_host(|| f(store.as_context_mut())) {
        Ok(action) => action,
        Err(payload) => {
            super::guard::restore_after_panic(
                &mut store.inner,
                roots_mark,
                pending,
                pending_generation,
            );
            store.epoch_callback = cb;
            super::guard::reraise(payload);
        }
    };
    #[cfg(feature = "async")]
    store.inner.recover_cancelled_async_calls();
    store.inner.swap_exec(exec); // reclaim
    store.inner.gc_roots_truncate(roots_mark);
    store.epoch_callback = cb;
    if let Some(error) = store.inner.take_internal_error() {
        store
            .inner
            .restore_pending_exception(pending, pending_generation);
        return Err(error);
    }
    action
}

/// Extends the epoch deadline by `delta` ticks beyond the current epoch.
fn extend_epoch_deadline<T>(store: &mut Store<T>, delta: u64) {
    let next = store.inner.engine().current_epoch().saturating_add(delta);
    store.inner.set_epoch_deadline(next);
}

/// Applies the store's epoch-deadline policy in a *sync* context: trap, or
/// extend-and-continue. `Yield` requires async, so it traps here.
pub(super) fn apply_epoch_deadline<T>(exec: &mut Execution, store: &mut Store<T>) -> Result<()> {
    match take_epoch_action(exec, store)? {
        UpdateDeadline::Interrupt => Err(Trap::Interrupt.into()),
        UpdateDeadline::Continue(delta) => {
            extend_epoch_deadline(store, delta);
            Ok(())
        }
        #[cfg(feature = "async")]
        UpdateDeadline::Yield(_) => Err(Trap::Interrupt.into()),
    }
}

/// Async epoch-deadline policy: like [`apply_epoch_deadline`] but `Yield(delta)` yields
/// to the executor and then extends the deadline (rather than trapping).
#[cfg(feature = "async")]
pub(super) async fn apply_epoch_deadline_async<T>(
    exec: &mut Execution,
    store: &mut Store<T>,
) -> Result<()> {
    match take_epoch_action(exec, store)? {
        UpdateDeadline::Interrupt => Err(Trap::Interrupt.into()),
        UpdateDeadline::Continue(delta) => {
            extend_epoch_deadline(store, delta);
            Ok(())
        }
        UpdateDeadline::Yield(delta) => {
            store.inner.swap_exec(exec);
            yield_now().await;
            store.inner.recover_cancelled_async_calls();
            store.inner.swap_exec(exec);
            extend_epoch_deadline(store, delta);
            Ok(())
        }
    }
}
