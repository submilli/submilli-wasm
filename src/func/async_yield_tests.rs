//! Fuel & epoch async-yield tests (`--features async`, #25d): long-running guests
//! yield to the executor instead of trapping, then resume. Uses a no-op-waker `drive`
//! helper that counts `Pending`s to prove a yield actually happened.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::task::{Context, Poll};

use pollster::block_on;

use crate::config::Config;
use crate::engine::Engine;
use crate::instance::Instance;
use crate::module::Module;
use crate::store::{Store, UpdateDeadline};
use crate::trap::Trap;
use crate::value::Val;

/// Polls `fut` to completion with a no-op waker, counting how many times it returned
/// `Pending` — i.e. how many times execution yielded to the executor.
fn drive<F: Future>(fut: F) -> (F::Output, u32) {
    let mut fut = Box::pin(fut);
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut pendings = 0;
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return (v, pendings),
            Poll::Pending => pendings += 1,
        }
    }
}

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

/// Counts down from the parameter, then returns 42 — enough ops to cross yield boundaries.
const LOOP_MODULE: &str = "(module (func (export \"run\") (param i32) (result i32)
    (block $b (loop $l
        local.get 0 i32.eqz br_if $b
        local.get 0 i32.const 1 i32.sub local.set 0
        br $l))
    i32.const 42))";

fn epoch_async_engine() -> Engine {
    let mut config = Config::new();
    config.epoch_interruption(true);
    config.async_support(true);
    Engine::new(&config).unwrap()
}

fn fuel_async_engine() -> Engine {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.async_support(true);
    Engine::new(&config).unwrap()
}

#[test]
fn epoch_async_yield_resumes() {
    let engine = epoch_async_engine();
    let m = module(&engine, LOOP_MODULE);
    let mut store = Store::new(&engine, ());
    store.epoch_deadline_async_yield_and_update(1_000_000); // yield, then push deadline far out
    store.set_epoch_deadline(0); // already at the deadline → yield on the first op
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    let (res, pendings) = drive(run.call_async(&mut store, &[Val::I32(50)], &mut out));
    res.unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
    assert!(pendings >= 1, "expected at least one yield to the executor");
}

#[test]
fn epoch_yield_traps_in_sync_context() {
    let mut config = Config::new();
    config.epoch_interruption(true);
    let engine = Engine::new(&config).unwrap(); // sync store
    let m = module(&engine, LOOP_MODULE);
    let mut store = Store::new(&engine, ());
    store.epoch_deadline_callback(|_| Ok(UpdateDeadline::Yield(1)));
    store.set_epoch_deadline(0);
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let err = run
        .call(&mut store, &[Val::I32(50)], &mut [Val::I32(0)])
        .unwrap_err();
    assert_eq!(*err.downcast_ref::<Trap>().unwrap(), Trap::Interrupt);
}

#[test]
fn fuel_async_yield_resumes() {
    let engine = fuel_async_engine();
    let m = module(&engine, LOOP_MODULE);
    let mut store = Store::new(&engine, ());
    store.set_fuel(1_000_000).unwrap();
    store.fuel_async_yield_interval(Some(10)).unwrap();
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    let (res, pendings) = drive(run.call_async(&mut store, &[Val::I32(100)], &mut out));
    res.unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
    assert!(pendings >= 1, "expected periodic fuel yields");
}

#[test]
fn fuel_yield_still_traps_when_total_exhausted() {
    let engine = fuel_async_engine();
    let m = module(&engine, LOOP_MODULE);
    let mut store = Store::new(&engine, ());
    store.set_fuel(20).unwrap(); // far less than the loop needs
    store.fuel_async_yield_interval(Some(10)).unwrap();
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let (res, pendings) =
        drive(run.call_async(&mut store, &[Val::I32(10_000)], &mut [Val::I32(0)]));
    let err = res.unwrap_err();
    assert_eq!(*err.downcast_ref::<Trap>().unwrap(), Trap::OutOfFuel);
    assert!(
        pendings >= 1,
        "should yield once before exhausting the reserve"
    );
}

#[test]
fn fuel_async_yield_interval_guards() {
    // Zero interval is rejected.
    let engine = fuel_async_engine();
    let mut store = Store::new(&engine, ());
    assert!(store.fuel_async_yield_interval(Some(0)).is_err());

    // Without `consume_fuel`.
    let mut cfg = Config::new();
    cfg.async_support(true);
    let no_fuel = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&no_fuel, ());
    assert!(store.fuel_async_yield_interval(Some(10)).is_err());

    // On a sync store (fuel but no async).
    let mut cfg = Config::new();
    cfg.consume_fuel(true);
    let sync = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&sync, ());
    assert!(store.fuel_async_yield_interval(Some(10)).is_err());
}
