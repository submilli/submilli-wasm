//! Phase-4 gate (#26e): reference-types + function-references regression tests.
//!
//! The spec `.wast` suite is the conformance oracle; these pin the subtle, error-prone
//! bits independently of the vendored submodule: `call_ref`/`ref.as_non_null` dispatch,
//! the mirror-image `br_on_null`/`br_on_non_null` value placement (`keep`/`pop` with
//! operands *below* the reference), expression-initialized tables, and non-nullable
//! local-init validation (block-scoped init must not persist past a control-flow merge).

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Engine, Instance, Module, Store, Trap, Val};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

fn validates(engine: &Engine, wat: &str) -> bool {
    Module::validate(engine, &wat::parse_str(wat).unwrap()).is_ok()
}

fn run_i32(store: &mut Store<()>, inst: Instance, name: &str, arg: &[Val]) -> i32 {
    let f = inst.get_func(&mut *store, name).unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut *store, arg, &mut out).unwrap();
    out[0].unwrap_i32()
}

// --- call_ref / ref.as_non_null / table init ------------------------------

#[test]
fn call_ref_dispatches_to_funcref() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (type $t (func (param i32) (result i32)))
            (func $double (type $t) local.get 0 local.get 0 i32.add)
            (elem declare func $double)
            (func (export \"run\") (param i32) (result i32)
                local.get 0
                ref.func $double
                call_ref $t))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(run_i32(&mut store, inst, "run", &[Val::I32(21)]), 42);
}

#[test]
fn call_ref_null_traps() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (type $t (func (result i32)))
            (func (export \"run\") (result i32)
                (call_ref $t (ref.null $t))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "run").unwrap();
    let err = f.call(&mut store, &[], &mut [Val::I32(0)]).unwrap_err();
    assert_eq!(*err.downcast_ref::<Trap>().unwrap(), Trap::NullReference);
}

#[test]
fn ref_as_non_null_passes_and_traps() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (func $f)
            (elem declare func $f)
            (func (export \"ok\") (result i32)
                (ref.is_null (ref.as_non_null (ref.func $f))))
            (func (export \"trap\")
                (ref.null func) ref.as_non_null drop))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(run_i32(&mut store, inst, "ok", &[]), 0); // non-null, no trap
    let f = inst.get_func(&mut store, "trap").unwrap();
    let err = f.call(&mut store, &[], &mut []).unwrap_err();
    assert_eq!(*err.downcast_ref::<Trap>().unwrap(), Trap::NullReference);
}

#[test]
fn table_with_ref_func_initializer() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (type $t (func (result i32)))
            (func $f (type $t) (i32.const 42))
            (table $tab 1 1 (ref $t) (ref.func $f))
            (func (export \"run\") (result i32)
                (call_ref $t (table.get $tab (i32.const 0)))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(run_i32(&mut store, inst, "run", &[]), 42);
}

// --- br_on_null / br_on_non_null value placement (mirror-image) -----------

#[test]
fn br_on_null_and_non_null_branch_both_ways() {
    let engine = Engine::default();
    // `bon`: 1 if the funcref param is null, else 0 (via br_on_null).
    // `bonn`: 0 if non-null, else 1 (via br_on_non_null).
    let m = module(
        &engine,
        "(module
            (func $f)
            (elem declare func $f)
            (func (export \"bon\") (param funcref) (result i32)
                (block $b
                    local.get 0
                    br_on_null $b
                    drop
                    (return (i32.const 0)))
                (i32.const 1))
            (func (export \"bonn\") (param funcref) (result i32)
                (block $b (result funcref)
                    local.get 0
                    br_on_non_null $b
                    (return (i32.const 1)))
                drop
                (i32.const 0))
            (func (export \"mkref\") (result funcref) (ref.func $f)))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = {
        let f = inst.get_func(&mut store, "mkref").unwrap();
        let mut out = [Val::FuncRef(None)];
        f.call(&mut store, &[], &mut out).unwrap();
        out[0]
    };
    assert_eq!(run_i32(&mut store, inst, "bon", &[Val::FuncRef(None)]), 1);
    assert_eq!(run_i32(&mut store, inst, "bon", &[r]), 0);
    assert_eq!(run_i32(&mut store, inst, "bonn", &[Val::FuncRef(None)]), 1);
    assert_eq!(run_i32(&mut store, inst, "bonn", &[r]), 0);
}

#[test]
fn br_on_keeps_operands_below_the_ref() {
    let engine = Engine::default();
    // A value sits *below* the reference on the stack, so `keep`/`pop` must move the right
    // operands. `bon`: null → label keeps the lower value (10), ref dropped; non-null →
    // keeps [10, ref], drops ref, adds 20 → 30. `bonn`: non-null → label carries [10, ref],
    // drop ref + add 5 → 15; null → ref dropped, lower value + 7 → 17.
    let m = module(
        &engine,
        "(module
            (func $f)
            (elem declare func $f)
            (func (export \"bon\") (param funcref) (result i32)
                (block $b (result i32)
                    (i32.const 10)
                    local.get 0
                    br_on_null $b
                    drop
                    (i32.const 20)
                    i32.add))
            (func (export \"bonn\") (param funcref) (result i32)
                (block $b (result i32 funcref)
                    (i32.const 10)
                    local.get 0
                    br_on_non_null $b
                    (i32.const 7)
                    i32.add
                    (return))
                drop
                (i32.const 5)
                i32.add)
            (func (export \"mkref\") (result funcref) (ref.func $f)))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = {
        let f = inst.get_func(&mut store, "mkref").unwrap();
        let mut out = [Val::FuncRef(None)];
        f.call(&mut store, &[], &mut out).unwrap();
        out[0]
    };
    assert_eq!(run_i32(&mut store, inst, "bon", &[Val::FuncRef(None)]), 10);
    assert_eq!(run_i32(&mut store, inst, "bon", &[r]), 30);
    assert_eq!(run_i32(&mut store, inst, "bonn", &[r]), 15);
    assert_eq!(run_i32(&mut store, inst, "bonn", &[Val::FuncRef(None)]), 17);
}

// --- non-nullable local-init validation -----------------------------------

#[test]
fn non_nullable_local_init_accepts_set_before_get() {
    let engine = Engine::default();
    assert!(validates(
        &engine,
        "(module
            (func $f)
            (elem declare func $f)
            (func
                (local $x (ref func))
                (local.set $x (ref.func $f))
                (drop (local.get $x))))",
    ));
}

#[test]
fn non_nullable_local_init_rejects_read_before_set() {
    let engine = Engine::default();
    assert!(!validates(
        &engine,
        "(module
            (func
                (local $x (ref func))
                (drop (local.get $x))))",
    ));
}

#[test]
fn non_nullable_local_init_rejects_conditional_init() {
    let engine = Engine::default();
    // Set only in the `then` arm: after the `if` merge the local is not definitely
    // initialized, so the later `local.get` must be rejected (block-scoped init does
    // not persist past the merge).
    assert!(!validates(
        &engine,
        "(module
            (func $f)
            (elem declare func $f)
            (func (param i32)
                (local $x (ref func))
                (if (local.get 0) (then (local.set $x (ref.func $f))))
                (drop (local.get $x))))",
    ));
}
