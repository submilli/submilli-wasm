//! Lowering: `RecGroupBuilder` member definitions → module IR. Sibling forward references become
//! relative concrete refs (`IrHeap::Concrete(index, _)`, `index < n`); already-registered types
//! are appended to an `externals` table and referenced as `IrHeap::Concrete(n + j, _)`.

use super::{FieldDef, MemberDef, SuperDef, ValDef};
use crate::canon::{AggKind, CanonicalTypeId, CompositeBody, IrField, IrHeap, IrStorage, IrVal};
use crate::value::{FieldType, Finality, HeapType, Mutability, StorageType, ValType};

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
            let st = supertype.as_ref().map(|s| super_index(s, n, ext));
            let body = CompositeBody::Struct(fields.iter().map(|f| field_ir(f, n, ext)).collect());
            (*finality, st, body, AggKind::Struct)
        }
        MemberDef::Array {
            finality,
            supertype,
            element,
        } => {
            let st = supertype.as_ref().map(|s| super_index(s, n, ext));
            let body = CompositeBody::Array(field_ir(element, n, ext));
            (*finality, st, body, AggKind::Array)
        }
        MemberDef::Func {
            finality,
            supertype,
            params,
            results,
        } => {
            let st = supertype.as_ref().map(|s| super_index(s, n, ext));
            let body = CompositeBody::Func {
                params: params.iter().map(|v| val_ir(v, n, ext)).collect(),
                results: results.iter().map(|v| val_ir(v, n, ext)).collect(),
            };
            (*finality, st, body, AggKind::Func)
        }
    }
}

fn super_index(s: &SuperDef, n: usize, ext: &mut Vec<CanonicalTypeId>) -> u32 {
    match s {
        SuperDef::Forward(index) => *index,
        SuperDef::Struct(t) => external(ext, n, t.canonical_id()),
        SuperDef::Array(t) => external(ext, n, t.canonical_id()),
        SuperDef::Func(t) => external(ext, n, t.canonical_id()),
    }
}

/// Records an external (already-registered) canonical id and returns its combined index `n + j`.
fn external(ext: &mut Vec<CanonicalTypeId>, n: usize, id: CanonicalTypeId) -> u32 {
    let j = ext.len();
    ext.push(id);
    (n + j) as u32
}

fn field_ir(f: &FieldDef, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrField {
    match f {
        FieldDef::Registered(ty) => registered_field_ir(ty, n, ext),
        FieldDef::Forward {
            target,
            mutable,
            nullable,
        } => IrField {
            mutable: *mutable,
            storage: IrStorage::Val(IrVal::Ref {
                nullable: *nullable,
                heap: IrHeap::Concrete(target.index, target.kind),
            }),
        },
    }
}

fn registered_field_ir(f: &FieldType, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrField {
    let storage = match f.element_type() {
        StorageType::I8 => IrStorage::I8,
        StorageType::I16 => IrStorage::I16,
        StorageType::ValType(v) => IrStorage::Val(registered_val_ir(v, n, ext)),
    };
    IrField {
        mutable: matches!(f.mutability(), Mutability::Var),
        storage,
    }
}

fn val_ir(v: &ValDef, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrVal {
    match v {
        ValDef::Registered(t) => registered_val_ir(t, n, ext),
        ValDef::Forward { target, nullable } => IrVal::Ref {
            nullable: *nullable,
            heap: IrHeap::Concrete(target.index, target.kind),
        },
    }
}

fn registered_val_ir(v: &ValType, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrVal {
    match v {
        ValType::I32 => IrVal::I32,
        ValType::I64 => IrVal::I64,
        ValType::F32 => IrVal::F32,
        ValType::F64 => IrVal::F64,
        ValType::V128 => IrVal::V128,
        ValType::Ref(rt) => IrVal::Ref {
            nullable: rt.is_nullable(),
            heap: registered_heap_ir(rt.heap_type(), n, ext),
        },
    }
}

fn registered_heap_ir(h: &HeapType, n: usize, ext: &mut Vec<CanonicalTypeId>) -> IrHeap {
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
