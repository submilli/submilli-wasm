//! #27b host GC API: a host function constructs/inspects `struct`/`array` objects, reads back
//! guest-produced ones, and (Stage B) builds self-referential rec groups with `RecGroupBuilder`.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    AnyRef, ArrayRef, ArrayRefPre, ArrayType, Engine, FieldTemplate, FieldType, Finality, Instance,
    Module, Mutability, RecGroupBuilder, Rooted, StorageType, Store, StructRef, StructRefPre,
    StructSuperType, StructType, Val, ValType,
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

    // struct $node { mut i32 value; mut (ref null $node) next }  — self-reference via the label.
    let mut builder = RecGroupBuilder::new(&engine);
    let node = builder.declare_struct();
    builder.define_struct_with_finality_and_supertype(
        node,
        Finality::Final,
        None::<StructSuperType>,
        [
            FieldTemplate::new(Mutability::Var, ValType::I32),
            FieldTemplate::ref_(Mutability::Var, true, node),
        ],
    );
    let group = builder.build().unwrap();
    assert_eq!(group.len(), 1);
    let node_ty: StructType = group.struct_(node);

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
        b.define_struct(a, [FieldTemplate::ref_(Mutability::Var, true, bb)]);
        b.define_struct(bb, [FieldTemplate::ref_(Mutability::Var, true, a)]);
        let g = b.build().unwrap();
        (g.struct_(a), g.struct_(bb))
    };
    let (a1, b1) = build();
    let (a2, b2) = build();
    assert_eq!(a1, a2);
    assert_eq!(b1, b2);
    assert_ne!(a1, b1);
}

#[test]
fn rec_group_errors_on_undefined_member() {
    let engine = Engine::default();
    let mut b = RecGroupBuilder::new(&engine);
    let _s = b.declare_struct(); // never defined
    assert!(b.build().is_err());

    // A built type can be a supertype of a later, separately-built type.
    let mut base = RecGroupBuilder::new(&engine);
    let base_id = base.add_struct([FieldTemplate::new(Mutability::Const, ValType::I32)]);
    // base must be non-final to be a supertype.
    let mut nf = RecGroupBuilder::new(&engine);
    let nf_id = nf.declare_struct();
    nf.define_struct_with_finality_and_supertype(
        nf_id,
        Finality::NonFinal,
        None::<StructSuperType>,
        [FieldTemplate::new(Mutability::Const, ValType::I32)],
    );
    let base_group = nf.build().unwrap();
    let _ = base;
    let _ = base_id;

    let mut sub = RecGroupBuilder::new(&engine);
    let sub_id = sub.declare_struct();
    sub.define_struct_with_finality_and_supertype(
        sub_id,
        Finality::Final,
        Some(StructSuperType::Type(base_group.struct_(nf_id))),
        [
            FieldTemplate::new(Mutability::Const, ValType::I32),
            FieldTemplate::new(Mutability::Const, ValType::I32),
        ],
    );
    assert!(sub.build().is_ok());
}
