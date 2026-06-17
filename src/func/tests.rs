//! `Func::call`/`Func::ty` end-to-end: drive the interpreter through the public
//! untyped API (an instantiated export called with `&[Val]`).
#![allow(clippy::unwrap_used)]

use crate::engine::Engine;
use crate::instance::Instance;
use crate::module::Module;
use crate::store::Store;
use crate::trap::Trap;
use crate::value::{Val, ValType};

fn instantiate(wat: &str) -> (Store<()>, Instance) {
    let engine = Engine::default();
    let bytes = wat::parse_str(wat).unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    (store, inst)
}

#[test]
fn untyped_call_returns_result() {
    let (mut store, inst) = instantiate(
        "(module (func (export \"add\") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add))",
    );
    let add = inst.get_func(&mut store, "add").unwrap();
    let mut results = [Val::I32(0)];
    add.call(&mut store, &[Val::I32(40), Val::I32(2)], &mut results)
        .unwrap();
    assert_eq!(results[0].unwrap_i32(), 42);
}

#[test]
fn multi_value_results_written() {
    let (mut store, inst) =
        instantiate("(module (func (export \"pair\") (result i32 i32) i32.const 1 i32.const 2))");
    let f = inst.get_func(&mut store, "pair").unwrap();
    let mut results = [Val::I32(0), Val::I32(0)];
    f.call(&mut store, &[], &mut results).unwrap();
    assert_eq!(results[0].unwrap_i32(), 1);
    assert_eq!(results[1].unwrap_i32(), 2);
}

#[test]
fn ty_reports_signature() {
    let (mut store, inst) =
        instantiate("(module (func (export \"f\") (param i32 f64) (result i64) i64.const 0))");
    let f = inst.get_func(&mut store, "f").unwrap();
    let ty = f.ty(&store);
    assert_eq!(
        ty.params().collect::<Vec<_>>(),
        vec![ValType::I32, ValType::F64]
    );
    assert_eq!(ty.results().collect::<Vec<_>>(), vec![ValType::I64]);
}

#[test]
fn trap_propagates_as_err() {
    let (mut store, inst) =
        instantiate("(module (func (export \"boom\") (result i32) unreachable))");
    let f = inst.get_func(&mut store, "boom").unwrap();
    let mut results = [Val::I32(0)];
    let err = f.call(&mut store, &[], &mut results).unwrap_err();
    assert_eq!(
        *err.downcast_ref::<Trap>().unwrap(),
        Trap::UnreachableCodeReached
    );
}

#[test]
fn wrong_arg_count_is_error_not_panic() {
    let (mut store, inst) =
        instantiate("(module (func (export \"id\") (param i32) (result i32) local.get 0))");
    let f = inst.get_func(&mut store, "id").unwrap();
    let mut results = [Val::I32(0)];
    assert!(f.call(&mut store, &[], &mut results).is_err());
}
