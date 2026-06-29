//! Materialization: engine-canonical type ids → public handle types (`FuncType`/`StructType`/
//! `ArrayType` and the `ValType`/`FieldType` containing them).
//!
//! These are free functions that take only the `Engine` (for `from_id`'s incref) — never the
//! registry — so they run **outside** the registry lock. The lock-held phase (`*_raw` accessors on
//! `TypeRegistry`) clones the canonical body; these then build the public handles afterwards.
//! Keeping handle construction out from under the lock is what lets refcounted handles (#27i)
//! incref without nesting a write lock inside the read lock.

use super::keys::{abs_decode, num_decode, CField, CHeap, CStore, CVal, CanonRef};
use super::{AggKind, CanonicalTypeId};
use crate::engine::Engine;
use crate::value::{
    ArrayType, FieldType, FuncType, HeapType, Mutability, RefType, StorageType, StructType, ValType,
};

pub(crate) fn mat_val(engine: &Engine, v: &CVal) -> ValType {
    match v {
        CVal::Num(c) => num_decode(*c),
        CVal::Ref(nullable, h) => ValType::Ref(RefType::new(*nullable, mat_heap(engine, h))),
    }
}

fn mat_heap(engine: &Engine, h: &CHeap) -> HeapType {
    match h {
        CHeap::Abs(c) => abs_decode(*c),
        CHeap::Concrete(kind, CanonRef::Canon(id)) => match kind {
            AggKind::Func => HeapType::ConcreteFunc(FuncType::from_id(engine, *id)),
            AggKind::Struct => HeapType::ConcreteStruct(StructType::from_id(engine, *id)),
            AggKind::Array => HeapType::ConcreteArray(ArrayType::from_id(engine, *id)),
        },
        CHeap::Concrete(_, CanonRef::Rel(_)) => unreachable!("stored bodies use absolute ids"),
    }
}

pub(crate) fn mat_field(engine: &Engine, f: &CField) -> FieldType {
    let mutability = if f.mutable {
        Mutability::Var
    } else {
        Mutability::Const
    };
    let storage = match &f.storage {
        CStore::Packed(0) => StorageType::I8,
        CStore::Packed(_) => StorageType::I16,
        CStore::Val(v) => StorageType::ValType(mat_val(engine, v)),
    };
    FieldType::new(mutability, storage)
}

/// The materialized (public) param + result types of a func type. Two-phase: clone the canonical
/// body under the registry read lock, then build handles after the lock is released.
pub(crate) fn func_sig(engine: &Engine, id: CanonicalTypeId) -> (Vec<ValType>, Vec<ValType>) {
    let (p, r) = engine
        .types()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .func_body_raw(id);
    (
        p.iter().map(|v| mat_val(engine, v)).collect(),
        r.iter().map(|v| mat_val(engine, v)).collect(),
    )
}

/// The materialized fields of a struct type (two-phase, like [`func_sig`]).
pub(crate) fn struct_fields(engine: &Engine, id: CanonicalTypeId) -> Vec<FieldType> {
    let fields = engine
        .types()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .struct_fields_raw(id);
    fields.iter().map(|f| mat_field(engine, f)).collect()
}

/// The materialized element of an array type (two-phase, like [`func_sig`]).
pub(crate) fn array_field(engine: &Engine, id: CanonicalTypeId) -> FieldType {
    let field = engine
        .types()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .array_field_raw(id);
    mat_field(engine, &field)
}
