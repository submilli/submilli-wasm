//! Post-registration validation of declared supertypes: a member must structurally match its
//! supertype (which must be non-final). Runs after interning so sibling forward references have
//! resolved to registered types; declared concrete-to-concrete subtyping is checked against the
//! engine registry (`Engine::is_subtype`) on top of the abstract lattice (`HeapType::matches`).

use super::{RecGroup, SuperDef};
use crate::canon::{AggKind, CanonicalTypeId};
use crate::engine::Engine;
use crate::value::{FieldType, Finality, HeapType, Mutability, StorageType, ValType};
use crate::{bail, ensure, Result};

/// Validates member `i`'s declared supertype (if any), now that the group is registered.
pub(super) fn supertype(
    engine: &Engine,
    group: &RecGroup,
    i: usize,
    sup: Option<&SuperDef>,
) -> Result<()> {
    let Some(sup) = sup else {
        return Ok(());
    };
    let kind = group.kinds[i];
    let sup_id = match sup {
        SuperDef::Forward(j) => {
            let j = *j as usize;
            ensure!(
                group.kinds[j] == kind,
                "a {} type's supertype must be a {} type",
                kind_name(kind),
                kind_name(kind),
            );
            group.ids[j]
        }
        SuperDef::Struct(t) => t.canonical_id(),
        SuperDef::Array(t) => t.canonical_id(),
        SuperDef::Func(t) => t.canonical_id(),
    };
    ensure!(
        engine.type_finality(sup_id) == Finality::NonFinal,
        "cannot create a subtype of a final supertype"
    );

    let sub_id = group.ids[i];
    match kind {
        AggKind::Struct => {
            let sub = engine.struct_fields(sub_id);
            let sup = engine.struct_fields(sup_id);
            let matches = sub.len() >= sup.len()
                && sub
                    .iter()
                    .zip(&sup)
                    .all(|(a, b)| field_matches(engine, a, b));
            ensure!(matches, "struct fields must match their supertype's fields");
        }
        AggKind::Array => ensure!(
            field_matches(
                engine,
                &engine.array_field(sub_id),
                &engine.array_field(sup_id)
            ),
            "array field type must match its supertype's field type"
        ),
        AggKind::Func => {
            if !func_matches(engine, sub_id, sup_id) {
                bail!("function type must match its supertype");
            }
        }
    }
    Ok(())
}

fn kind_name(kind: AggKind) -> &'static str {
    match kind {
        AggKind::Struct => "struct",
        AggKind::Array => "array",
        AggKind::Func => "function",
    }
}

/// Does field `sub` match (subtype) field `sup`? Mutability must agree; mutable fields are
/// invariant, immutable ones covariant.
fn field_matches(engine: &Engine, sub: &FieldType, sup: &FieldType) -> bool {
    if sub.mutability() != sup.mutability() {
        return false;
    }
    match sub.mutability() {
        Mutability::Var => sub.element_type() == sup.element_type(),
        Mutability::Const => storage_matches(engine, sub.element_type(), sup.element_type()),
    }
}

fn storage_matches(engine: &Engine, sub: &StorageType, sup: &StorageType) -> bool {
    match (sub, sup) {
        (StorageType::ValType(a), StorageType::ValType(b)) => val_matches(engine, a, b),
        _ => sub == sup,
    }
}

/// Structural function subtyping: equal arity, contravariant params, covariant results.
fn func_matches(engine: &Engine, sub: CanonicalTypeId, sup: CanonicalTypeId) -> bool {
    let (sub_params, sub_results) = engine.func_sig(sub);
    let (sup_params, sup_results) = engine.func_sig(sup);
    sub_params.len() == sup_params.len()
        && sub_results.len() == sup_results.len()
        && sup_params
            .iter()
            .zip(&sub_params)
            .all(|(a, b)| val_matches(engine, a, b))
        && sub_results
            .iter()
            .zip(&sup_results)
            .all(|(a, b)| val_matches(engine, a, b))
}

/// [`ValType::matches`] extended with declared concrete-to-concrete subtyping.
fn val_matches(engine: &Engine, sub: &ValType, sup: &ValType) -> bool {
    if sub.matches(sup) {
        return true;
    }
    let (ValType::Ref(a), ValType::Ref(b)) = (sub, sup) else {
        return false;
    };
    (!a.is_nullable() || b.is_nullable()) && heap_matches(engine, a.heap_type(), b.heap_type())
}

fn heap_matches(engine: &Engine, sub: &HeapType, sup: &HeapType) -> bool {
    use HeapType as H;
    if sub.matches(sup) {
        return true;
    }
    match (sub, sup) {
        (H::ConcreteStruct(a), H::ConcreteStruct(b)) => {
            engine.is_subtype(a.canonical_id(), b.canonical_id())
        }
        (H::ConcreteArray(a), H::ConcreteArray(b)) => {
            engine.is_subtype(a.canonical_id(), b.canonical_id())
        }
        (H::ConcreteFunc(a), H::ConcreteFunc(b)) => {
            engine.is_subtype(a.canonical_id(), b.canonical_id())
        }
        _ => false,
    }
}
