//! #27h Phase-5 gate (interim): two separate guest modules exchange a GC ref across an
//! import/export boundary. Matching rec groups share a canonical type id, so the import links and
//! the cross-module ref casts/reads correctly; a mismatched rec group is a distinct canonical id,
//! so the import fails to link at instantiation.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Engine, Instance, Linker, Module, Store, Val};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

/// A provider exporting `make (result (ref $s))` for `(struct (field i32) (field i32))`.
const PROVIDER: &str = r#"
    (module
      (type $s (struct (field i32) (field i32)))
      (func (export "make") (result (ref $s))
        (struct.new $s (i32.const 11) (i32.const 22))))
"#;

#[test]
fn same_rec_group_exchange_and_cast_succeeds() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    let provider = module(&engine, PROVIDER);
    let pinst = Instance::new(&mut store, &provider, &[]).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.instance(&mut store, "lib", pinst).unwrap();

    // Consumer declares the *structurally identical* `$s`, imports `make` with that signature, and
    // reads field 1 of the cross-module struct it returns.
    let consumer = module(
        &engine,
        r#"
        (module
          (type $s (struct (field i32) (field i32)))
          (import "lib" "make" (func $make (result (ref $s))))
          (func (export "read1") (result i32)
            (struct.get $s 1 (call $make))))
    "#,
    );
    let cinst = linker.instantiate(&mut store, &consumer).unwrap();
    let read1 = cinst.get_func(&mut store, "read1").unwrap();
    let mut out = [Val::I32(0)];
    read1.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 22);
}

#[test]
fn same_rec_group_anyref_cast_succeeds() {
    // Exchange via `anyref` and a guest `ref.cast` — exercises the cast path (not just the typed
    // import) on a host-canonical id shared across modules.
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    let provider = module(
        &engine,
        r#"
        (module
          (type $s (struct (field i32) (field i32)))
          (func (export "make") (result anyref)
            (struct.new $s (i32.const 11) (i32.const 22))))
    "#,
    );
    let pinst = Instance::new(&mut store, &provider, &[]).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.instance(&mut store, "lib", pinst).unwrap();

    let consumer = module(
        &engine,
        r#"
        (module
          (type $s (struct (field i32) (field i32)))
          (import "lib" "make" (func $make (result anyref)))
          (func (export "read0") (result i32)
            (struct.get $s 0 (ref.cast (ref $s) (call $make)))))
    "#,
    );
    let cinst = linker.instantiate(&mut store, &consumer).unwrap();
    let read0 = cinst.get_func(&mut store, "read0").unwrap();
    let mut out = [Val::I32(0)];
    read0.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 11);
}

#[test]
fn mismatched_rec_group_fails_to_link() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    let provider = module(&engine, PROVIDER);
    let pinst = Instance::new(&mut store, &provider, &[]).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.instance(&mut store, "lib", pinst).unwrap();

    // Consumer's `$s` is a distinct rec group (different field types) ⇒ a distinct canonical id ⇒
    // the imported `make`'s result type doesn't match ⇒ link fails at instantiation.
    let consumer = module(
        &engine,
        r#"
        (module
          (type $s (struct (field i64) (field i64)))
          (import "lib" "make" (func $make (result (ref $s))))
          (func (export "noop")))
    "#,
    );
    let err = linker.instantiate(&mut store, &consumer).unwrap_err();
    assert!(
        err.to_string()
            .contains("imported function signature mismatch"),
        "unexpected link error: {err}"
    );
}
