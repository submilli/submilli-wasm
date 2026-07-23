//! #27b host GC API: a host function constructs/inspects `struct`/`array` objects, reads back
//! guest-produced ones, and (Stage B) builds self-referential rec groups with `RecGroupBuilder`.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    AnyRef, ArrayRef, ArrayRefPre, ArrayType, Engine, FieldType, Finality, Instance, Module,
    Mutability, RecGroupBuilder, Rooted, StorageType, Store, StructRef, StructRefPre, StructType,
    Val, ValType,
};

#[test]
fn host_builds_and_reads_struct_and_array() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    // struct { var i32, var i8 } — the i8 field is packed (1 byte, truncating).
    let st = StructType::new(
        &engine,
        [
            FieldType::new(Mutability::Var, StorageType::ValType(ValType::I32)),
            FieldType::new(Mutability::Var, StorageType::I8),
        ],
    )
    .unwrap();
    let pre = StructRefPre::new(&mut store, st);
    let s = StructRef::new(&mut store, &pre, &[Val::I32(100), Val::I32(0x1FF)]).unwrap();
    assert_eq!(s.field(&mut store, 0).unwrap().unwrap_i32(), 100);
    assert_eq!(s.field(&mut store, 1).unwrap().unwrap_i32(), 0xFF); // packed i8 truncation
    assert!(s.field(&mut store, 2).is_err()); // out of bounds

    // Wrong field count / type are host errors, not panics.
    assert!(StructRef::new(&mut store, &pre, &[Val::I32(1)]).is_err());
    assert!(StructRef::new(&mut store, &pre, &[Val::I64(1), Val::I32(2)]).is_err());

    // array i32[3] filled with 9.
    let at = ArrayType::new(
        &engine,
        FieldType::new(Mutability::Var, StorageType::ValType(ValType::I32)),
    );
    let apre = ArrayRefPre::new(&mut store, at);
    let a = ArrayRef::new(&mut store, &apre, &Val::I32(9), 3).unwrap();
    assert_eq!(a.len(&store).unwrap(), 3);
    assert_eq!(a.get(&mut store, 0).unwrap().unwrap_i32(), 9);
    assert_eq!(a.get(&mut store, 2).unwrap().unwrap_i32(), 9);
    assert!(a.get(&mut store, 3).is_err());

    let a2 = ArrayRef::new_fixed(&mut store, &apre, &[Val::I32(7), Val::I32(8)]).unwrap();
    assert_eq!(a2.len(&store).unwrap(), 2);
    assert_eq!(a2.get(&mut store, 1).unwrap().unwrap_i32(), 8);

    // Upcast to anyref and reinterpret; a struct is not an array (and vice versa).
    let any: Rooted<AnyRef> = s.into();
    assert_eq!(
        any.unwrap_struct(&store)
            .unwrap()
            .field(&mut store, 0)
            .unwrap()
            .unwrap_i32(),
        100
    );
    assert!(any.unwrap_array(&store).is_err());
    let arr_any: Rooted<AnyRef> = a.into();
    assert!(arr_any.unwrap_struct(&store).is_err());
    assert!(arr_any.unwrap_array(&store).is_ok());
}

#[test]
fn array_i8_slice_roundtrip() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    let array_ty = ArrayType::new(&engine, FieldType::new(Mutability::Var, StorageType::I8));
    let pre = ArrayRefPre::new(&mut store, array_ty);

    let src = [0x80, 0xFF, 0x00, 0x01, 0x7F];
    let array = ArrayRef::new_from_i8_slice(&mut store, &pre, &src).unwrap();
    assert_eq!(array.len(&store).unwrap(), 5);

    let mut dst = [0u8; 5];
    array.copy_to_i8_slice(&store, &mut dst).unwrap();
    assert_eq!(dst, src);

    // Normal `get` reads packed i8 elements with the zero-extended `array.get_u`
    // interpretation.
    assert_eq!(array.get(&store, 0).unwrap().unwrap_i32(), 128);
    assert_eq!(array.get(&store, 1).unwrap().unwrap_i32(), 255);

    let fixed = ArrayRef::new_fixed(
        &mut store,
        &pre,
        &src.iter().map(|&b| Val::I32(b.into())).collect::<Vec<_>>(),
    )
    .unwrap();
    for i in 0..src.len() as u32 {
        assert_eq!(
            array.get(&store, i).unwrap().unwrap_i32(),
            fixed.get(&store, i).unwrap().unwrap_i32(),
        );
    }

    let empty = ArrayRef::new_from_i8_slice(&mut store, &pre, &[]).unwrap();
    assert_eq!(empty.len(&store).unwrap(), 0);
    empty.copy_to_i8_slice(&store, &mut []).unwrap();

    assert!(array.copy_to_i8_slice(&store, &mut [0u8; 3]).is_err());

    let i32_ty = ArrayType::new(
        &engine,
        FieldType::new(Mutability::Var, StorageType::ValType(ValType::I32)),
    );
    let i32_pre = ArrayRefPre::new(&mut store, i32_ty);
    assert!(ArrayRef::new_from_i8_slice(&mut store, &i32_pre, &[1, 2, 3]).is_err());
    let i32_array = ArrayRef::new(&mut store, &i32_pre, &Val::I32(0), 3).unwrap();
    assert!(i32_array.copy_to_i8_slice(&store, &mut [0u8; 3]).is_err());
}

#[test]
#[cfg(feature = "async")]
fn array_i8_slice_async() {
    let mut config = submilli_wasm::Config::new();
    config.async_support(true);
    let engine = Engine::new(&config).unwrap();
    let mut store = Store::new(&engine, ());

    let array_ty = ArrayType::new(&engine, FieldType::new(Mutability::Var, StorageType::I8));
    let pre = ArrayRefPre::new(&mut store, array_ty);

    let src = [0x80, 0xFF, 0x00, 0x01, 0x7F];
    let array =
        pollster::block_on(ArrayRef::new_from_i8_slice_async(&mut store, &pre, &src)).unwrap();
    assert_eq!(array.len(&store).unwrap(), 5);

    let mut dst = [0u8; 5];
    array.copy_to_i8_slice(&store, &mut dst).unwrap();
    assert_eq!(dst, src);
}

#[test]
fn host_reads_guest_produced_struct() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let wat = r#"
        (module
          (type $s (struct (field i32) (field (mut i8))))
          (func (export "make") (result (ref $s))
            (struct.new $s (i32.const 42) (i32.const 7))))
    "#;
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let make = inst.get_func(&mut store, "make").unwrap();
    let mut out = [Val::I32(0)];
    make.call(&mut store, &[], &mut out).unwrap();

    // The guest returned a structref (anyref-encoded); the host reads its fields.
    let Val::AnyRef(Some(any)) = out[0] else {
        panic!("expected a non-null anyref result");
    };
    let s = any.unwrap_struct(&store).unwrap();
    assert_eq!(s.field(&mut store, 0).unwrap().unwrap_i32(), 42);
    assert_eq!(s.field(&mut store, 1).unwrap().unwrap_i32(), 7);
}

#[test]
fn host_struct_matches_guest_canonical_type() {
    // A host-built struct of the same structure as the guest's declared type shares the canonical
    // id, so the guest can `ref.cast` + `struct.get` a host-produced object.
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let wat = r#"
        (module
          (type $s (struct (field i32)))
          (func (export "read") (param anyref) (result i32)
            (struct.get $s 0 (ref.cast (ref $s) (local.get 0)))))
    "#;
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();

    // Immutable field, to match the guest's `(field i32)` exactly (same canonical type).
    let st = StructType::new(
        &engine,
        [FieldType::new(
            Mutability::Const,
            StorageType::ValType(ValType::I32),
        )],
    )
    .unwrap();
    let pre = StructRefPre::new(&mut store, st);
    let s = StructRef::new(&mut store, &pre, &[Val::I32(123)]).unwrap();

    let read = inst.get_func(&mut store, "read").unwrap();
    let mut out = [Val::I32(0)];
    let any: Rooted<AnyRef> = s.into();
    read.call(&mut store, &[Val::AnyRef(Some(any))], &mut out)
        .unwrap();
    assert_eq!(out[0].unwrap_i32(), 123);
}

// --- Stage B: RecGroupBuilder (self-referential / mutually-recursive host types) ---

#[test]
fn self_referential_struct_round_trip() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());

    // struct $node { mut i32 value; mut (ref null $node) next }  — self-reference via the handle.
    let mut builder = RecGroupBuilder::new(&engine);
    let node = builder.declare_struct();
    builder
        .define_struct(node)
        .field(FieldType::new(
            Mutability::Var,
            StorageType::ValType(ValType::I32),
        ))
        .forward_ref_field(node)
        .mutability(Mutability::Var)
        .nullable(true)
        .finish()
        .finish();
    let group = builder.build().unwrap();
    assert_eq!(group.len(), 1);
    let node_ty: StructType = group.get_struct(node).unwrap();

    // Allocate two nodes; link the first to the second through the self-referential field.
    let pre = StructRefPre::new(&mut store, node_ty);
    let tail = StructRef::new(&mut store, &pre, &[Val::I32(2), Val::AnyRef(None)]).unwrap();
    let head = StructRef::new(
        &mut store,
        &pre,
        &[Val::I32(1), Val::AnyRef(Some(tail.into()))],
    )
    .unwrap();

    assert_eq!(head.field(&mut store, 0).unwrap().unwrap_i32(), 1);
    // Follow `head.next` → the tail node, read its value.
    let next = head.field(&mut store, 1).unwrap();
    let Val::AnyRef(Some(next)) = next else {
        panic!("next should be a non-null anyref");
    };
    let tail2 = next.unwrap_struct(&store).unwrap();
    assert_eq!(tail2.field(&mut store, 0).unwrap().unwrap_i32(), 2);
}

#[test]
fn mutually_recursive_and_cross_builder_identity() {
    let engine = Engine::default();

    // A pair { $a { ref null $b }, $b { ref null $a } } built twice — structurally identical
    // groups must intern to the SAME canonical ids (cross-builder dedup).
    let build = || {
        let mut b = RecGroupBuilder::new(&engine);
        let a = b.declare_struct();
        let bb = b.declare_struct();
        b.define_struct(a)
            .forward_ref_field(bb)
            .mutability(Mutability::Var)
            .finish()
            .finish();
        b.define_struct(bb)
            .forward_ref_field(a)
            .mutability(Mutability::Var)
            .finish()
            .finish();
        let g = b.build().unwrap();
        (g.get_struct(a).unwrap(), g.get_struct(bb).unwrap())
    };
    let (a1, b1) = build();
    let (a2, b2) = build();
    assert_eq!(a1, a2);
    assert_eq!(b1, b2);
    assert_ne!(a1, b1);
}

#[test]
fn rec_group_build_errors_and_edge_cases() {
    let engine = Engine::default();
    let i32_field = || FieldType::new(Mutability::Const, StorageType::ValType(ValType::I32));

    // Declared but never defined (no `finish`) is a build error; an empty group is fine.
    let mut b = RecGroupBuilder::new(&engine);
    let _s = b.declare_struct();
    assert!(b.build().is_err());
    assert!(RecGroupBuilder::new(&engine).build().unwrap().is_empty());

    // A non-final registered struct can be the supertype of a later, separately-built one.
    let mut nf = RecGroupBuilder::new(&engine);
    let nf_id = nf.declare_struct();
    nf.define_struct(nf_id)
        .finality(Finality::NonFinal)
        .field(i32_field())
        .finish();
    let base_group = nf.build().unwrap();
    let base_ty = base_group.get_struct(nf_id).unwrap();

    let mut sub = RecGroupBuilder::new(&engine);
    let sub_id = sub.declare_struct();
    sub.define_struct(sub_id)
        .supertype(base_ty.clone())
        .field(i32_field())
        .field(i32_field())
        .finish();
    assert!(sub.build().is_ok());

    // Subtyping a *final* type is rejected at build().
    let mut bad = RecGroupBuilder::new(&engine);
    let final_id = bad.declare_struct();
    bad.define_struct(final_id).field(i32_field()).finish();
    let final_group = bad.build().unwrap();
    let mut bad = RecGroupBuilder::new(&engine);
    let bad_id = bad.declare_struct();
    bad.define_struct(bad_id)
        .supertype(final_group.get_struct(final_id).unwrap())
        .field(i32_field())
        .finish();
    assert!(bad.build().is_err());

    // Fields that don't match the supertype's prefix are rejected at build().
    let mut bad = RecGroupBuilder::new(&engine);
    let bad_id = bad.declare_struct();
    bad.define_struct(bad_id)
        .supertype(base_ty)
        .field(FieldType::new(
            Mutability::Const,
            StorageType::ValType(ValType::I64),
        ))
        .finish();
    assert!(bad.build().is_err());
}

/// Host-call params must survive a collection triggered from *inside* the call. The run loop
/// pops params off the operand stack (out of the collector's root shadow), so they are
/// reachable only through the host-call root bracket; churning past the tiny reservation
/// forces a mid-call collection that would otherwise free the guest's struct while the host
/// still holds it.
#[test]
fn host_call_params_survive_host_triggered_collection() {
    use submilli_wasm::{Caller, Collector, Config, Func, FuncType, HeapType, RefType};

    let mut cfg = Config::new();
    cfg.collector(Collector::Auto).gc_heap_reservation(64 << 10);
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&engine, ());

    let churn_engine = engine.clone();
    let anyref = ValType::Ref(RefType::new(true, HeapType::Any));
    let pin = Func::new(
        &mut store,
        FuncType::new(&engine, [anyref], [ValType::I32]),
        move |mut caller: Caller<'_, ()>, params, results| {
            // ~320 KiB of host-built garbage against a 64 KiB budget → collection mid-call.
            let st = StructType::new(
                &churn_engine,
                [FieldType::new(
                    Mutability::Var,
                    StorageType::ValType(ValType::I64),
                )],
            )?;
            let pre = StructRefPre::new(&mut caller, st);
            for _ in 0..20_000 {
                StructRef::new(&mut caller, &pre, &[Val::I64(0)])?;
            }
            let Val::AnyRef(Some(param)) = params[0] else {
                return Err(submilli_wasm::Error::msg("expected a struct param"));
            };
            results[0] = param.unwrap_struct(&caller)?.field(&mut caller, 0)?;
            Ok(())
        },
    );

    let wat = r#"(module
        (type $s (struct (field i32)))
        (import "h" "pin" (func $pin (param anyref) (result i32)))
        (func (export "run") (result i32)
            (call $pin (struct.new $s (i32.const 42)))))"#;
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[pin.into()]).unwrap();
    let run = inst.get_typed_func::<(), i32>(&mut store, "run").unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}

/// The epoch-deadline callback is host code too: a collection it triggers must see the guest's
/// live operand stack as roots (the callback runs with the execution parked, like a host call).
/// The guest allocates a struct, then a host call increments the epoch past the deadline, so
/// the callback fires — exactly once, deterministically — at the next op, with the struct on
/// the operand stack; its churn past the tiny reservation forces a collection right there.
#[test]
fn operand_stack_survives_epoch_callback_collection() {
    use submilli_wasm::{Caller, Collector, Config, Func, FuncType, UpdateDeadline};

    let mut cfg = Config::new();
    cfg.collector(Collector::Auto)
        .gc_heap_reservation(64 << 10)
        .epoch_interruption(true);
    let engine = Engine::new(&cfg).unwrap();
    let mut store = Store::new(&engine, ());

    let churn_engine = engine.clone();
    store.epoch_deadline_callback(move |mut ctx| {
        let st = StructType::new(
            &churn_engine,
            [FieldType::new(
                Mutability::Var,
                StorageType::ValType(ValType::I64),
            )],
        )?;
        let pre = StructRefPre::new(&mut ctx, st);
        for _ in 0..20_000 {
            StructRef::new(&mut ctx, &pre, &[Val::I64(0)])?;
        }
        Ok(UpdateDeadline::Continue(u64::MAX))
    });
    store.set_epoch_deadline(1);

    let arm_engine = engine.clone();
    let arm = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        move |_caller: Caller<'_, ()>, _params, _results| {
            arm_engine.increment_epoch();
            Ok(())
        },
    );

    let wat = r#"(module
        (type $s (struct (field i32)))
        (import "h" "arm" (func $arm))
        (func (export "run") (result i32)
            (struct.new $s (i32.const 42))
            (call $arm)
            (struct.get $s 0)))"#;
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[arm.into()]).unwrap();
    let run = inst.get_typed_func::<(), i32>(&mut store, "run").unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
}
