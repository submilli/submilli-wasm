//! #34: store isolation. (1) A handle (Func/Memory/Global/Table/Tag/Instance) minted by store A used
//! with store B is rejected (panics — wasmtime parity, embedder bug, never UB / wrong entity).
//! (2) CVE-2024-12053 regression: cross-module type matching uses *canonical* type ids, not
//! module-relative indices, so a struct at a different relative index still links and a decoy at the
//! same index does not.

#![allow(clippy::unwrap_used)]

use std::panic::{catch_unwind, AssertUnwindSafe};

use submilli_wasm::{Engine, Extern, Instance, Module, Store};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

const EXPORTS: &str = "(module
    (memory (export \"mem\") 1)
    (global (export \"g\") (mut i32) (i32.const 0))
    (table (export \"t\") 1 funcref)
    (func (export \"f\"))
    (tag (export \"e\")))";

/// Each handle obtained from store A panics when used with store B; same-store use works.
#[test]
fn cross_store_handle_use_is_rejected() {
    let engine = Engine::default();
    let m = module(&engine, EXPORTS);

    let mut a = Store::new(&engine, ());
    let ia = Instance::new(&mut a, &m, &[]).unwrap();
    let mem = ia.get_memory(&mut a, "mem").unwrap();
    let glob = ia.get_global(&mut a, "g").unwrap();
    let tab = ia.get_table(&mut a, "t").unwrap();
    let func = ia.get_func(&mut a, "f").unwrap();
    let tag = match ia.get_export(&mut a, "e") {
        Some(Extern::Tag(t)) => t,
        other => panic!("expected a tag export, got {other:?}"),
    };

    // Same-store use works.
    assert_eq!(mem.size(&a), 1);

    // A fresh store B; using A's handles with it must panic (never silently touch B's entities).
    let mut b = Store::new(&engine, ());
    assert!(catch_unwind(AssertUnwindSafe(|| mem.size(&b))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| glob.get(&mut b))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| tab.size(&b))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| func.call(&mut b, &[], &mut []))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| tag.ty(&b))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| ia.get_func(&mut b, "f"))).is_err());
}

/// CVE-2024-12053: cross-module GC type identity is by canonical id, not relative index. Module A
/// defines `$s = struct {i32}` at relative index 0 and exports a func taking `(ref null $s)`. Module
/// B declares a *decoy* `struct {i64}` at index 0 and the matching `struct {i32}` at index **1**,
/// importing A's func with the param typed as its index-1 `$s` — links iff canonical ids (not the
/// differing relative indices) are compared. Module C imports it as the index-0 decoy — must fail.
#[test]
fn cross_module_type_matching_is_canonical() {
    let engine = Engine::default();
    let a = module(
        &engine,
        "(module (type $s (struct (field i32))) (func (export \"f\") (param (ref null $s))))",
    );
    let b = module(
        &engine,
        "(module
            (type $decoy (struct (field i64)))
            (type $s (struct (field i32)))
            (import \"a\" \"f\" (func (param (ref null 1)))))",
    );
    let c = module(
        &engine,
        "(module
            (type $decoy (struct (field i64)))
            (type $s (struct (field i32)))
            (import \"a\" \"f\" (func (param (ref null 0)))))",
    );

    let mut store = Store::new(&engine, ());
    let ia = Instance::new(&mut store, &a, &[]).unwrap();
    let f = ia.get_func(&mut store, "f").unwrap();

    // B's relative index 1 → same canonical struct as A's index 0 → links.
    assert!(
        Instance::new(&mut store, &b, &[Extern::Func(f)]).is_ok(),
        "matching struct at a different relative index must link (canonical id)"
    );
    // C's relative index 0 is the decoy struct {i64} → different canonical id → link fails.
    assert!(
        Instance::new(&mut store, &c, &[Extern::Func(f)]).is_err(),
        "a decoy struct at the imported index must NOT link"
    );
}
