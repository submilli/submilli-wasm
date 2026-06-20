//! Lowering: `RecGroupBuilder` member templates → module IR. Sibling labels become relative
//! concrete refs (`IrHeap::Concrete(index, _)`, `index < n`); already-registered types are
//! appended to an `externals` table and referenced as `IrHeap::Concrete(n + j, _)`.

use super::template::{
    ArraySuperType, FieldTemplate, FuncSuperType, HeapTypeTemplate, StorageTypeTemplate,
    StructSuperType, ValTypeTemplate,
};
use super::MemberDef;
use crate::canon::{AggKind, CanonicalTypeId, CompositeBody, IrField, IrHeap, IrStorage, IrVal};
use crate::value::{Finality, HeapType, Mutability, StorageType, ValType};

/// Lowers one member to `(finality, supertype-index, body, kind)`, recording any external refs.
pub(super) fn lower(
    def: &MemberDef,
    n: usize,
    ext: &mut Vec<CanonicalTypeId>,
) -> (Finality, Option<u32>, CompositeBody, AggKind) {
    match def {
        MemberDef::Struct {
            finality,
            supertype,
            fields,
        } => {
            let st = supertype.as_ref().map(|s| match s {
                StructSuperType::Local(id) => id.index,
                StructSuperType::Type(t) => external(ext, n, t.canonical_id()),
            });
            let body = CompositeBody::Struct(fields.iter().map(|f| field_ir(f, n, ext)).collect());
            (*finality, st, body, AggKind::Struct)
        }
        MemberDef::Array {
            finality,
            supertype,
            field,
        } => {
            let st = supertype.as_ref().map(|s| match s {
                ArraySuperType::Local(id) => id.index,
                ArraySuperType::Type(t) => external(ext, n, t.canonical_id()),
            });
            let body = CompositeBody::Array(field_ir(field, n, ext));
            (*finality, st, body, AggKind::Array)
        }
        MemberDef::Func {
            finality,
            supertype,
            params,
            results,
        } => {
            let st = supertype.as_ref().map(|s| match s {
                FuncSuperType::Local(id) => id.index,
                FuncSuperType::Type(t) => external(ext, n, t.canonical_id()),
            });
            let body = CompositeBody::Func {
                params: params.iter().map(|v| val_ir(v, n, ext)).collect(),
                results: results.iter().map(|v| val_ir(v, n, ext)).collect(),
            };
            (*finality, st, body, AggKind::Func)
        }
    }
}

/// Records an external (already-registered) canonical id and returns its combined index `n + j`.
fn external(ext: &mut Vec<CanonicalTypeId>, n: usize, id: CanonicalTypeId) -> u32 {
    let j = ext.len();
    ext.push(id);
    (n + j) as u32
}

fn field_ir(f: &FieldTemplate, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrField {
    let storage = match &f.element {
        StorageTypeTemplate::Type(StorageType::I8) => IrStorage::I8,
        StorageTypeTemplate::Type(StorageType::I16) => IrStorage::I16,
        StorageTypeTemplate::Type(StorageType::ValType(v)) => {
            IrStorage::Val(public_val_ir(v, n, ext))
        }
        StorageTypeTemplate::Ref { nullable, heap } => IrStorage::Val(IrVal::Ref {
            nullable: *nullable,
            heap: heap_ir(heap, n, ext),
        }),
    };
    IrField {
        mutable: matches!(f.mutability, Mutability::Var),
        storage,
    }
}

fn val_ir(v: &ValTypeTemplate, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrVal {
    match v {
        ValTypeTemplate::Type(t) => public_val_ir(t, n, ext),
        ValTypeTemplate::Ref { nullable, heap } => IrVal::Ref {
            nullable: *nullable,
            heap: heap_ir(heap, n, ext),
        },
    }
}

fn public_val_ir(v: &ValType, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrVal {
    match v {
        ValType::I32 => IrVal::I32,
        ValType::I64 => IrVal::I64,
        ValType::F32 => IrVal::F32,
        ValType::F64 => IrVal::F64,
        ValType::V128 => IrVal::V128,
        ValType::Ref(rt) => IrVal::Ref {
            nullable: rt.is_nullable(),
            heap: public_heap_ir(rt.heap_type(), n, ext),
        },
    }
}

fn heap_ir(h: &HeapTypeTemplate, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrHeap {
    match h {
        HeapTypeTemplate::Type(t) => public_heap_ir(t, n, ext),
        HeapTypeTemplate::LocalStruct(id) => IrHeap::Concrete(id.index, AggKind::Struct),
        HeapTypeTemplate::LocalArray(id) => IrHeap::Concrete(id.index, AggKind::Array),
        HeapTypeTemplate::LocalFunc(id) => IrHeap::Concrete(id.index, AggKind::Func),
    }
}

fn public_heap_ir(h: &HeapType, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrHeap {
    use HeapType as H;
    match h {
        H::ConcreteStruct(t) => {
            IrHeap::Concrete(external(ext, n, t.canonical_id()), AggKind::Struct)
        }
        H::ConcreteArray(t) => IrHeap::Concrete(external(ext, n, t.canonical_id()), AggKind::Array),
        H::ConcreteFunc(t) => IrHeap::Concrete(external(ext, n, t.canonical_id()), AggKind::Func),
        H::Func => IrHeap::Func,
        H::NoFunc => IrHeap::NoFunc,
        H::Extern => IrHeap::Extern,
        H::NoExtern => IrHeap::NoExtern,
        H::Any => IrHeap::Any,
        H::Eq => IrHeap::Eq,
        H::I31 => IrHeap::I31,
        H::Struct => IrHeap::Struct,
        H::Array => IrHeap::Array,
        H::Exn => IrHeap::Exn,
        H::NoExn => IrHeap::NoExn,
        H::None => IrHeap::None,
    }
}
