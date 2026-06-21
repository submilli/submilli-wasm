//! The generic execution driver: runs the (non-generic) interpreter core and
//! services host-function suspensions, which need the typed `Store<T>` to build a
//! `Caller<'_, T>`. Keeping this thin and `T`-generic isolates the data type from
//! the interpreter loop. See ARCHITECTURE §7/§10.

use std::sync::Arc;

use super::exn::surface_exception;
use super::{Execution, Outcome};
use crate::exception::ThrownException;
use crate::extern_::{Memory, Table};
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::module::op::CompiledFunc;
use crate::store::{FuncEntity, Store, UpdateDeadline};
use crate::trap::Trap;
use crate::value::{Ref, Val};
use crate::Result;

/// Runs `code` (of `instance`) with `args`, servicing host calls, and returns the
/// results. The wasm core runs on `&mut store.inner`; only host calls touch `T`.
pub(crate) fn execute<T>(
    store: &mut Store<T>,
    instance: Instance,
    code: Arc<CompiledFunc>,
    args: Vec<Val>,
) -> Result<Vec<Val>> {
    let mut exec = Execution {
        values: args,
        frames: Vec::new(),
    };
    exec.push_call(instance, code);
    loop {
        let outcome = match exec.run(&mut store.inner) {
            Ok(o) => o,
            Err(e) => return Err(surface_exception(&mut store.inner, e)),
        };
        match outcome {
            Outcome::Finished => return Ok(exec.values),
            Outcome::HostCall { func, instance } => {
                if let Err(e) = exec.invoke_host(store, func, instance) {
                    return Err(surface_exception(&mut store.inner, e));
                }
            }
            #[cfg(feature = "async")]
            Outcome::HostAsync { .. } => {
                return Err(crate::Error::msg(
                    "async host function called from a synchronous context",
                ))
            }
            #[cfg(feature = "async")]
            Outcome::FuelYield => {
                return Err(crate::Error::msg("fuel yield requires an async store"))
            }
            Outcome::EpochDeadline => apply_epoch_deadline(store)?,
            Outcome::Grow { memory, delta } => exec.do_grow(store, memory, delta)?,
            Outcome::TableGrow { table, delta, init } => {
                exec.do_grow_table(store, table, delta, init)?;
            }
        }
    }
}

/// Async sibling of [`execute`]: drives the same resumable core to completion as a
/// `Future`, so the call can be parked under an executor. Mirrors `execute`'s loop and
/// reuses the same (sync) servicing helpers; async host calls are awaited here.
#[cfg(feature = "async")]
pub(crate) async fn execute_async<T>(
    store: &mut Store<T>,
    instance: Instance,
    code: Arc<CompiledFunc>,
    args: Vec<Val>,
) -> Result<Vec<Val>> {
    let mut exec = Execution {
        values: args,
        frames: Vec::new(),
    };
    exec.push_call(instance, code);
    loop {
        let outcome = match exec.run(&mut store.inner) {
            Ok(o) => o,
            Err(e) => return Err(surface_exception(&mut store.inner, e)),
        };
        match outcome {
            Outcome::Finished => return Ok(exec.values),
            Outcome::HostCall { func, instance } => {
                if let Err(e) = exec.invoke_host(store, func, instance) {
                    return Err(surface_exception(&mut store.inner, e));
                }
            }
            Outcome::HostAsync { func, instance } => {
                if let Err(e) = exec.invoke_host_async(store, func, instance).await {
                    return Err(surface_exception(&mut store.inner, e));
                }
            }
            Outcome::FuelYield => {
                yield_now().await;
                store.inner.refuel_from_reserve();
            }
            Outcome::EpochDeadline => apply_epoch_deadline_async(store).await?,
            Outcome::Grow { memory, delta } => exec.do_grow_async(store, memory, delta).await?,
            Outcome::TableGrow { table, delta, init } => {
                exec.do_grow_table_async(store, table, delta, init).await?;
            }
        }
    }
}

/// A one-shot yield to the async executor: returns `Pending` once (waking immediately),
/// then `Ready`, giving the executor a chance to poll other tasks.
#[cfg(feature = "async")]
async fn yield_now() {
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
fn apply_epoch_deadline<T>(store: &mut Store<T>) -> Result<()> {
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
async fn apply_epoch_deadline_async<T>(store: &mut Store<T>) -> Result<()> {
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

impl Execution {
    /// Invokes a suspended host function: pops its args off the operand stack,
    /// runs the closure with a `Caller`, and pushes the results back. A host `Err`
    /// propagates as the call's trap/error.
    fn invoke_host<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
        instance: Instance,
    ) -> Result<()> {
        let (n_params, mut results, host_index) = match store.inner.func(func) {
            FuncEntity::Host { ty, host_index } => (
                ty.params().len(),
                ty.results()
                    .map(|t| Val::default_for_valtype(&t))
                    .collect::<Vec<_>>(),
                *host_index,
            ),
            FuncEntity::Wasm { .. } => unreachable!("HostCall only suspends on sync host funcs"),
            #[cfg(feature = "async")]
            FuncEntity::HostAsync { .. } => {
                unreachable!("HostCall only suspends on sync host funcs")
            }
        };
        let params = self.values.split_off(self.values.len() - n_params);
        let cb = store.host_funcs[host_index as usize].clone();
        if let Err(e) = cb(
            Caller::new(store.as_context_mut(), Some(instance)),
            &params,
            &mut results,
        ) {
            return self.host_call_error(&mut store.inner, e);
        }
        // The host returned normally, so it did not throw. Drop any exception it set via
        // `Store::throw` but swallowed instead of propagating (host misuse) — the pending slot is
        // scoped to a single host call and must be empty once one completes without throwing.
        store.inner.take_pending_exception();
        self.values.extend(results);
        Ok(())
    }

    /// Handles a host function's `Err`. A host *throw* both returns `ThrownException` **and** leaves a
    /// pending exception (`Store::throw`); only that combination re-enters the guest's handlers. Any
    /// other host error — or a `ThrownException` with no pending exception — propagates as an ordinary
    /// error that `try_table` must not catch. Keying on the error type (not just the slot) keeps an
    /// unrelated error, or a stale pending from an undrained earlier exception, from being mistaken
    /// for a throw.
    fn host_call_error(
        &mut self,
        inner: &mut crate::store::StoreInner,
        e: crate::Error,
    ) -> Result<()> {
        if e.is::<ThrownException>() {
            if let Some(exn) = inner.take_pending_exception() {
                return self.raise_host_exception(inner, exn);
            }
        }
        Err(e)
    }

    /// Async sibling of [`invoke_host`](Self::invoke_host): runs the suspended async host
    /// closure and awaits its future before pushing results. Args/results are owned locals,
    /// so no store borrow is held across the `.await`.
    #[cfg(feature = "async")]
    async fn invoke_host_async<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
        instance: Instance,
    ) -> Result<()> {
        let (n_params, mut results, host_index) = match store.inner.func(func) {
            FuncEntity::HostAsync { ty, host_index } => (
                ty.params().len(),
                ty.results()
                    .map(|t| Val::default_for_valtype(&t))
                    .collect::<Vec<_>>(),
                *host_index,
            ),
            _ => unreachable!("HostAsync only suspends on async host funcs"),
        };
        let params = self.values.split_off(self.values.len() - n_params);
        let cb = store.async_host_funcs[host_index as usize].clone();
        let outcome = {
            let caller = Caller::new(store.as_context_mut(), Some(instance));
            let fut = cb(caller, &params, &mut results);
            std::boxed::Box::into_pin(fut).await
        };
        if let Err(e) = outcome {
            return self.host_call_error(&mut store.inner, e);
        }
        // See `invoke_host`: a host that returned normally leaves no pending exception.
        store.inner.take_pending_exception();
        self.values.extend(results);
        Ok(())
    }

    /// Services a suspended `memory.grow`: consults the limiter and pushes the new
    /// page count, or `-1` on a soft failure (a trap propagates from `grow_memory`).
    fn do_grow<T>(&mut self, store: &mut Store<T>, memory: Memory, delta: u64) -> Result<()> {
        let result = match store.grow_memory(memory, delta)? {
            Some(old) => old as i32,
            None => -1,
        };
        self.push(Val::I32(result));
        Ok(())
    }

    /// Async sibling of [`do_grow`](Self::do_grow): awaits an async resource limiter.
    #[cfg(feature = "async")]
    async fn do_grow_async<T>(
        &mut self,
        store: &mut Store<T>,
        memory: Memory,
        delta: u64,
    ) -> Result<()> {
        let result = match store.grow_memory_async(memory, delta).await? {
            Some(old) => old as i32,
            None => -1,
        };
        self.push(Val::I32(result));
        Ok(())
    }

    /// Services a suspended `table.grow`: consults the limiter and pushes the old size,
    /// or `-1` on a soft failure (a trap propagates from `grow_table`).
    fn do_grow_table<T>(
        &mut self,
        store: &mut Store<T>,
        table: Table,
        delta: u64,
        init: Ref,
    ) -> Result<()> {
        let result = match store.grow_table(table, delta, init)? {
            Some(old) => old as i32,
            None => -1,
        };
        self.push(Val::I32(result));
        Ok(())
    }

    /// Async sibling of [`do_grow_table`](Self::do_grow_table).
    #[cfg(feature = "async")]
    async fn do_grow_table_async<T>(
        &mut self,
        store: &mut Store<T>,
        table: Table,
        delta: u64,
        init: Ref,
    ) -> Result<()> {
        let result = match store.grow_table_async(table, delta, init).await? {
            Some(old) => old as i32,
            None => -1,
        };
        self.push(Val::I32(result));
        Ok(())
    }
}
