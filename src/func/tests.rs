//! `Func::call`/`Func::ty` end-to-end: drive the interpreter through the public
//! untyped API (an instantiated export called with `&[Val]`).
#![allow(clippy::unwrap_used)]

use crate::engine::Engine;
use crate::instance::Instance;
use crate::module::Module;
use crate::store::AsContextMut;
use crate::store::Store;
use crate::trap::Trap;
use crate::value::{
    ExnRef, ExnRefPre, ExnType, ExternRef, FieldType, Mutability, StorageType, StructRef,
    StructRefPre, StructType, TagType, Val, ValType,
};
use crate::{Tag, ThrownException};

fn instantiate(wat: &str) -> (Store<()>, Instance) {
    let engine = Engine::default();
    let bytes = wat::parse_str(wat).unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    (store, inst)
}

#[cfg(feature = "async")]
#[path = "async_tests.rs"]
mod async_tests;

#[cfg(feature = "async")]
#[path = "async_yield_tests.rs"]
mod async_yield_tests;

#[cfg(feature = "async")]
#[path = "async_limiter_tests.rs"]
mod async_limiter_tests;

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

// --- host functions ---

use crate::extern_::Extern;
use crate::func::{Caller, Func};
use crate::value::FuncType;
use crate::Error;

fn ft(engine: &Engine, params: &[ValType], results: &[ValType]) -> FuncType {
    FuncType::new(engine, params.iter().cloned(), results.iter().cloned())
}

#[test]
fn host_func_computes_result() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let add = Func::new(
        &mut store,
        ft(&engine, &[ValType::I32, ValType::I32], &[ValType::I32]),
        |_caller: Caller<'_, ()>, params, results| {
            results[0] = Val::I32(params[0].unwrap_i32() + params[1].unwrap_i32());
            Ok(())
        },
    );
    let mut out = [Val::I32(0)];
    add.call(&mut store, &[Val::I32(40), Val::I32(2)], &mut out)
        .unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

#[test]
fn direct_host_call_transfers_only_returned_gc_roots() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let struct_ty = StructType::new(
        &engine,
        [FieldType::new(
            Mutability::Var,
            StorageType::ValType(ValType::I32),
        )],
    )
    .unwrap();
    let pre = StructRefPre::new(&mut store, struct_ty);
    let host = Func::new(
        &mut store,
        ft(&engine, &[], &[ValType::ANYREF]),
        move |mut caller, _params, results| {
            let returned = StructRef::new(&mut caller, &pre, &[Val::I32(1)])?;
            let _temporary = StructRef::new(&mut caller, &pre, &[Val::I32(2)])?;
            results[0] = Val::AnyRef(Some(returned.to_anyref()));
            Ok(())
        },
    );
    let roots_before = store.inner.gc_roots_mark();
    let mut results = [Val::AnyRef(None)];
    host.call(&mut store, &[], &mut results).unwrap();
    assert_eq!(store.inner.gc_roots_mark(), roots_before + 1);
    store.gc();
    let Val::AnyRef(Some(returned)) = results[0] else {
        panic!("host did not return an anyref");
    };
    assert!(returned.as_struct(&store).unwrap().is_some());
}

#[test]
fn cross_store_host_result_is_an_error_and_cleans_up() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let mut other = Store::new(&engine, ());
    let foreign = ExternRef::new(&mut other, 7_u32).unwrap();
    let bad = Func::new(
        &mut store,
        ft(&engine, &[], &[ValType::EXTERNREF]),
        move |_caller, _params, results| {
            results[0] = Val::ExternRef(Some(foreign));
            Ok(())
        },
    );
    let mut result = [Val::ExternRef(None)];
    let call = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bad.call(&mut store, &[], &mut result)
    }));
    assert!(call.unwrap().is_err());

    let healthy = Func::wrap(&mut store, || 7_i32);
    let mut result = [Val::I32(0)];
    healthy.call(&mut store, &[], &mut result).unwrap();
    assert_eq!(result[0].unwrap_i32(), 7);
}

#[test]
fn pending_snapshot_survives_overwrite_collection_and_rollback() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let module = Module::new(
        &engine,
        wat::parse_str(
            "(module (tag $t (param i32))
                (func (export \"throw\") i32.const 9 throw $t))",
        )
        .unwrap(),
    )
    .unwrap();
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let thrower = instance.get_func(&mut store, "throw").unwrap();
    assert!(thrower
        .call(&mut store, &[], &mut [])
        .unwrap_err()
        .is::<ThrownException>());

    let tag_ty = TagType::new(FuncType::new(&engine, [], []));
    let tag = Tag::new(&mut store, &tag_ty).unwrap();
    let pre = ExnRefPre::new(&mut store, ExnType::from_tag_type(&tag_ty).unwrap());
    let bad = Func::new(
        &mut store,
        ft(&engine, &[], &[ValType::I32]),
        move |mut caller, _params, results| {
            let replacement = ExnRef::new(&mut caller, &pre, &tag, &[])?;
            let _ = caller.as_context_mut().throw::<()>(replacement);
            caller.as_context_mut().store_mut().gc();
            results[0] = Val::I64(1);
            Ok(())
        },
    );
    assert!(bad.call(&mut store, &[], &mut [Val::I32(0)]).is_err());

    let original = store.take_pending_exception().unwrap();
    store.gc();
    assert_eq!(original.field(&mut store, 0).unwrap().unwrap_i32(), 9);
}

#[test]
fn host_func_mutates_caller_data() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, 0u32);
    let bump = Func::new(
        &mut store,
        ft(&engine, &[], &[]),
        |mut caller: Caller<'_, u32>, _params, _results| {
            *caller.data_mut() += 1;
            Ok(())
        },
    );
    bump.call(&mut store, &[], &mut []).unwrap();
    bump.call(&mut store, &[], &mut []).unwrap();
    assert_eq!(*store.data(), 2);
}

#[test]
fn host_err_surfaces_as_trap() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let boom = Func::new(
        &mut store,
        ft(&engine, &[], &[]),
        |_caller: Caller<'_, ()>, _params, _results| Err(Error::from(Trap::UnreachableCodeReached)),
    );
    let err = boom.call(&mut store, &[], &mut []).unwrap_err();
    assert_eq!(
        *err.downcast_ref::<Trap>().unwrap(),
        Trap::UnreachableCodeReached
    );
}

#[test]
fn wasm_imports_and_calls_host_func() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let dbl = Func::new(
        &mut store,
        ft(&engine, &[ValType::I32], &[ValType::I32]),
        |_caller: Caller<'_, ()>, params, results| {
            results[0] = Val::I32(params[0].unwrap_i32() * 2);
            Ok(())
        },
    );
    let bytes = wat::parse_str(
        "(module
            (import \"h\" \"f\" (func (param i32) (result i32)))
            (func (export \"run\") (param i32) (result i32)
                local.get 0 call 0 i32.const 1 i32.add))",
    )
    .unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(dbl)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    run.call(&mut store, &[Val::I32(20)], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 41); // 20*2 + 1
}

// --- Caller::get_export + guest memory ---

#[test]
fn host_fn_writes_guest_memory_via_get_export() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    // store_byte(addr, val): reach the caller's exported memory and write one byte.
    let store_byte = Func::new(
        &mut store,
        ft(&engine, &[ValType::I32, ValType::I32], &[]),
        |mut caller: Caller<'_, ()>, params, _results| {
            let addr = params[0].unwrap_i32() as usize;
            let val = params[1].unwrap_i32() as u8;
            let Some(Extern::Memory(mem)) = caller.get_export("mem") else {
                return Err(Error::msg("no memory export"));
            };
            mem.write(&mut caller, addr, &[val]).map_err(Error::from)
        },
    );
    let bytes = wat::parse_str(
        "(module
            (import \"h\" \"store_byte\" (func $s (param i32 i32)))
            (memory (export \"mem\") 1)
            (func (export \"run\") (param i32 i32) local.get 0 local.get 1 call $s))",
    )
    .unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(store_byte)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    run.call(&mut store, &[Val::I32(8), Val::I32(0x2a)], &mut [])
        .unwrap();
    let mem = inst.get_memory(&mut store, "mem").unwrap();
    assert_eq!(mem.data(&store)[8], 0x2a);
}

#[test]
fn host_fn_reads_guest_memory_via_get_export() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let read_byte = Func::new(
        &mut store,
        ft(&engine, &[ValType::I32], &[ValType::I32]),
        |mut caller: Caller<'_, ()>, params, results| {
            let addr = params[0].unwrap_i32() as usize;
            let Some(Extern::Memory(mem)) = caller.get_export("mem") else {
                return Err(Error::msg("no memory export"));
            };
            results[0] = Val::I32(i32::from(mem.data(&caller)[addr]));
            Ok(())
        },
    );
    let bytes = wat::parse_str(
        "(module
            (import \"h\" \"read_byte\" (func $r (param i32) (result i32)))
            (memory (export \"mem\") 1)
            (data (i32.const 4) \"\\07\")
            (func (export \"run\") (param i32) (result i32) local.get 0 call $r))",
    )
    .unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(read_byte)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    run.call(&mut store, &[Val::I32(4)], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
}

#[test]
fn get_export_is_none_at_top_level() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let probe = Func::new(
        &mut store,
        ft(&engine, &[], &[ValType::I32]),
        |mut caller: Caller<'_, ()>, _params, results| {
            results[0] = Val::I32(i32::from(caller.get_export("mem").is_some()));
            Ok(())
        },
    );
    let mut out = [Val::I32(0)];
    probe.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 0);
}

#[test]
fn dynamic_host_result_type_is_checked() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let bad = Func::new(
        &mut store,
        ft(&engine, &[], &[ValType::I32]),
        |_caller, _params, results| {
            results[0] = Val::I64(7);
            Ok(())
        },
    );
    let mut results = [Val::I32(0)];
    let err = bad.call(&mut store, &[], &mut results).unwrap_err();
    assert!(err
        .to_string()
        .contains("function attempted to return an incompatible value"));
    assert_eq!(results[0].unwrap_i32(), 0);
}

// --- typed API ---

use crate::func::TypedFunc;

#[test]
fn typed_call_on_wasm_export() {
    let (mut store, inst) = instantiate(
        "(module (func (export \"add\") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add))",
    );
    let add: TypedFunc<(i32, i32), i32> = inst.get_typed_func(&mut store, "add").unwrap();
    assert_eq!(add.call(&mut store, (2, 3)).unwrap(), 5);
}

#[test]
fn typed_multi_value_results() {
    let (mut store, inst) =
        instantiate("(module (func (export \"pair\") (result i32 i64) i32.const 1 i64.const 2))");
    let pair: TypedFunc<(), (i32, i64)> = inst.get_typed_func(&mut store, "pair").unwrap();
    assert_eq!(pair.call(&mut store, ()).unwrap(), (1, 2));
}

#[test]
fn typed_signature_mismatch_errors() {
    let (mut store, inst) = instantiate(
        "(module (func (export \"add\") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.add))",
    );
    let f = inst.get_func(&mut store, "add").unwrap();
    // wrong result type
    assert!(f.typed::<(i32, i32), i64>(&store).is_err());
}

#[test]
fn wrap_typed_host_func_called_by_wasm() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let add = Func::wrap(&mut store, |a: i32, b: i32| a + b);
    let bytes = wat::parse_str(
        "(module
            (import \"h\" \"add\" (func (param i32 i32) (result i32)))
            (func (export \"run\") (param i32 i32) (result i32)
                local.get 0 local.get 1 call 0))",
    )
    .unwrap();
    let module = Module::new(&engine, &bytes).unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(add)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    run.call(&mut store, &[Val::I32(4), Val::I32(5)], &mut out)
        .unwrap();
    assert_eq!(out[0].unwrap_i32(), 9);
}

#[test]
fn wrap_caller_aware_and_result_trap() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, 10i32);
    let bump = Func::wrap(&mut store, |mut caller: Caller<'_, i32>, x: i32| {
        *caller.data_mut() += x;
    });
    bump.call(&mut store, &[Val::I32(5)], &mut []).unwrap();
    assert_eq!(*store.data(), 15);

    let boom = Func::wrap(&mut store, || -> crate::Result<i32> {
        Err(Error::from(Trap::UnreachableCodeReached))
    });
    let err = boom.call(&mut store, &[], &mut [Val::I32(0)]).unwrap_err();
    assert_eq!(
        *err.downcast_ref::<Trap>().unwrap(),
        Trap::UnreachableCodeReached
    );
}

#[test]
fn typed_untyped_agree() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let sq = Func::wrap(&mut store, |x: i32| x * x);
    let mut untyped = [Val::I32(0)];
    sq.call(&mut store, &[Val::I32(7)], &mut untyped).unwrap();
    let typed: TypedFunc<i32, i32> = sq.typed(&store).unwrap();
    assert_eq!(untyped[0].unwrap_i32(), typed.call(&mut store, 7).unwrap());
}
