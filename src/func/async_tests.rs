//! Async entry-point tests (`--features async`): `call_async`, `new_async`,
//! `Linker::instantiate_async`, and the sync/async store guards. Driven by
//! `pollster::block_on` — the futures here complete without ever pending (real
//! suspension arrives with async host fns / yields in later phases).
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pollster::block_on;

use crate::config::Config;
use crate::engine::Engine;
use crate::extern_::Extern;
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::linker::Linker;
use crate::module::Module;
use crate::store::Store;
use crate::value::{FuncType, Val, ValType};

/// A future that returns `Pending` once before completing — forces the async driver to
/// genuinely park and resume (rather than completing synchronously on first poll).
struct YieldOnce(bool);

impl Future for YieldOnce {
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

fn ft(engine: &Engine, params: &[ValType], results: &[ValType]) -> FuncType {
    FuncType::new(engine, params.iter().cloned(), results.iter().cloned())
}

fn async_engine() -> Engine {
    let mut config = Config::new();
    config.async_support(true);
    Engine::new(&config).unwrap()
}

/// An engine with async support explicitly disabled (it is on by default now).
fn sync_engine() -> Engine {
    let mut config = Config::new();
    config.async_support(false);
    Engine::new(&config).unwrap()
}

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

#[test]
fn call_async_runs_wasm_export() {
    let engine = async_engine();
    let m = module(
        &engine,
        "(module (func (export \"add\") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add))",
    );
    let mut store = Store::new(&engine, ());
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let add = inst.get_func(&mut store, "add").unwrap();
    let mut out = [Val::I32(0)];
    block_on(add.call_async(&mut store, &[Val::I32(40), Val::I32(2)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

#[test]
fn typed_call_async_agrees() {
    let engine = async_engine();
    let m = module(
        &engine,
        "(module (func (export \"mul\") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.mul))",
    );
    let mut store = Store::new(&engine, ());
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let mul = inst
        .get_typed_func::<(i32, i32), i32>(&mut store, "mul")
        .unwrap();
    let got = block_on(mul.call_async(&mut store, (6, 7))).unwrap();
    assert_eq!(got, 42);
}

#[test]
fn new_async_runs_start_function() {
    let engine = async_engine();
    let m = module(
        &engine,
        "(module
            (memory (export \"mem\") 1)
            (func $start (i32.store (i32.const 0) (i32.const 42)))
            (start $start))",
    );
    let mut store = Store::new(&engine, ());
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let mem = inst.get_memory(&mut store, "mem").unwrap();
    assert_eq!(mem.data(&store)[0], 42);
}

#[test]
fn instantiate_async_links_modules() {
    let engine = async_engine();
    let mut store = Store::new(&engine, ());
    let provider = module(
        &engine,
        "(module (func (export \"forty_two\") (result i32) i32.const 42))",
    );
    let pinst = block_on(Instance::new_async(&mut store, &provider, &[])).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.instance(&mut store, "lib", pinst).unwrap();
    let consumer = module(
        &engine,
        "(module
            (import \"lib\" \"forty_two\" (func $f (result i32)))
            (func (export \"run\") (result i32) call $f))",
    );
    let cinst = block_on(linker.instantiate_async(&mut store, &consumer)).unwrap();
    let run = cinst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    block_on(run.call_async(&mut store, &[], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

#[test]
fn sync_call_works_on_async_store() {
    // Fiber-less: a plain sync `Func::call` is allowed on an async-enabled store.
    let engine = async_engine();
    assert!(engine.is_async());
    let mut store = Store::new(&engine, ());
    let f = Func::wrap(&mut store, || 7i32);
    let mut out = [Val::I32(0)];
    f.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
}

#[test]
fn sync_instance_new_works_on_async_store() {
    let engine = async_engine();
    let m = module(&engine, "(module)");
    let mut store = Store::new(&engine, ());
    Instance::new(&mut store, &m, &[]).unwrap();
}

#[test]
fn call_async_rejected_on_sync_store() {
    let engine = sync_engine(); // async explicitly disabled
    let mut store = Store::new(&engine, ());
    let f = Func::wrap(&mut store, || 7i32);
    let err = block_on(f.call_async(&mut store, &[], &mut [Val::I32(0)])).unwrap_err();
    assert!(err.to_string().contains("async_support"));
}

#[test]
fn host_import_called_from_async_wasm() {
    let engine = async_engine();
    let mut store = Store::new(&engine, ());
    let dbl = Func::wrap(&mut store, |x: i32| x * 2);
    let m = module(
        &engine,
        "(module
            (import \"h\" \"f\" (func (param i32) (result i32)))
            (func (export \"run\") (param i32) (result i32) local.get 0 call 0))",
    );
    let inst = block_on(Instance::new_async(&mut store, &m, &[Extern::Func(dbl)])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    block_on(run.call_async(&mut store, &[Val::I32(21)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

// --- async host functions ---

const CALLS_HOST: &str = "(module
    (import \"h\" \"f\" (func (param i32) (result i32)))
    (func (export \"run\") (param i32) (result i32) local.get 0 call 0))";

#[test]
fn new_async_host_fn_awaited_from_wasm() {
    let engine = async_engine();
    let mut store = Store::new(&engine, ());
    // The host future yields once (suspends the whole call) before producing x + 1.
    let inc = Func::new_async(
        &mut store,
        ft(&engine, &[ValType::I32], &[ValType::I32]),
        |_caller: Caller<'_, ()>, params, results| {
            let x = params[0].unwrap_i32();
            Box::new(async move {
                YieldOnce(false).await;
                results[0] = Val::I32(x + 1);
                Ok(())
            })
        },
    );
    let m = module(&engine, CALLS_HOST);
    let inst = block_on(Instance::new_async(&mut store, &m, &[Extern::Func(inc)])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    block_on(run.call_async(&mut store, &[Val::I32(41)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

#[test]
fn wrap_async_typed_host_fn_from_wasm() {
    let engine = async_engine();
    let mut store = Store::new(&engine, ());
    let dbl = Func::wrap_async(&mut store, |_caller: Caller<'_, ()>, (x,): (i32,)| {
        Box::new(async move {
            YieldOnce(false).await;
            x * 2
        })
    });
    let m = module(&engine, CALLS_HOST);
    let inst = block_on(Instance::new_async(&mut store, &m, &[Extern::Func(dbl)])).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    block_on(run.call_async(&mut store, &[Val::I32(21)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

#[test]
fn linker_async_host_fns() {
    let engine = async_engine();
    let mut store = Store::new(&engine, ());
    let mut linker: Linker<()> = Linker::new(&engine);
    linker
        .func_new_async(
            "h",
            "f",
            ft(&engine, &[ValType::I32], &[ValType::I32]),
            |_caller: Caller<'_, ()>, params, results| {
                let x = params[0].unwrap_i32();
                Box::new(async move {
                    YieldOnce(false).await;
                    results[0] = Val::I32(x + 1);
                    Ok(())
                })
            },
        )
        .unwrap();
    let m = module(&engine, CALLS_HOST);
    let inst = block_on(linker.instantiate_async(&mut store, &m)).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    block_on(run.call_async(&mut store, &[Val::I32(9)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 10);
}

#[test]
fn async_host_fn_called_directly() {
    let engine = async_engine();
    let mut store = Store::new(&engine, ());
    let inc = Func::wrap_async(&mut store, |_caller: Caller<'_, ()>, (x,): (i32,)| {
        Box::new(async move { x + 1 })
    });
    let mut out = [Val::I32(0)];
    block_on(inc.call_async(&mut store, &[Val::I32(7)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 8);
}

#[test]
fn async_host_fn_rejected_in_sync_context() {
    // Async host fn imported into wasm, then driven by the *sync* interpreter → error.
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

#[test]
fn sync_call_of_async_host_fn_errors() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let f = Func::wrap_async(&mut store, |_caller: Caller<'_, ()>, (x,): (i32,)| {
        Box::new(async move { x + 1 })
    });
    let err = f
        .call(&mut store, &[Val::I32(1)], &mut [Val::I32(0)])
        .unwrap_err();
    assert!(err.to_string().contains("asynchronously") || err.to_string().contains("async"));
}
