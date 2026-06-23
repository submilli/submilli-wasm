//! Epoch-deadline servicing for the execution driver: when `run` suspends on an epoch deadline,
//! consult the store's callback and either trap, extend-and-continue, or (async) yield. Split out of
//! the driver (`host`) to keep that file small.

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
fn take_epoch_action<T>(store: &mut Store<T>) -> Result<UpdateDeadline> {
    let mut cb = store.epoch_callback.take();
    let action = match cb.as_mut() {
        Some(f) => f(store.as_context_mut()),
        None => Ok(UpdateDeadline::Interrupt),
    };
    store.epoch_callback = cb;
    action
}

/// Extends the epoch deadline by `delta` ticks beyond the current epoch.
fn extend_epoch_deadline<T>(store: &mut Store<T>, delta: u64) {
    let next = store.inner.engine().current_epoch().saturating_add(delta);
    store.inner.set_epoch_deadline(next);
}

/// Applies the store's epoch-deadline policy in a *sync* context: trap, or
/// extend-and-continue. `Yield` requires async, so it traps here.
pub(super) fn apply_epoch_deadline<T>(store: &mut Store<T>) -> Result<()> {
    match take_epoch_action(store)? {
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
pub(super) async fn apply_epoch_deadline_async<T>(store: &mut Store<T>) -> Result<()> {
    match take_epoch_action(store)? {
        UpdateDeadline::Interrupt => Err(Trap::Interrupt.into()),
        UpdateDeadline::Continue(delta) => {
            extend_epoch_deadline(store, delta);
            Ok(())
        }
        UpdateDeadline::Yield(delta) => {
            yield_now().await;
            extend_epoch_deadline(store, delta);
            Ok(())
        }
    }
}
