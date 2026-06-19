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
