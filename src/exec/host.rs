//! The generic execution driver: runs the (non-generic) interpreter core and
//! services host-function suspensions, which need the typed `Store<T>` to build a
//! `Caller<'_, T>`. Keeping this thin and `T`-generic isolates the data type from
//! the interpreter loop. See ARCHITECTURE §7/§10.

use std::sync::Arc;

use super::epoch::apply_epoch_deadline;
#[cfg(feature = "async")]
use super::epoch::{apply_epoch_deadline_async, yield_now};
use super::exn::surface_exception;
use super::frame::Delimiter;
use super::{cell, Execution, Outcome};
use crate::exception::ThrownException;
use crate::extern_::{Memory, Table};
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::module::op::CompiledFunc;
use crate::store::{FuncEntity, Store};
use crate::value::{Ref, Val, ValType};
use crate::Result;

/// Decodes the final operand cells back to public `Val`s using the entry function's result types
/// (the stack is untyped; the caller's signature supplies the types — see `cell`).
fn decode_results(result_tys: &[ValType], cells: Vec<cell::Cell>) -> Vec<Val> {
    result_tys
        .iter()
        .zip(cells)
        .map(|(t, c)| cell::decode(c, t))
        .collect()
}

/// The boundary state of one (top-level or re-entrant) call on the shared execution: where its
/// operands begin (`value_base`), the parked outer frame depth (`stop_depth`), and the depth `run`
/// stops at (`run_stop`, one above the delimiter).
struct Boundary {
    value_base: usize,
    stop_depth: usize,
    run_stop: usize,
}

/// Takes the shared execution (fresh if none is parked), pushes this call's boundary + args + entry
/// frame, and returns the execution alongside its [`Boundary`]. The delimiter is a host re-entry
/// when outer frames are already parked, else the top-level entry.
fn enter<T>(
    store: &mut Store<T>,
    instance: Instance,
    func_index: u32,
    code: Arc<CompiledFunc>,
    args: Vec<Val>,
) -> (Execution, Boundary) {
    let mut exec = store.inner.take_exec().unwrap_or_default();
    let value_base = exec.values.len();
    let stop_depth = exec.frames.len();
    let delim = if stop_depth == 0 {
        Delimiter::TopLevel
    } else {
        Delimiter::HostReentry
    };
    exec.enter_call(delim, instance, func_index, code, args);
    let b = Boundary {
        value_base,
        stop_depth,
        run_stop: stop_depth + 1,
    };
    (exec, b)
}

/// Closes out a (sub-)call: extracts results (or restores the stacks on error), re-parks the shared
/// execution for the outer call to resume (top-level drops it), and surfaces any error.
fn finish<T>(
    store: &mut Store<T>,
    mut exec: Execution,
    b: &Boundary,
    result_tys: &[ValType],
    outcome: Result<()>,
) -> Result<Vec<Val>> {
    let result = match outcome {
        Ok(()) => Ok(decode_results(
            result_tys,
            exec.take_results(b.value_base, b.stop_depth),
        )),
        Err(e) => {
            exec.discard_to(b.value_base, b.stop_depth);
            Err(surface_exception(&mut store.inner, e))
        }
    };
    // A top-level call (no parked outer frames) drops the execution; a re-entry re-parks it so the
    // outer driver resumes on the same — now restored — stacks.
    if b.stop_depth != 0 {
        store.inner.park_exec(exec);
    }
    result
}

/// Runs `code` (of `instance`) with `args`, servicing host calls, and returns the
/// results. The wasm core runs on `&mut store.inner`; only host calls touch `T`.
pub(crate) fn execute<T>(
    store: &mut Store<T>,
    instance: Instance,
    func_index: u32,
    code: Arc<CompiledFunc>,
    args: Vec<Val>,
    result_tys: &[ValType],
) -> Result<Vec<Val>> {
    let (mut exec, b) = enter(store, instance, func_index, code, args);
    let outcome = drive(&mut exec, store, b.run_stop);
    finish(store, exec, &b, result_tys, outcome)
}

/// Drives the resumable core to completion (sync), servicing host calls and grow suspensions.
/// Returns `Ok(())` when the call finishes (results left on `exec` above its `value_base`); any
/// error propagates raw, to be surfaced + cleaned up by [`finish`].
fn drive<T>(exec: &mut Execution, store: &mut Store<T>, run_stop: usize) -> Result<()> {
    loop {
        match exec.run(&mut store.inner, run_stop)? {
            Outcome::Finished => return Ok(()),
            Outcome::HostCall { func, instance } => {
                exec.invoke_host(store, func, instance, run_stop)?;
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
            Outcome::GcGrow {
                reserved_target,
                bytes_needed,
            } => store.grow_gc_reservation(reserved_target, bytes_needed)?,
        }
    }
}

/// Async sibling of [`execute`]: drives the same resumable core to completion as a
/// `Future`, so the call can be parked under an executor. Mirrors `execute` but awaits
/// async host calls and yields.
#[cfg(feature = "async")]
pub(crate) async fn execute_async<T>(
    store: &mut Store<T>,
    instance: Instance,
    func_index: u32,
    code: Arc<CompiledFunc>,
    args: Vec<Val>,
    result_tys: &[ValType],
) -> Result<Vec<Val>> {
    let (mut exec, b) = enter(store, instance, func_index, code, args);
    let outcome = drive_async(&mut exec, store, b.run_stop).await;
    finish(store, exec, &b, result_tys, outcome)
}

/// Async sibling of [`drive`].
#[cfg(feature = "async")]
async fn drive_async<T>(exec: &mut Execution, store: &mut Store<T>, run_stop: usize) -> Result<()> {
    loop {
        match exec.run(&mut store.inner, run_stop)? {
            Outcome::Finished => return Ok(()),
            Outcome::HostCall { func, instance } => {
                exec.invoke_host(store, func, instance, run_stop)?;
            }
            Outcome::HostAsync { func, instance } => {
                exec.invoke_host_async(store, func, instance, run_stop)
                    .await?;
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
    /// Invokes a suspended host function: pops its args off the operand stack,
    /// runs the closure with a `Caller`, and pushes the results back. A host `Err`
    /// propagates as the call's trap/error.
    fn invoke_host<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
        instance: Instance,
        stop_depth: usize,
    ) -> Result<()> {
        let (param_tys, mut results, host_index) = match store.inner.func(func) {
            FuncEntity::Host { ty, host_index } => (
                ty.params().collect::<Vec<ValType>>(),
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
        // Scope the host roots created during the call (the GC host API registers a root per
        // `StructRef`/`ArrayRef`/`ExternRef` it builds). Without this they accumulate for the
        // store's life — a host fn that builds a GC object per call (e.g. one string per line) would
        // pin every one, defeating collection. Returned values survive via `push_results` (operand
        // roots); anything the host stored into a global/table/pending-exception is rooted there.
        let roots_mark = store.inner.gc_roots_mark();
        let params = self.pop_params(&param_tys);
        let cb = store.host_funcs[host_index as usize].clone();
        // Park the shared execution so a host fn that re-enters wasm (`Func::call`) runs on these
        // same stacks; reclaim it after the call (a re-entrant call re-parks it on its way out). The
        // guest's live operands stay reachable for GC while parked — the collector seeds from the
        // slot (`StoreInner::exec_roots`) on a host-triggered collection.
        store.inner.park_exec(std::mem::take(self));
        let result = cb(
            Caller::new(store.as_context_mut(), Some(instance)),
            &params,
            &mut results,
        );
        *self = store
            .inner
            .take_exec()
            .expect("re-entrant call must re-park the shared execution");
        if let Err(e) = result {
            store.inner.gc_roots_truncate(roots_mark);
            return self.host_call_error(&mut store.inner, e, stop_depth);
        }
        // The host returned normally, so it did not throw. Drop any exception it set via
        // `Store::throw` but swallowed instead of propagating (host misuse) — the pending slot is
        // scoped to a single host call and must be empty once one completes without throwing.
        store.inner.take_pending_exception();
        self.push_results(results);
        store.inner.gc_roots_truncate(roots_mark); // results are now operand-rooted
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
        stop_depth: usize,
    ) -> Result<()> {
        if e.is::<ThrownException>() {
            if let Some(exn) = inner.take_pending_exception() {
                return self.raise_host_exception(inner, exn, stop_depth);
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
        stop_depth: usize,
    ) -> Result<()> {
        let (param_tys, mut results, host_index) = match store.inner.func(func) {
            FuncEntity::HostAsync { ty, host_index } => (
                ty.params().collect::<Vec<ValType>>(),
                ty.results()
                    .map(|t| Val::default_for_valtype(&t))
                    .collect::<Vec<_>>(),
                *host_index,
            ),
            _ => unreachable!("HostAsync only suspends on async host funcs"),
        };
        // Scope the host-created GC roots for the call's duration (see `invoke_host`).
        let roots_mark = store.inner.gc_roots_mark();
        let params = self.pop_params(&param_tys);
        let cb = store.async_host_funcs[host_index as usize].clone();
        // Park the shared execution across the await (see `invoke_host`).
        store.inner.park_exec(std::mem::take(self));
        let outcome = {
            let caller = Caller::new(store.as_context_mut(), Some(instance));
            let fut = cb(caller, &params, &mut results);
            std::boxed::Box::into_pin(fut).await
        };
        *self = store
            .inner
            .take_exec()
            .expect("re-entrant call must re-park the shared execution");
        if let Err(e) = outcome {
            store.inner.gc_roots_truncate(roots_mark);
            return self.host_call_error(&mut store.inner, e, stop_depth);
        }
        // See `invoke_host`: a host that returned normally leaves no pending exception.
        store.inner.take_pending_exception();
        self.push_results(results);
        store.inner.gc_roots_truncate(roots_mark);
        Ok(())
    }

    /// Services a suspended `memory.grow`: consults the limiter and pushes the new
    /// page count, or `-1` on a soft failure (a trap propagates from `grow_memory`).
    fn do_grow<T>(&mut self, store: &mut Store<T>, memory: Memory, delta: u64) -> Result<()> {
        let is_64 = store.inner.memory(memory).ty.is_64();
        let old = store.grow_memory(memory, delta)?;
        self.push_index(is_64, old.unwrap_or(u64::MAX)); // soft-fail → -1 in either width
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
        let is_64 = store.inner.memory(memory).ty.is_64();
        let old = store.grow_memory_async(memory, delta).await?;
        self.push_index(is_64, old.unwrap_or(u64::MAX));
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
        let is_64 = store.inner.table(table).ty.is_64();
        let old = store.grow_table(table, delta, init)?;
        self.push_index(is_64, old.unwrap_or(u64::MAX));
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
        let is_64 = store.inner.table(table).ty.is_64();
        let old = store.grow_table_async(table, delta, init).await?;
        self.push_index(is_64, old.unwrap_or(u64::MAX));
        Ok(())
    }
}
