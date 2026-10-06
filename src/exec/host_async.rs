//! The async twin of [`super::host`]: drives the resumable core as a `Future`, awaiting
//! async host calls, fuel yields, and async resource limiters. Split out of `host.rs`;
//! the whole module is `async`-feature-gated (see `exec::mod`).

// `host_index` indexes the store's own `async_host_funcs` (registered together — #33 carve-out).
#![allow(clippy::indexing_slicing)]

use super::epoch::{apply_epoch_deadline_async, yield_now};
use super::host::{enter, finish, HostCallState};
use super::{Execution, Outcome};
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::module::code::Code;
use crate::store::{FuncEntity, Store};
use crate::value::{Val, ValType};
use crate::Result;

/// Async sibling of [`execute`]: drives the same resumable core to completion as a
/// `Future`, so the call can be parked under an executor. Mirrors `execute` but awaits
/// async host calls and yields.
pub(crate) async fn execute_async<T>(
    store: &mut Store<T>,
    instance: Instance,
    func_index: u32,
    code: Code,
    args: Vec<Val>,
    result_tys: &[ValType],
) -> Result<Vec<Val>> {
    let (mut exec, b) = enter(store, instance, func_index, code, args)?;
    let roots_mark = store.inner.gc_roots_mark();
    let pending = store.inner.pending_exception();
    let pending_generation = store.inner.pending_exception_generation();
    if let Some(exception) = pending {
        store
            .inner
            .push_gc_root(exception.raw(), crate::canon::RefKind::Exn);
    }
    let token = store.inner.begin_async_call_cleanup(
        Some(roots_mark),
        pending,
        pending_generation,
        Some(crate::store::AsyncCallBoundary {
            value_base: b.value_base,
            stop_depth: b.stop_depth,
        }),
    );
    let mut cancellation = super::guard::AsyncCancellation::new(token);
    let outcome = drive_async(&mut exec, store, b.run_stop).await;
    let cleanup = store
        .inner
        .complete_async_call_cleanup(cancellation.token());
    cancellation.disarm();
    store.inner.gc_roots_truncate(roots_mark);
    let outcome = match cleanup {
        Ok(()) => outcome,
        Err(error) => {
            store
                .inner
                .restore_pending_exception(pending, pending_generation);
            Err(error)
        }
    };
    finish(store, exec, &b, result_tys, outcome)
}

/// Async sibling of [`drive`].
async fn drive_async<T>(exec: &mut Execution, store: &mut Store<T>, run_stop: usize) -> Result<()> {
    loop {
        match exec.run(store, run_stop)? {
            Outcome::Finished => return Ok(()),
            Outcome::HostAsync { func, instance } => {
                exec.invoke_host_async(store, func, instance, run_stop)
                    .await?;
                // The await is the natural long-latency safepoint: other tenants generate
                // engine-wide GC pressure while this guest is parked, so honor the mailbox
                // on resume (sync host calls skip this — their path is tens of ns).
                exec.gc_pressure_safepoint(&mut store.inner)?;
            }
            Outcome::FuelYield => {
                store.inner.swap_exec(exec);
                yield_now().await;
                store.inner.recover_cancelled_async_calls();
                store.inner.swap_exec(exec);
                store.inner.refuel_from_reserve();
            }
            Outcome::EpochDeadline => {
                if let Err(e) = apply_epoch_deadline_async(exec, store).await {
                    return Err(exec.attach_suspension_backtrace(&store.inner, e));
                }
            }
            Outcome::Grow { memory, delta } => {
                let is_64 = store.inner.memory(memory).ty.is_64();
                store.inner.swap_exec(exec);
                let old = store.grow_memory_async(memory, delta).await;
                store.inner.recover_cancelled_async_calls();
                store.inner.swap_exec(exec);
                exec.push_index(is_64, old?.unwrap_or(u64::MAX));
            }
            Outcome::TableGrow { table, delta, init } => {
                let is_64 = store.inner.table(table).ty.is_64();
                store.inner.swap_exec(exec);
                let old = store.grow_table_async(table, delta, init).await;
                store.inner.recover_cancelled_async_calls();
                store.inner.swap_exec(exec);
                exec.push_index(is_64, old?.unwrap_or(u64::MAX));
            }
            // GC reservation growth uses the sync limiter path (errors if an async limiter is
            // installed — combining an async limiter with the GC heap is unsupported for now).
            Outcome::GcGrow {
                reserved_target,
                bytes_needed,
            } => exec.do_grow_gc(store, reserved_target, bytes_needed)?,
        }
    }
}

impl Execution {
    /// Runs a suspended async host closure, split into sync halves around the single await so no
    /// store borrow is held while parked.
    async fn invoke_host_async<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
        instance: Instance,
        stop_depth: usize,
    ) -> Result<()> {
        let mut state = self.prep_host_async(store, func)?;
        let cb = store.async_host_funcs[state.host_index as usize].clone();
        // Park the shared execution across the await; contain a host panic across the poll (#33).
        store.inner.swap_exec(self);
        let guarded = async {
            let caller = Caller::new(store.as_context_mut(), Some(instance));
            Box::into_pin(cb(caller, &state.params, &mut state.results)).await
        };
        let outcome = super::guard::CatchUnwind(Box::pin(guarded)).await;
        let outcome = match outcome {
            Ok(outcome) => {
                store.inner.recover_cancelled_async_calls();
                store.inner.swap_exec(self);
                outcome
            }
            Err(payload) => {
                super::guard::restore_after_panic(
                    &mut store.inner,
                    state.roots_mark,
                    state.pending,
                    state.pending_generation,
                );
                super::guard::reraise(payload);
            }
        };
        if let Some(error) = store.inner.take_internal_error() {
            store
                .inner
                .restore_pending_exception(state.pending, state.pending_generation);
            Self::discard_host_call(store, state);
            return Err(error);
        }
        self.finish_host_call(store, state, outcome, stop_depth)
    }

    /// Sync front half of an async host call: decodes args into the reused buffers and returns
    /// everything the await needs. Same shape as `invoke_host`.
    fn prep_host_async<T>(&mut self, store: &mut Store<T>, func: Func) -> Result<HostCallState> {
        let (host_index, sig) = match store.inner.func(func) {
            FuncEntity::HostAsync {
                sig, host_index, ..
            } => (*host_index, sig.clone()),
            _ => unreachable!("HostAsync only suspends on async host funcs"),
        };
        self.prepare_host_call(store, host_index, sig)
    }
}
