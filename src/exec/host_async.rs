//! The async twin of [`super::host`]: drives the resumable core as a `Future`, awaiting
//! async host calls, fuel yields, and async resource limiters. Split out of `host.rs` for
//! the file-size cap; the whole module is `async`-feature-gated (see `exec::mod`).

// `host_index` indexes the store's own `async_host_funcs` (registered together — #33 carve-out).
#![allow(clippy::indexing_slicing)]

use super::epoch::{apply_epoch_deadline_async, yield_now};
use super::host::{enter, finish};
use super::{Execution, Outcome};
use crate::extern_::{Memory, Table};
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::module::code::Code;
use crate::store::{FuncEntity, Store};
use crate::value::{Ref, Val, ValType};
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
    let (mut exec, b) = enter(store, instance, func_index, code, args);
    let outcome = drive_async(&mut exec, store, b.run_stop).await;
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
                exec.gc_pressure_safepoint(&mut store.inner);
            }
            Outcome::FuelYield => {
                yield_now().await;
                store.inner.refuel_from_reserve();
            }
            Outcome::EpochDeadline => {
                if let Err(e) = apply_epoch_deadline_async(store).await {
                    return Err(exec.attach_suspension_backtrace(&store.inner, e));
                }
            }
            Outcome::Grow { memory, delta } => exec.do_grow_async(store, memory, delta).await?,
            Outcome::TableGrow { table, delta, init } => {
                exec.do_grow_table_async(store, table, delta, init).await?;
            }
            // GC reservation growth uses the sync limiter path (errors if an async limiter is
            // installed — combining an async limiter with the GC heap is unsupported for now).
            Outcome::GcGrow {
                reserved_target,
                bytes_needed,
            } => store.grow_gc_reservation(reserved_target, bytes_needed)?,
        }
    }
}

impl Execution {
    /// Async sibling of [`invoke_host`](Self::invoke_host): runs the suspended async host
    /// closure and awaits its future before pushing results. Args/results are owned locals,
    /// so no store borrow is held across the `.await`.
    /// Async sibling of [`invoke_host`](Self::invoke_host), split into sync halves around the
    /// single await. (Fully inlining the await into `drive_async` was tried and measured
    /// perf-neutral — the nested state machine is not where the async boundary's cost is.)
    async fn invoke_host_async<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
        instance: Instance,
        stop_depth: usize,
    ) -> Result<()> {
        let (mut scratch, roots_mark, host_index) = self.prep_host_async(store, func);
        let cb = store.async_host_funcs[host_index as usize].clone();
        // Park the shared execution across the await; contain a host panic across the poll (#33).
        store.inner.swap_exec(self);
        let outcome = {
            let caller = Caller::new(store.as_context_mut(), Some(instance));
            let fut = cb(caller, &scratch.0, &mut scratch.1);
            super::guard::CatchUnwind(std::boxed::Box::into_pin(fut)).await
        };
        store.inner.swap_exec(self);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(payload) => {
                super::guard::restore_after_panic(&mut store.inner, roots_mark);
                super::guard::reraise(payload);
            }
        };
        self.finish_host_async(store, outcome, scratch, roots_mark, stop_depth)
    }

    /// Sync front half of an async host call: decodes args into the reused buffers and returns
    /// everything the await needs. Same shape as `invoke_host`.
    fn prep_host_async<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
    ) -> ((Vec<Val>, Vec<Val>), usize, u32) {
        let (mut params, mut results) = store.inner.take_host_scratch();
        let host_index = match store.inner.func(func) {
            FuncEntity::HostAsync {
                sig, host_index, ..
            } => {
                results.clear();
                results.extend_from_slice(&sig.result_defaults);
                params.clear();
                self.pop_params_into(&sig.params, &mut params);
                *host_index
            }
            _ => unreachable!("HostAsync only suspends on async host funcs"),
        };
        // Scope the host-created GC roots for the call's duration (see `invoke_host`).
        let roots_mark = store.inner.gc_roots_mark();
        ((params, results), roots_mark, host_index)
    }

    /// Sync back half of an async host call: results, scratch return, roots scope, errors.
    fn finish_host_async<T>(
        &mut self,
        store: &mut Store<T>,
        outcome: Result<()>,
        scratch: (Vec<Val>, Vec<Val>),
        roots_mark: usize,
        stop_depth: usize,
    ) -> Result<()> {
        let (params, results) = scratch;
        if let Err(e) = outcome {
            store.inner.gc_roots_truncate(roots_mark);
            return self.host_call_error(&mut store.inner, e, stop_depth);
        }
        // See `invoke_host`: a host that returned normally leaves no pending exception.
        store.inner.take_pending_exception();
        self.push_results_slice(&results);
        store.inner.put_host_scratch(params, results);
        store.inner.gc_roots_truncate(roots_mark);
        Ok(())
    }

    /// Async sibling of [`do_grow`](Self::do_grow): awaits an async resource limiter.
    async fn do_grow_async<T>(
        &mut self,
        store: &mut Store<T>,
        memory: Memory,
        delta: u64,
    ) -> Result<()> {
        let is_64 = store.inner.memory(memory).ty.is_64();
        let old = store.grow_memory_async(memory, delta).await?;
        self.push_index(is_64, old.unwrap_or(u64::MAX));
        Ok(())
    }

    /// Async sibling of [`do_grow_table`](Self::do_grow_table).
    async fn do_grow_table_async<T>(
        &mut self,
        store: &mut Store<T>,
        table: Table,
        delta: u64,
        init: Ref,
    ) -> Result<()> {
        let is_64 = store.inner.table(table).ty.is_64();
        let old = store.grow_table_async(table, delta, init).await?;
        self.push_index(is_64, old.unwrap_or(u64::MAX));
        Ok(())
    }
}
