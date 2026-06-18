//! `Linker<T>` tests: host-func resolution, define, multi-module linking, aliasing.
#![allow(clippy::unwrap_used)]

use crate::{
    Engine, Global, GlobalType, Instance, Linker, Module, Mutability, Store, Val, ValType,
};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

fn call_i32(store: &mut Store<()>, inst: Instance, name: &str) -> i32 {
    let f = inst.get_func(&mut *store, name).unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut *store, &[], &mut out).unwrap();
    out[0].unwrap_i32()
}

#[test]
fn func_wrap_resolves_and_calls() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap("h", "add", |a: i32, b: i32| a + b)
        .unwrap();
    let m = module(
        &engine,
        "(module
            (import \"h\" \"add\" (func $add (param i32 i32) (result i32)))
            (func (export \"run\") (result i32) i32.const 2 i32.const 3 call $add))",
    );
    let inst = linker.instantiate(&mut store, &m).unwrap();
    assert_eq!(call_i32(&mut store, inst, "run"), 5);
}

#[test]
fn define_extern_global() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let g = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(99),
    )
    .unwrap();
    let mut linker = Linker::new(&engine);
    linker.define(&store, "env", "g", g).unwrap();
    let m = module(
        &engine,
        "(module
            (import \"env\" \"g\" (global i32))
            (func (export \"get\") (result i32) global.get 0))",
    );
    let inst = linker.instantiate(&mut store, &m).unwrap();
    assert_eq!(call_i32(&mut store, inst, "get"), 99);
}

#[test]
fn instance_register_multi_module() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let provider = module(
        &engine,
        "(module (func (export \"f\") (result i32) i32.const 7))",
    );
    let pinst = Instance::new(&mut store, &provider, &[]).unwrap();
    let mut linker = Linker::new(&engine);
    linker.instance(&mut store, "a", pinst).unwrap();
    let consumer = module(
        &engine,
        "(module
            (import \"a\" \"f\" (func $f (result i32)))
            (func (export \"run\") (result i32) call $f))",
    );
    let cinst = linker.instantiate(&mut store, &consumer).unwrap();
    assert_eq!(call_i32(&mut store, cinst, "run"), 7);
}

#[test]
fn unknown_import_errors() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let linker: Linker<()> = Linker::new(&engine);
    let m = module(
        &engine,
        "(module (import \"missing\" \"x\" (func)) (func (export \"run\")))",
    );
    assert!(linker.instantiate(&mut store, &m).is_err());
}

#[test]
fn shadowing_is_rejected_unless_allowed() {
    let engine = Engine::default();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.func_wrap("h", "f", || {}).unwrap();
    assert!(linker.func_wrap("h", "f", || {}).is_err());
    linker.allow_shadowing(true);
    assert!(linker.func_wrap("h", "f", || {}).is_ok());
}

#[test]
fn alias_redirects_definition() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    linker.func_wrap("h", "f", || 5i32).unwrap();
    linker.alias("h", "f", "h2", "g").unwrap();
    let m = module(
        &engine,
        "(module
            (import \"h2\" \"g\" (func $g (result i32)))
            (func (export \"run\") (result i32) call $g))",
    );
    let inst = linker.instantiate(&mut store, &m).unwrap();
    assert_eq!(call_i32(&mut store, inst, "run"), 5);
}
