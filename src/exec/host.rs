//! The generic execution driver: runs the (non-generic) interpreter core and
//! services host-function suspensions, which need the typed `Store<T>` to build a
//! `Caller<'_, T>`. Keeping this thin and `T`-generic isolates the data type from
//! the interpreter loop. See ARCHITECTURE §7/§10.

// Indexing is `{async_,}host_funcs[host_index]` with `host_index` from a validated entity (#33).
#![allow(clippy::indexing_slicing)]

use super::epoch::apply_epoch_deadline;
use super::exn::surface_exception;
use super::frame::Delimiter;
use super::{cell, Execution, Outcome};
use crate::canon::RefKind;
use crate::exception::ThrownException;
use crate::extern_::{Memory, Table};
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::module::code::Code;
use crate::store::{FuncEntity, Store, StoreInner};
use crate::value::{Ref, Val, ValType};
use crate::Result;

/// Roots every reference param for the host call's duration. `pop_params_into` removed them
/// from the operand root shadow, so without this a collection triggered from inside the call
/// (any host-side allocation can hit the GC budget) would free an object the host still holds.
/// Registered after the call's `roots_mark`, so the existing truncate unwinds them together
/// with the call's own host-created roots.
pub(super) fn root_ref_params(inner: &mut StoreInner, params: &[Val]) {
    for v in params {
        match v {
            Val::AnyRef(Some(r)) => inner.push_gc_root(r.raw(), RefKind::Any),
            Val::ExternRef(Some(r)) => inner.push_gc_root(r.raw(), RefKind::Extern),
            Val::ExnRef(Some(r)) => inner.push_gc_root(r.raw(), RefKind::Exn),
            // Funcs are store entities (never collected); numerics and nulls carry no referent.
            _ => {}
        }
    }
}

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
pub(super) struct Boundary {
    pub(super) value_base: usize,
    pub(super) stop_depth: usize,
    pub(super) run_stop: usize,
}

/// Takes the shared execution (fresh if none is parked), pushes this call's boundary + args + entry
/// frame, and returns the execution alongside its [`Boundary`]. The delimiter is a host re-entry
/// when outer frames are already parked, else the top-level entry.
pub(super) fn enter<T>(
    store: &mut Store<T>,
    instance: Instance,
    func_index: u32,
    code: Code,
    args: Vec<Val>,
) -> (Execution, Boundary) {
    let mut exec = store.inner.take_exec();
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
pub(super) fn finish<T>(
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
    code: Code,
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
        match exec.run(store, run_stop)? {
            Outcome::Finished => return Ok(()),
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
            Outcome::EpochDeadline => {
                if let Err(e) = apply_epoch_deadline(exec, store) {
                    return Err(exec.attach_suspension_backtrace(&store.inner, e));
                }
            }
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

impl Execution {
    /// Invokes a suspended host function: pops its args off the operand stack,
    /// runs the closure with a `Caller`, and pushes the results back. A host `Err`
    /// propagates as the call's trap/error.
    // `inline(never)`: this body (buffers, catch_unwind, Caller, error paths) is called from
    // *inside* the dispatch loop now — letting it inline there wrecks the loop's code layout
    // (measured ~2x slower across all workloads when it did).
    #[inline(never)]
    pub(super) fn invoke_host<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
        instance: Instance,
        stop_depth: usize,
    ) -> Result<()> {
        // Reused arg/result buffers (returned below): with the cached signature, the whole
        // boundary is allocation-free in steady state. A re-entrant host call takes a fresh
        // (empty) pair — only nesting allocates. Taken *first* so the signature borrow below
        // needs no `Arc` bump.
        let (mut params, mut results) = store.inner.take_host_scratch();
        let host_index = match store.inner.func(func) {
            FuncEntity::Host {
                sig, host_index, ..
            } => {
                results.clear();
                results.extend_from_slice(&sig.result_defaults);
                params.clear();
                self.pop_params_into(&sig.params, &mut params);
                *host_index
            }
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
        root_ref_params(&mut store.inner, &params);
        let cb = store.host_funcs[host_index as usize].clone();
        // Park the shared execution so a host fn that re-enters wasm (`Func::call`) runs on these
        // same stacks; reclaim it after the call (a re-entrant call re-parks it on its way out). The
        // guest's live operands stay reachable for GC while parked — the collector seeds from the
        // slot (`StoreInner::exec_roots`) on a host-triggered collection.
        store.inner.swap_exec(self); // park (self becomes the slot's empty execution)
                                     // Contain a host-fn panic (#33): catch, restore store state, re-raise. See `guard`.
        let result = match super::guard::catch_host(|| {
            cb(
                Caller::new(store.as_context_mut(), Some(instance)),
                &params,
                &mut results,
            )
        }) {
            Ok(result) => result,
            Err(payload) => {
                super::guard::restore_after_panic(&mut store.inner, roots_mark);
                super::guard::reraise(payload);
            }
        };
        store.inner.swap_exec(self); // reclaim (re-entrant calls re-parked through the slot)
        if let Err(e) = result {
            store.inner.gc_roots_truncate(roots_mark);
            return self.host_call_error(&mut store.inner, e, stop_depth);
        }
        // The host returned normally, so it did not throw. Drop any exception it set via
        // `Store::throw` but swallowed instead of propagating (host misuse) — the pending slot is
        // scoped to a single host call and must be empty once one completes without throwing.
        store.inner.take_pending_exception();
        self.push_results_slice(&results);
        store.inner.put_host_scratch(params, results);
        store.inner.gc_roots_truncate(roots_mark); // results are now operand-rooted
        Ok(())
    }

    /// Handles a host function's `Err`. A host *throw* both returns `ThrownException` **and** leaves a
    /// pending exception (`Store::throw`); only that combination re-enters the guest's handlers. Any
    /// other host error — or a `ThrownException` with no pending exception — propagates as an ordinary
    /// error that `try_table` must not catch. Keying on the error type (not just the slot) keeps an
    /// unrelated error, or a stale pending from an undrained earlier exception, from being mistaken
    /// for a throw.
    pub(super) fn host_call_error(
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

    /// Services a suspended `memory.grow`: consults the limiter and pushes the new
    /// page count, or `-1` on a soft failure (a trap propagates from `grow_memory`).
    fn do_grow<T>(&mut self, store: &mut Store<T>, memory: Memory, delta: u64) -> Result<()> {
        let is_64 = store.inner.memory(memory).ty.is_64();
        let old = store.grow_memory(memory, delta)?;
        self.push_index(is_64, old.unwrap_or(u64::MAX)); // soft-fail → -1 in either width
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
}
