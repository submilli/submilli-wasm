//! Phase-3 gate: async/concurrency integration tests driving the public async API
//! exactly as a wasmtime embedder would. Proves the `docs/PLAN.md` Phase-3 acceptance
//! criteria — async-host I/O completion, fuel/epoch yield-and-resume, and concurrent
//! stores on one shared `Engine` making independent progress.
//!
//! Compiled only under `--features async` (empty otherwise, so default `cargo test`
//! is unaffected).
#![cfg(feature = "async")]
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pollster::block_on;

use submilli_wasm::{
    Caller, Config, Engine, Extern, Func, FuncType, Instance, Module, Store, Val, ValType,
};

// --- helpers --------------------------------------------------------------

fn engine_with(consume_fuel: bool, epoch: bool) -> Engine {
    let mut config = Config::new();
    config.async_support(true);
    config.consume_fuel(consume_fuel);
    config.epoch_interruption(epoch);
    Engine::new(&config).unwrap()
}

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

fn ft(engine: &Engine, params: &[ValType], results: &[ValType]) -> FuncType {
    FuncType::new(engine, params.iter().cloned(), results.iter().cloned())
}

/// A one-shot future that is `Pending` once (simulating not-yet-ready I/O), then `Ready`.
async fn io_ready() {
    struct IoReady(bool);
    impl Future for IoReady {
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
    IoReady(false).await;
}

/// Round-robin-polls several futures to completion with a no-op waker, returning their
/// outputs (in input order) and the number of polling rounds. `rounds > futs.len()`
/// means the futures yielded and made *interleaved* progress rather than running serially.
fn drive_all<O>(futs: Vec<Pin<Box<dyn Future<Output = O>>>>) -> (Vec<O>, usize) {
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut futs = futs;
    let mut done: Vec<Option<O>> = futs.iter().map(|_| None).collect();
    let mut remaining = futs.len();
    let mut rounds = 0usize;
    while remaining > 0 {
        rounds += 1;
        for (i, f) in futs.iter_mut().enumerate() {
            if done[i].is_some() {
                continue;
            }
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                done[i] = Some(v);
                remaining -= 1;
            }
        }
    }
    (done.into_iter().map(Option::unwrap).collect(), rounds)
}

/// Loops `n` times, then returns the original `n` — enough ops to cross yield boundaries
/// and a distinct result per input.
const ECHO_LOOP: &str = "(module (func (export \"run\") (param i32) (result i32)
    (local $i i32)
    (local.set $i (local.get 0))
    (block $b (loop $l
        local.get $i i32.eqz br_if $b
        local.get $i i32.const 1 i32.sub local.set $i
        br $l))
    local.get 0))";

/// Imports an async host fn `h.f : i32 -> i32` and forwards its parameter to it.
const CALLS_HOST: &str = "(module
    (import \"h\" \"f\" (func (param i32) (result i32)))
    (func (export \"run\") (param i32) (result i32) local.get 0 call 0))";

// --- Done-when #1: async host fn awaiting I/O runs to completion ----------

#[test]
fn async_host_fn_awaiting_io_completes() {
    let engine = engine_with(false, false);
    let mut store = Store::new(&engine, ());
    // `dbl` awaits simulated async I/O before producing its result.
    let dbl = Func::new_async(
        &mut store,
        ft(&engine, &[ValType::I32], &[ValType::I32]),
        |_caller: Caller<'_, ()>, params, results| {
            let x = params[0].unwrap_i32();
            Box::new(async move {
                io_ready().await;
                results[0] = Val::I32(x * 2);
                Ok(())
            })
        },
    );
    let m = module(&engine, CALLS_HOST);
    let inst = block_on(Instance::new_async(&mut store, &m, &[Extern::Func(dbl)])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    block_on(run.call_async(&mut store, &[Val::I32(21)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

// --- Done-when #2a: long-running wasm yields on fuel/epoch and resumes ----

#[test]
fn fuel_yield_runs_to_completion() {
    let engine = engine_with(true, false);
    let m = module(&engine, ECHO_LOOP);
    let mut store = Store::new(&engine, ());
    store.set_fuel(100_000_000).unwrap();
    store.fuel_async_yield_interval(Some(64)).unwrap();
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    block_on(run.call_async(&mut store, &[Val::I32(5_000)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 5_000);
}

#[test]
fn epoch_yield_runs_to_completion() {
    let engine = engine_with(false, true);
    let m = module(&engine, ECHO_LOOP);
    // Self-contained task so the future is `'static` (owns its store + result buffer).
    let task: Pin<Box<dyn Future<Output = i32>>> = Box::pin(async move {
        let mut store = Store::new(&engine, ());
        store.epoch_deadline_async_yield_and_update(1_000_000);
        store.set_epoch_deadline(0); // at the deadline → yield on the first op
        let inst = Instance::new_async(&mut store, &m, &[]).await.unwrap();
        let run = inst.get_func(&mut store, "run").unwrap();
        let mut out = [Val::I32(0)];
        run.call_async(&mut store, &[Val::I32(1_000)], &mut out)
            .await
            .unwrap();
        out[0].unwrap_i32()
    });
    let (results, rounds) = drive_all(vec![task]);
    assert_eq!(results[0], 1_000);
    assert!(
        rounds >= 2,
        "epoch yield should have suspended at least once"
    );
}

// --- Done-when #2b: concurrent stores make independent progress -----------

/// A self-contained task: its own `Store` on `engine`, a fuel-yielding `run(n)` returning `n`.
fn fuel_task(engine: Engine, m: Module, n: i32) -> Pin<Box<dyn Future<Output = i32>>> {
    Box::pin(async move {
        let mut store = Store::new(&engine, ());
        store.set_fuel(100_000_000).unwrap();
        store.fuel_async_yield_interval(Some(64)).unwrap();
        let inst = Instance::new_async(&mut store, &m, &[]).await.unwrap();
        let run = inst.get_func(&mut store, "run").unwrap();
        let mut out = [Val::I32(0)];
        run.call_async(&mut store, &[Val::I32(n)], &mut out)
            .await
            .unwrap();
        out[0].unwrap_i32()
    })
}

#[test]
fn concurrent_stores_cooperative_progress() {
    let engine = engine_with(true, false);
    let m = module(&engine, ECHO_LOOP);
    let inputs = [3_000, 5_000, 7_000, 9_000];
    let tasks: Vec<Pin<Box<dyn Future<Output = i32>>>> = inputs
        .iter()
        .map(|&n| fuel_task(engine.clone(), m.clone(), n))
        .collect();

    let (results, rounds) = drive_all(tasks);

    assert_eq!(results, inputs); // each store produced its own correct result
    assert!(
        rounds > inputs.len(),
        "stores should interleave via yields, not run serially (rounds={rounds})"
    );
}

#[test]
fn concurrent_stores_across_threads() {
    let engine = engine_with(true, false);
    let m = module(&engine, ECHO_LOOP);
    let handles: Vec<_> = (1..=4i32)
        .map(|k| {
            let engine = engine.clone();
            let m = m.clone();
            std::thread::spawn(move || block_on(fuel_task(engine, m, k * 2_500)))
        })
        .collect();
    let results: Vec<i32> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results, vec![2_500, 5_000, 7_500, 10_000]);
}

// --- sync entry rejects async host fns ------------------------------------

#[test]
fn sync_call_rejects_async_host_fn() {
    let engine = Engine::default(); // sync store
    let mut store = Store::new(&engine, ());
    let f = Func::new_async(
        &mut store,
        ft(&engine, &[ValType::I32], &[ValType::I32]),
        |_caller: Caller<'_, ()>, params, results| {
            let x = params[0].unwrap_i32();
            Box::new(async move {
                results[0] = Val::I32(x);
                Ok(())
            })
        },
    );
    let m = module(&engine, CALLS_HOST);
    let inst = Instance::new(&mut store, &m, &[Extern::Func(f)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let err = run
        .call(&mut store, &[Val::I32(1)], &mut [Val::I32(0)])
        .unwrap_err();
    assert!(err.to_string().contains("synchronous context"));
}
