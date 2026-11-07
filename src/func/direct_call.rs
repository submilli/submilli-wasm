//! Lifecycle for directly-invoked sync and async host functions.

use super::{default_results, validate_results, Caller};
use crate::store::{AsContextMut, FuelStep, Store, StoreInner, UpdateDeadline};
use crate::trap::Trap;
use crate::value::{ExnRef, FuncType, Rooted, Val, ValType};
use crate::Result;

pub(super) fn call_direct_host<S: AsContextMut>(
    store: &mut S,
    host_index: u32,
    params: &[Val],
    ty: &FuncType,
) -> Result<Vec<Val>> {
    preflight_direct_call(store.as_context_mut().inner_mut())?;
    let cb = store.as_context_mut().store_mut().host_funcs[host_index as usize].clone();
    let mut out = default_results(ty);
    let boundary = DirectCallBoundary::capture(store.as_context().inner());
    let outcome = match crate::exec::guard::catch_host(|| {
        cb(Caller::new(store.as_context_mut(), None), params, &mut out)
    }) {
        Ok(result) => result,
        Err(payload) => {
            boundary.restore_after_panic(store.as_context_mut().inner_mut());
            crate::exec::guard::reraise(payload);
        }
    };
    #[cfg(feature = "async")]
    store
        .as_context_mut()
        .inner_mut()
        .recover_cancelled_async_calls();
    let result_types: Vec<_> = ty.results().collect();
    finish_direct_host_call(
        store.as_context_mut().inner_mut(),
        boundary,
        out,
        &result_types,
        outcome,
    )
}

pub(super) fn preflight_direct_call(inner: &mut StoreInner) -> Result<()> {
    let error = if inner.has_parked_execution() {
        inner.internal_error()
    } else {
        inner.take_internal_error()
    };
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The gates of a host fn called from inside another call. A guest that re-exports a host import
/// can turn a host fn calling that export into pure host→host recursion, which runs no wasm op —
/// so nothing else would bound its stack or its time. It is charged like a wasm step: its native
/// stack (the bytes used beneath it plus the parked execution's heap stacks) against
/// `max_wasm_stack`, one unit of fuel, and the epoch deadline. A top-level call from the embedder
/// stays free, as in wasmtime. Sync: neither a fuel yield point nor an epoch `Yield` can suspend
/// here, so the first refuels from the reserve and the second traps, as in the sync driver.
pub(super) fn nested_host_gates<T>(store: &mut Store<T>) -> Result<()> {
    if !nested_host_stack_ok(store)? {
        return Ok(());
    }
    if store.inner.engine().consume_fuel() {
        match store.inner.consume_fuel_step() {
            FuelStep::Ran => {}
            FuelStep::NeedYield => refuel_and_charge(&mut store.inner)?,
            FuelStep::Exhausted => return Err(Trap::OutOfFuel.into()),
        }
    }
    if epoch_due(store) {
        match crate::exec::epoch::nested_host_epoch_action(store)? {
            UpdateDeadline::Continue(delta) => {
                crate::exec::epoch::extend_epoch_deadline(store, delta);
            }
            _ => return Err(Trap::Interrupt.into()),
        }
    }
    Ok(())
}

/// [`nested_host_gates`] for an async direct call, which can suspend: a fuel yield point and an
/// epoch `Yield` give the executor a turn first, as the async driver does.
#[cfg(feature = "async")]
pub(super) async fn nested_host_gates_async<T>(store: &mut Store<T>) -> Result<()> {
    use crate::exec::epoch::{extend_epoch_deadline, nested_host_epoch_action, yield_now};
    if !nested_host_stack_ok(store)? {
        return Ok(());
    }
    if store.inner.engine().consume_fuel() {
        match store.inner.consume_fuel_step() {
            FuelStep::Ran => {}
            FuelStep::NeedYield => {
                yield_now().await;
                refuel_and_charge(&mut store.inner)?;
            }
            FuelStep::Exhausted => return Err(Trap::OutOfFuel.into()),
        }
    }
    if epoch_due(store) {
        match nested_host_epoch_action(store)? {
            UpdateDeadline::Continue(delta) => extend_epoch_deadline(store, delta),
            UpdateDeadline::Yield(delta) => {
                yield_now().await;
                extend_epoch_deadline(store, delta);
            }
            UpdateDeadline::Interrupt => return Err(Trap::Interrupt.into()),
        }
    }
    Ok(())
}

/// Whether this direct host call is nested (otherwise it has no gates); if so, checks its native
/// stack.
fn nested_host_stack_ok<T>(store: &mut Store<T>) -> Result<bool> {
    let native = crate::exec::native::nested_bytes();
    if native == 0 {
        return Ok(false);
    }
    let inner = &mut store.inner;
    let used = native.saturating_add(inner.parked_heap_stack_bytes());
    if used >= inner.engine().max_wasm_stack() {
        return Err(Trap::StackOverflow.into());
    }
    Ok(true)
}

/// A reached epoch deadline — not while the epoch callback itself runs, which is servicing it.
fn epoch_due<T>(store: &Store<T>) -> bool {
    let inner = &store.inner;
    inner.engine().epoch_interruption() && inner.epoch_deadline_reached() && !store.epoch_servicing
}

/// At a fuel yield point: moves the next interval in from the reserve and charges the unit there.
fn refuel_and_charge(inner: &mut StoreInner) -> Result<()> {
    inner.refuel_from_reserve();
    match inner.consume_fuel_step() {
        FuelStep::Ran => Ok(()),
        FuelStep::NeedYield | FuelStep::Exhausted => Err(Trap::OutOfFuel.into()),
    }
}

/// State owned by a direct host callback and restored on every abnormal exit.
#[derive(Clone, Copy)]
pub(super) struct DirectCallBoundary {
    pub(super) roots_mark: usize,
    pub(super) pending: Option<Rooted<ExnRef>>,
    pub(super) pending_generation: u64,
}

impl DirectCallBoundary {
    pub(super) fn capture(inner: &StoreInner) -> Self {
        let roots_mark = inner.gc_roots_mark();
        let pending = inner.pending_exception();
        if let Some(exception) = pending {
            inner.push_gc_root(exception.raw(), crate::canon::RefKind::Exn);
        }
        Self {
            roots_mark,
            pending,
            pending_generation: inner.pending_exception_generation(),
        }
    }

    pub(super) fn restore_after_panic(self, inner: &mut StoreInner) {
        crate::exec::guard::restore_direct_after_panic(
            inner,
            self.roots_mark,
            self.pending,
            self.pending_generation,
        );
    }
}

pub(super) fn finish_direct_host_call(
    inner: &mut StoreInner,
    boundary: DirectCallBoundary,
    out: Vec<Val>,
    result_types: &[ValType],
    outcome: Result<()>,
) -> Result<Vec<Val>> {
    let DirectCallBoundary {
        roots_mark,
        pending,
        pending_generation,
    } = boundary;
    let internal_error = if inner.has_parked_execution() {
        inner.internal_error()
    } else {
        inner.take_internal_error()
    };
    if let Some(error) = internal_error {
        inner.gc_roots_truncate(roots_mark);
        inner.restore_pending_exception(pending, pending_generation);
        return Err(error);
    }
    if let Err(error) = outcome {
        let fresh_throw = error.is::<crate::exception::ThrownException>()
            && inner.pending_exception_generation() != pending_generation;
        inner.gc_roots_truncate(roots_mark);
        if fresh_throw && inner.pending_exception().is_none() {
            inner.restore_pending_exception(pending, pending_generation);
            return missing_pending_exception(inner);
        }
        return Err(error);
    }
    if let Err(error) = validate_results(inner, &out, result_types) {
        inner.gc_roots_truncate(roots_mark);
        inner.restore_pending_exception(pending, pending_generation);
        return Err(error);
    }
    hand_out_results(inner, roots_mark, out, pending, pending_generation)
}

fn missing_pending_exception(inner: &mut StoreInner) -> Result<Vec<Val>> {
    let error =
        crate::error::InternalError::ResultShape("host throw marker had no pending exception");
    if inner.has_parked_execution() {
        inner.latch_internal_error(error);
    }
    Err(error.into())
}

fn hand_out_results(
    inner: &mut StoreInner,
    roots_mark: usize,
    out: Vec<Val>,
    pending: Option<Rooted<ExnRef>>,
    pending_generation: u64,
) -> Result<Vec<Val>> {
    inner.gc_roots_truncate(roots_mark);
    let handed_out = out
        .into_iter()
        .map(|value| inner.root_host_result(value))
        .collect::<Result<Vec<_>>>();
    if let Err(error) = &handed_out {
        inner.gc_roots_truncate(roots_mark);
        inner.restore_pending_exception(pending, pending_generation);
        if inner.has_parked_execution() {
            if let Some(internal) = error.downcast_ref::<crate::error::InternalError>() {
                inner.latch_internal_error(*internal);
            }
        }
    }
    handed_out
}
