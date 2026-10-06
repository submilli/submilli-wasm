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
use crate::error::InternalError;
use crate::exception::ThrownException;
use crate::extern_::{Memory, Table};
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::module::code::Code;
use crate::store::{FuncEntity, Store, StoreInner};
use crate::value::{Ref, Val, ValType};
use crate::Result;

/// Resources owned by one suspended host call. Keeping them together makes every success, error,
/// panic, and async suspension path restore the same roots, pending exception, and scratch buffers.
pub(super) struct HostCallState {
    pub(super) params: Vec<Val>,
    pub(super) results: Vec<Val>,
    pub(super) roots_mark: usize,
    pub(super) pending: Option<crate::value::Rooted<crate::value::ExnRef>>,
    pub(super) pending_generation: u64,
    pub(super) host_index: u32,
    pub(super) sig: std::sync::Arc<crate::store::HostSig>,
}

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
fn decode_results(
    inner: &mut StoreInner,
    result_tys: &[ValType],
    cells: Vec<cell::Cell>,
) -> Result<Vec<Val>> {
    if result_tys.len() != cells.len() {
        return Err(InternalError::ResultShape("wrong number of execution results").into());
    }
    let decoded: Vec<Val> = result_tys
        .iter()
        .zip(cells)
        .map(|(t, c)| cell::decode(c, t))
        .collect();
    if let Err(error) = crate::extern_::ensure_values_match(
        inner,
        &decoded,
        result_tys,
        "execution attempted to return an incompatible value",
    ) {
        return Err(error
            .downcast_ref::<InternalError>()
            .copied()
            .unwrap_or(InternalError::ResultShape(
                "execution returned an invalid or dangling value",
            ))
            .into());
    }

    let roots_mark = inner.gc_roots_mark();
    let result: Result<Vec<Val>> = decoded
        .into_iter()
        .map(|value| {
            inner.root_host_result(value).map_err(|error| {
                error
                    .downcast_ref::<InternalError>()
                    .copied()
                    .unwrap_or(InternalError::GcMetadata(
                        "execution result referenced invalid GC state",
                    ))
                    .into()
            })
        })
        .collect();
    if result.is_err() {
        inner.gc_roots_truncate(roots_mark);
    }
    result
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
) -> Result<(Execution, Boundary)> {
    let error = if store.inner.has_parked_execution() {
        store.inner.internal_error()
    } else {
        store.inner.take_internal_error()
    };
    if let Some(error) = error {
        return Err(error);
    }
    let mut exec = store.inner.take_exec();
    let value_base = exec.values.len();
    let stop_depth = exec.frames.len();
    let delim = if stop_depth == 0 {
        Delimiter::TopLevel
    } else {
        Delimiter::HostReentry
    };
    if let Err(error) = exec.enter_call(delim, instance, func_index, code, args) {
        if stop_depth != 0 {
            store.inner.park_exec(exec);
        }
        return Err(error);
    }
    let b = Boundary {
        value_base,
        stop_depth,
        run_stop: stop_depth.checked_add(1).ok_or(InternalError::FrameStack(
            "execution boundary depth overflow",
        ))?,
    };
    Ok((exec, b))
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
        Ok(()) => match exec.take_results(b.value_base, b.stop_depth, result_tys) {
            Ok(cells) => decode_results(&mut store.inner, result_tys, cells),
            Err(error) => match exec.discard_to(b.value_base, b.stop_depth) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            },
        },
        Err(e) => match exec.discard_to(b.value_base, b.stop_depth) {
            Ok(()) => Err(surface_exception(&mut store.inner, e)),
            Err(cleanup_error) => Err(cleanup_error),
        },
    };
    // A top-level call (no parked outer frames) drops the execution; a re-entry re-parks it so the
    // outer driver resumes on the same — now restored — stacks.
    if b.stop_depth != 0 {
        if let Err(error) = &result {
            if let Some(error) = error.downcast_ref::<crate::error::InternalError>() {
                store.inner.latch_internal_error(*error);
            }
        }
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
    let (mut exec, b) = enter(store, instance, func_index, code, args)?;
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
            } => exec.do_grow_gc(store, reserved_target, bytes_needed)?,
        }
    }
}

impl Execution {
    fn prep_host<T>(&mut self, store: &mut Store<T>, func: Func) -> Result<HostCallState> {
        let (host_index, sig) = match store.inner.func(func) {
            FuncEntity::Host {
                sig, host_index, ..
            } => (*host_index, sig.clone()),
            FuncEntity::Wasm { .. } => unreachable!("HostCall only suspends on sync host funcs"),
            #[cfg(feature = "async")]
            FuncEntity::HostAsync { .. } => {
                unreachable!("HostCall only suspends on sync host funcs")
            }
        };
        self.prepare_host_call(store, host_index, sig)
    }

    pub(super) fn prepare_host_call<T>(
        &mut self,
        store: &mut Store<T>,
        host_index: u32,
        sig: std::sync::Arc<crate::store::HostSig>,
    ) -> Result<HostCallState> {
        let (mut params, mut results) = store.inner.take_host_scratch();
        results.clear();
        results.extend_from_slice(&sig.result_defaults);
        params.clear();
        if let Err(error) = self.pop_params_into(&sig.params, &mut params) {
            store.inner.put_host_scratch(params, results);
            return Err(error);
        }
        let roots_mark = store.inner.gc_roots_mark();
        let pending = store.inner.pending_exception();
        let pending_generation = store.inner.pending_exception_generation();
        if let Some(exception) = pending {
            store.inner.push_gc_root(exception.raw(), RefKind::Exn);
        }
        root_ref_params(&mut store.inner, &params);
        Ok(HostCallState {
            params,
            results,
            roots_mark,
            pending,
            pending_generation,
            host_index,
            sig,
        })
    }

    pub(super) fn discard_host_call<T>(store: &mut Store<T>, state: HostCallState) {
        store.inner.gc_roots_truncate(state.roots_mark);
        store.inner.put_host_scratch(state.params, state.results);
    }

    pub(super) fn finish_host_call<T>(
        &mut self,
        store: &mut Store<T>,
        state: HostCallState,
        outcome: Result<()>,
        stop_depth: usize,
    ) -> Result<()> {
        if let Err(error) = outcome {
            let fresh_throw = error.is::<ThrownException>()
                && store.inner.pending_exception_generation() != state.pending_generation;
            if fresh_throw {
                let prior_pending = state.pending;
                let prior_generation = state.pending_generation;
                let Some(exn) = store.inner.take_pending_exception() else {
                    store
                        .inner
                        .restore_pending_exception(prior_pending, prior_generation);
                    Self::discard_host_call(store, state);
                    return Err(InternalError::ResultShape(
                        "host throw marker had no pending exception",
                    )
                    .into());
                };
                Self::discard_host_call(store, state);
                store
                    .inner
                    .restore_pending_exception(prior_pending, prior_generation);
                return self.raise_host_exception(&mut store.inner, exn, stop_depth);
            }
            Self::discard_host_call(store, state);
            return Err(error);
        }
        if let Err(error) = crate::extern_::ensure_values_match(
            &store.inner,
            &state.results,
            &state.sig.results,
            "function attempted to return an incompatible value",
        ) {
            store
                .inner
                .restore_pending_exception(state.pending, state.pending_generation);
            Self::discard_host_call(store, state);
            return Err(error);
        }
        self.push_results_slice(&state.results);
        Self::discard_host_call(store, state);
        Ok(())
    }

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
        let mut state = self.prep_host(store, func)?;
        let cb = store.host_funcs[state.host_index as usize].clone();
        // Park the shared execution so a host fn that re-enters wasm (`Func::call`) runs on these
        // same stacks; reclaim it after the call (a re-entrant call re-parks it on its way out). The
        // guest's live operands stay reachable for GC while parked — the collector seeds from the
        // slot (`StoreInner::exec_roots`) on a host-triggered collection.
        store.inner.swap_exec(self); // park (self becomes the slot's empty execution)
                                     // Contain a host-fn panic (#33): catch, restore store state, re-raise. See `guard`.
        let result = match super::guard::catch_host(|| {
            cb(
                Caller::new(store.as_context_mut(), Some(instance)),
                &state.params,
                &mut state.results,
            )
        }) {
            Ok(result) => result,
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
        #[cfg(feature = "async")]
        store.inner.recover_cancelled_async_calls();
        store.inner.swap_exec(self); // reclaim (re-entrant calls re-parked through the slot)
        if let Some(error) = store.inner.take_internal_error() {
            store
                .inner
                .restore_pending_exception(state.pending, state.pending_generation);
            Self::discard_host_call(store, state);
            return Err(error);
        }
        self.finish_host_call(store, state, result, stop_depth)
    }

    /// Services a suspended `memory.grow`: consults the limiter and pushes the new
    /// page count, or `-1` on a soft failure (a trap propagates from `grow_memory`).
    fn do_grow<T>(&mut self, store: &mut Store<T>, memory: Memory, delta: u64) -> Result<()> {
        let is_64 = store.inner.memory(memory).ty.is_64();
        let old =
            self.with_parked_host_operation(store, |store| store.grow_memory(memory, delta))?;
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
        let old =
            self.with_parked_host_operation(store, |store| store.grow_table(table, delta, init))?;
        self.push_index(is_64, old.unwrap_or(u64::MAX));
        Ok(())
    }

    pub(super) fn do_grow_gc<T>(
        &mut self,
        store: &mut Store<T>,
        reserved_target: usize,
        bytes_needed: u64,
    ) -> Result<()> {
        self.with_parked_host_operation(store, |store| {
            store.grow_gc_reservation(reserved_target, bytes_needed)
        })
    }

    /// Parks the active execution while calling embedder-controlled limiter code. A limiter panic
    /// follows the same restore-and-rethrow path as a host function, including nested reentry.
    fn with_parked_host_operation<T, R>(
        &mut self,
        store: &mut Store<T>,
        operation: impl FnOnce(&mut Store<T>) -> Result<R>,
    ) -> Result<R> {
        let roots_mark = store.inner.gc_roots_mark();
        let pending = store.inner.pending_exception();
        let pending_generation = store.inner.pending_exception_generation();
        if let Some(exception) = pending {
            store.inner.push_gc_root(exception.raw(), RefKind::Exn);
        }
        store.inner.swap_exec(self);
        let outcome = super::guard::catch_host(|| operation(store));
        match outcome {
            Ok(result) => {
                #[cfg(feature = "async")]
                store.inner.recover_cancelled_async_calls();
                store.inner.swap_exec(self);
                store.inner.gc_roots_truncate(roots_mark);
                result
            }
            Err(payload) => {
                super::guard::restore_after_panic(
                    &mut store.inner,
                    roots_mark,
                    pending,
                    pending_generation,
                );
                super::guard::reraise(payload);
            }
        }
    }
}
