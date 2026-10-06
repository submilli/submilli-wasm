//! #33: host-function panic containment. A panicking host fn must not corrupt the store it ran in,
//! and must never poison the shared engine or other tenants. We catch the unwind at the boundary,
//! restore store state (parked execution, scoped GC roots, pending-exception slot), and **re-raise**
//! (wasmtime parity) — so the call site sees the panic, the store stays usable, and other stores on
//! the same engine are unaffected.
//!
//! These tests deliberately panic inside host fns; the panic messages on stderr are expected.

#![allow(clippy::unwrap_used)]

use std::panic::AssertUnwindSafe;

use submilli_wasm::{Engine, Func, Instance, Module, Store, Val};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

const CALLS_IMPORT: &str =
    "(module (import \"\" \"h\" (func $h)) (func (export \"run\") (call $h)))";
const BENIGN: &str = "(module (func (export \"id\") (result i32) (i32.const 7)))";

/// A benign module instantiates and runs on `store` — proves the store is not poisoned.
fn assert_store_usable(store: &mut Store<()>, engine: &Engine) {
    let benign = module(engine, BENIGN);
    let inst = Instance::new(&mut *store, &benign, &[]).unwrap();
    let id = inst.get_func(&mut *store, "id").unwrap();
    let mut out = [Val::I32(0)];
    id.call(&mut *store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
}

/// A guest that calls a panicking host import (the re-entrant boundary, with the execution parked):
/// the panic is contained + re-raised, and the store is left usable (parked execution restored).
#[test]
fn guest_call_to_panicking_host_leaves_store_usable() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let h = Func::wrap(&mut store, || -> () { panic!("boom from host") });
    let m = module(&engine, CALLS_IMPORT);
    let inst = Instance::new(&mut store, &m, &[h.into()]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| run.call(&mut store, &[], &mut [])));
    assert!(result.is_err(), "host panic should propagate (re-raised)");

    assert_store_usable(&mut store, &engine);
}

/// The embedder calling a panicking host `Func` directly (the top-level boundary).
#[test]
fn direct_call_to_panicking_host_leaves_store_usable() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let h = Func::wrap(&mut store, || -> () { panic!("boom direct") });

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| h.call(&mut store, &[], &mut [])));
    assert!(result.is_err());

    assert_store_usable(&mut store, &engine);
}

/// A callback may catch a panic from a directly-called host function and continue its guest caller.
/// Direct panic cleanup must leave the outer parked execution intact.
#[test]
fn nested_direct_host_panic_can_be_caught_without_losing_outer_execution() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let inner = Func::wrap(&mut store, || -> () { panic!("nested direct boom") });
    let outer = Func::new(
        &mut store,
        submilli_wasm::FuncType::new(&engine, [], []),
        move |mut caller, _params, _results| {
            let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
                inner.call(&mut caller, &[], &mut [])
            }));
            assert!(panic.is_err());
            Ok(())
        },
    );
    let m = module(&engine, CALLS_IMPORT);
    let inst = Instance::new(&mut store, &m, &[outer.into()]).unwrap();
    inst.get_func(&mut store, "run")
        .unwrap()
        .call(&mut store, &[], &mut [])
        .unwrap();
    assert_store_usable(&mut store, &engine);
}

/// Panic cleanup for a directly-called host function must preserve an internal failure already
/// latched by a nested call. Catching the panic cannot let the outer guest resume successfully.
#[test]
fn nested_direct_panic_preserves_latched_internal_failure() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let bad = Func::new(
        &mut store,
        submilli_wasm::FuncType::new(&engine, [], [submilli_wasm::ValType::I32]),
        |_caller, _params, results| {
            results[0] = Val::I64(1);
            Ok(())
        },
    );
    let panicker = Func::new(
        &mut store,
        submilli_wasm::FuncType::new(&engine, [], []),
        move |mut caller, _params, _results| {
            let _ = bad.call(&mut caller, &[], &mut [Val::I32(0)]);
            panic!("panic after latching an internal failure");
        },
    );
    let outer = Func::new(
        &mut store,
        submilli_wasm::FuncType::new(&engine, [], []),
        move |mut caller, _params, _results| {
            let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
                panicker.call(&mut caller, &[], &mut [])
            }));
            assert!(panic.is_err());
            Ok(())
        },
    );
    let m = module(&engine, CALLS_IMPORT);
    let inst = Instance::new(&mut store, &m, &[outer.into()]).unwrap();
    let error = inst
        .get_func(&mut store, "run")
        .unwrap()
        .call(&mut store, &[], &mut [])
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("internal result shape invariant violated"));
    assert_store_usable(&mut store, &engine);
}

/// A host-fn panic in one tenant must not poison the shared engine: another store on the **same
/// engine** still instantiates (touching the engine type registry) and runs.
#[test]
fn host_panic_does_not_poison_other_tenants() {
    let engine = Engine::default();

    let mut a = Store::new(&engine, ());
    let h = Func::wrap(&mut a, || -> () { panic!("tenant A boom") });
    let m = module(&engine, CALLS_IMPORT);
    let inst = Instance::new(&mut a, &m, &[h.into()]).unwrap();
    let run = inst.get_func(&mut a, "run").unwrap();
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| run.call(&mut a, &[], &mut [])));

    let mut b = Store::new(&engine, ());
    assert_store_usable(&mut b, &engine);
}

#[cfg(feature = "async")]
mod async_tests {
    use super::{assert_store_usable, module, CALLS_IMPORT};
    use std::panic::AssertUnwindSafe;
    use submilli_wasm::{Caller, Engine, Func, FuncType, Instance, Store, ValType};

    fn ft(engine: &Engine, params: &[ValType], results: &[ValType]) -> FuncType {
        FuncType::new(engine, params.iter().cloned(), results.iter().cloned())
    }

    /// An async host fn whose future panics when polled: contained across the `.await`, re-raised,
    /// store left usable.
    #[test]
    fn async_host_panic_leaves_store_usable() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let h = Func::new_async(
            &mut store,
            ft(&engine, &[], &[]),
            |_caller: Caller<'_, ()>, _params, _results| {
                Box::new(async move { panic!("async boom") })
            },
        );
        let m = module(&engine, CALLS_IMPORT);
        let inst = pollster::block_on(Instance::new_async(&mut store, &m, &[h.into()])).unwrap();
        let run = inst.get_func(&mut store, "run").unwrap();

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            pollster::block_on(run.call_async(&mut store, &[], &mut []))
        }));
        assert!(result.is_err());

        assert_store_usable(&mut store, &engine);
    }

    /// Panicking before the callback can construct its future is contained by the same boundary as
    /// a panic while polling the future.
    #[test]
    fn async_host_future_construction_panic_leaves_store_usable() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let h = Func::new_async(
            &mut store,
            ft(&engine, &[], &[]),
            |_caller: Caller<'_, ()>, _params, _results| panic!("async construction boom"),
        );
        let m = module(&engine, CALLS_IMPORT);
        let inst = pollster::block_on(Instance::new_async(&mut store, &m, &[h.into()])).unwrap();
        let run = inst.get_func(&mut store, "run").unwrap();

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            pollster::block_on(run.call_async(&mut store, &[], &mut []))
        }));
        assert!(result.is_err());
        assert_store_usable(&mut store, &engine);
    }
}
