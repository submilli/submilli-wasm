//! #42: memory64 + table64 — `i64`-indexed memories and tables. Addresses, `memory.size`/`grow`
//! operands+results, and table indices are `i64`; bounds checks still trap out-of-range.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Engine, Instance, Module, Store, Val};

fn instantiate(wat: &str) -> (Store<()>, Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    (store, inst)
}

fn call(store: &mut Store<()>, inst: Instance, name: &str, args: &[Val]) -> Result<Vec<Val>, ()> {
    let f = inst.get_func(&mut *store, name).unwrap();
    let n = f.ty(&*store).results().len();
    let mut out = vec![Val::I32(0); n];
    match f.call(&mut *store, args, &mut out) {
        Ok(()) => Ok(out),
        Err(_) => Err(()),
    }
}

/// A 64-bit memory round-trips a store/load addressed by an `i64`, and `memory.size`/`memory.grow`
/// take/return `i64` (grow past the declared max yields `-1` as an `i64`).
#[test]
fn memory64_addressing_size_and_grow() {
    let (mut store, inst) = instantiate(
        r#"(module
            (memory i64 1 2)
            (func (export "roundtrip") (param i64 i32) (result i32)
                (i32.store (local.get 0) (local.get 1))
                (i32.load (local.get 0)))
            (func (export "size") (result i64) (memory.size))
            (func (export "grow") (param i64) (result i64) (memory.grow (local.get 0))))"#,
    );

    let r = call(
        &mut store,
        inst,
        "roundtrip",
        &[Val::I64(40_000), Val::I32(0xdead_beefu32 as i32)],
    );
    assert_eq!(r.unwrap()[0].unwrap_i32(), 0xdead_beefu32 as i32);

    assert_eq!(
        call(&mut store, inst, "size", &[]).unwrap()[0].unwrap_i64(),
        1
    );
    // grow by 1 → old size 1 (as i64), new size 2.
    assert_eq!(
        call(&mut store, inst, "grow", &[Val::I64(1)]).unwrap()[0].unwrap_i64(),
        1
    );
    // grow again would exceed the declared max (2) → -1 as i64.
    assert_eq!(
        call(&mut store, inst, "grow", &[Val::I64(1)]).unwrap()[0].unwrap_i64(),
        -1
    );
}

/// A load at a huge `i64` address traps (out of bounds) rather than wrapping or allocating.
#[test]
fn memory64_huge_address_traps() {
    let (mut store, inst) = instantiate(
        r#"(module
            (memory i64 1)
            (func (export "load") (param i64) (result i32) (i32.load (local.get 0))))"#,
    );
    assert!(call(&mut store, inst, "load", &[Val::I64(0xffff_ffff_ffff_i64)]).is_err());
}

/// A 64-bit table: `table.size`/`table.grow` use `i64`, and `table.get`/`table.set` are addressed
/// by an `i64` index.
#[test]
fn table64_size_grow_and_access() {
    let (mut store, inst) = instantiate(
        r#"(module
            (table i64 1 3 externref)
            (func (export "size") (result i64) (table.size))
            (func (export "grow") (param i64) (result i64)
                (table.grow (ref.null extern) (local.get 0)))
            (func (export "is_null") (param i64) (result i32)
                (ref.is_null (table.get (local.get 0)))))"#,
    );

    assert_eq!(
        call(&mut store, inst, "size", &[]).unwrap()[0].unwrap_i64(),
        1
    );
    // grow by 1 → old size 1 (i64); a further grow past max (3) returns -1.
    assert_eq!(
        call(&mut store, inst, "grow", &[Val::I64(1)]).unwrap()[0].unwrap_i64(),
        1
    );
    assert_eq!(
        call(&mut store, inst, "grow", &[Val::I64(2)]).unwrap()[0].unwrap_i64(),
        -1
    );
    // The element at i64 index 0 is the null externref it was initialized with.
    assert_eq!(
        call(&mut store, inst, "is_null", &[Val::I64(0)]).unwrap()[0].unwrap_i32(),
        1
    );
}
