//! Unit tests for the canonical type registry: cross-module dedup, declared-subtype chains,
//! and refcounted reclamation (the latter isn't directly exercised by the spec suite).

use super::*;
use crate::canon::{CompositeBody, IrField, IrStorage};

fn func(group: u32, supertype: Option<u32>) -> ModuleType {
    ModuleType {
        group,
        finality: Finality::NonFinal,
        supertype,
        body: CompositeBody::Func {
            params: Vec::new(),
            results: Vec::new(),
        },
    }
}

#[test]
fn structurally_identical_groups_share_a_canonical_id() {
    let mut reg = TypeRegistry::default();
    let (ids1, g1) = reg.intern_module(&[func(0, None)]);
    let (ids2, g2) = reg.intern_module(&[func(0, None)]);
    assert_eq!(ids1[0], ids2[0]); // cross-module dedup
    assert_eq!(g1, g2); // same group id, refcount now 2
}

#[test]
fn declared_supertype_chain_is_a_subtype() {
    let mut reg = TypeRegistry::default();
    let field = IrField {
        mutable: false,
        storage: IrStorage::I8,
    };
    let module = [
        ModuleType {
            group: 0,
            finality: Finality::NonFinal,
            supertype: None,
            body: CompositeBody::Struct(vec![field.clone()]),
        },
        ModuleType {
            group: 1,
            finality: Finality::Final,
            supertype: Some(0),
            body: CompositeBody::Struct(vec![field.clone(), field]),
        },
    ];
    let (ids, _) = reg.intern_module(&module);
    assert!(reg.is_subtype(ids[1], ids[0]));
    assert!(!reg.is_subtype(ids[0], ids[1]));
}

#[test]
fn refcount_reclaims_only_at_zero() {
    let mut reg = TypeRegistry::default();
    let (ids1, g1) = reg.intern_module(&[func(0, None)]);
    let (_ids2, g2) = reg.intern_module(&[func(0, None)]); // refcount 2
    reg.release(&g1);
    assert!(reg.kind(ids1[0]).is_some()); // still held by the second module
    reg.release(&g2);
    assert!(reg.kind(ids1[0]).is_none()); // reclaimed at refcount 0
}

// --- #27i: host-handle RAII reclamation (via the public API + `live_group_count`) ---

mod reclaim {
    #![allow(clippy::unwrap_used)]
    use crate::value::{
        ArrayType, FieldTemplate, FieldType, Finality, Mutability, RecGroupBuilder, StorageType,
        StructRef, StructRefPre, StructSuperType, StructType, Val, ValType,
    };
    use crate::{Engine, Store};

    fn i32_field(mutability: Mutability) -> FieldType {
        FieldType::new(mutability, StorageType::ValType(ValType::I32))
    }

    #[test]
    fn host_type_drop_reclaims() {
        let engine = Engine::default();
        let base = engine.live_group_count();
        {
            let _st = StructType::new(&engine, [i32_field(Mutability::Var)]).unwrap();
            let _at = ArrayType::new(&engine, i32_field(Mutability::Var));
            assert_eq!(engine.live_group_count(), base + 2);
        }
        assert_eq!(engine.live_group_count(), base); // both reclaimed on drop
    }

    #[test]
    fn clone_keeps_type_alive_until_last_drop() {
        let engine = Engine::default();
        let base = engine.live_group_count();
        let st = StructType::new(&engine, [i32_field(Mutability::Var)]).unwrap();
        let clone = st.clone();
        drop(st);
        assert_eq!(engine.live_group_count(), base + 1); // clone still holds it
        drop(clone);
        assert_eq!(engine.live_group_count(), base);
    }

    #[test]
    fn build_and_drop_loop_returns_to_baseline() {
        let engine = Engine::default();
        let base = engine.live_group_count();
        for _ in 0..100 {
            let mut b = RecGroupBuilder::new(&engine);
            let node = b.declare_struct();
            b.define_struct(
                node,
                [FieldTemplate::ref_(Mutability::Var, true, node)], // self-referential
            );
            let _g = b.build().unwrap();
        }
        assert_eq!(engine.live_group_count(), base); // no unbounded growth
    }

    #[test]
    fn cross_group_supertype_pins_until_both_drop() {
        let engine = Engine::default();
        let base = engine.live_group_count();
        // A non-final base group, then a separate group whose member subtypes it.
        let mut bb = RecGroupBuilder::new(&engine);
        let base_id = bb.declare_struct();
        bb.define_struct_with_finality_and_supertype(
            base_id,
            Finality::NonFinal,
            None,
            [FieldTemplate::from(i32_field(Mutability::Const))],
        );
        let base_group = bb.build().unwrap();

        let mut sb = RecGroupBuilder::new(&engine);
        let sub_id = sb.declare_struct();
        sb.define_struct_with_finality_and_supertype(
            sub_id,
            Finality::Final,
            Some(StructSuperType::Type(base_group.struct_(base_id))),
            [
                FieldTemplate::from(i32_field(Mutability::Const)),
                FieldTemplate::from(i32_field(Mutability::Const)),
            ],
        );
        let sub_group = sb.build().unwrap();
        assert_eq!(engine.live_group_count(), base + 2);

        drop(base_group); // sub still pins base via the supertype edge
        assert_eq!(engine.live_group_count(), base + 2);
        drop(sub_group); // reclaiming sub cascades into base
        assert_eq!(engine.live_group_count(), base);
    }

    #[test]
    fn host_object_outlives_its_type_handle() {
        let engine = Engine::default();
        let base = engine.live_group_count();
        let mut store = Store::new(&engine, ());
        let st = StructType::new(&engine, [i32_field(Mutability::Var)]).unwrap();
        let pre = StructRefPre::new(&mut store, st); // moves the StructType into the pre
        let s = StructRef::new(&mut store, &pre, &[Val::I32(7)]).unwrap();
        drop(pre); // drops the only StructType handle — but the store pinned the type
        assert_eq!(engine.live_group_count(), base + 1);
        assert_eq!(s.field(&mut store, 0).unwrap().unwrap_i32(), 7); // still readable
        drop(store); // store drop releases its pin → reclaimed
        assert_eq!(engine.live_group_count(), base);
    }
}
