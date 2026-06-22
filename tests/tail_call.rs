//! #39: tail calls — `return_call*` replace the current frame, so deep tail recursion runs in
//! bounded stack, and a `return_call` to a host fn returns to the caller.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Caller, Engine, Extern, Func, Instance, Module, Store, Val};

fn call1(store: &mut Store<()>, inst: Instance, name: &str, arg: i32) -> i32 {
    let f = inst.get_func(&mut *store, name).unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut *store, &[Val::I32(arg)], &mut out).unwrap();
    out[0].unwrap_i32()
}

/// A self-`return_call` 1,000,000 deep completes without `StackOverflow` — the frame-reuse proof
/// (a normal recursive call would trap).
#[test]
fn deep_self_tail_recursion_does_not_overflow() {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (func $count (export "count") (param i32) (result i32)
                    (if (result i32) (i32.eqz (local.get 0))
                        (then (i32.const 0))
                        (else (return_call $count (i32.sub (local.get 0) (i32.const 1)))))))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    assert_eq!(call1(&mut store, inst, "count", 1_000_000), 0);
}

/// Mutual recursion via `return_call` (even/odd).
#[test]
fn mutual_tail_recursion() {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (func $even (export "even") (param i32) (result i32)
                    (if (result i32) (i32.eqz (local.get 0)) (then (i32.const 1))
                        (else (return_call $odd (i32.sub (local.get 0) (i32.const 1))))))
                (func $odd (param i32) (result i32)
                    (if (result i32) (i32.eqz (local.get 0)) (then (i32.const 0))
                        (else (return_call $even (i32.sub (local.get 0) (i32.const 1)))))))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    assert_eq!(call1(&mut store, inst, "even", 100_000), 1);
    assert_eq!(call1(&mut store, inst, "even", 99_999), 0);
}

/// `return_call` to an imported host fn from the outermost frame — the host's result surfaces as
/// the exported function's result.
#[test]
fn tail_call_to_host() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let double = Func::wrap(&mut store, |_: Caller<'_, ()>, x: i32| x * 2);
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "double" (func $double (param i32) (result i32)))
                (func (export "tail_double") (param i32) (result i32)
                    (return_call $double (local.get 0))))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(double)]).unwrap();
    assert_eq!(call1(&mut store, inst, "tail_double", 21), 42);
}
