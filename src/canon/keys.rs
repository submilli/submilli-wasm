//! The canonical, position-independent type form: the hash-cons key (`CType`/`CBody`) and the
//! conversions into it from the module IR (`*_key`) and from public host-built types (`*_body`),
//! plus the small abstract-heap-type codecs. The registry (`super::registry`) stores these and
//! materializes public handle types back out.

use super::{AggKind, CanonicalTypeId, CompositeBody, IrField, IrHeap, IrStorage, IrVal};
use crate::value::{FieldType, Finality, HeapType, Mutability, StorageType, ValType};

/// A reference inside a canonical group: a position in the same group, or an absolute canonical
/// id. The key uses both (`Rel` for intra-group); a *stored* body resolves `Rel` → `Canon`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum CanonRef {
    /// A position within the same rec group (intra-group, hash-cons key only).
    Rel(u32),
    /// An absolute engine-canonical type id.
    Canon(CanonicalTypeId),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum CHeap {
    Abs(u8),
    Concrete(AggKind, CanonRef),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum CVal {
    Num(u8),
    Ref(bool, CHeap),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum CStore {
    Packed(u8),
    Val(CVal),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct CField {
    pub(super) mutable: bool,
    pub(super) storage: CStore,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum CBody {
    Func(Vec<CVal>, Vec<CVal>),
    Struct(Vec<CField>),
    Array(CField),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct CType {
    pub(super) finality: Finality,
    pub(super) supertype: Option<CanonRef>,
    pub(super) body: CBody,
}

/// A canonicalized rec group — the hash-cons key.
pub(super) type CGroup = Vec<CType>;

// --- module IR → canonical key (refs relative until resolved) ---

pub(super) fn body_key(b: &CompositeBody, cref: &impl Fn(u32) -> CanonRef) -> CBody {
    match b {
        CompositeBody::Func { params, results } => CBody::Func(
            params.iter().map(|v| val_key(v, cref)).collect(),
            results.iter().map(|v| val_key(v, cref)).collect(),
        ),
        CompositeBody::Struct(fields) => {
            CBody::Struct(fields.iter().map(|f| field_key(f, cref)).collect())
        }
        CompositeBody::Array(f) => CBody::Array(field_key(f, cref)),
    }
}

fn field_key(f: &IrField, cref: &impl Fn(u32) -> CanonRef) -> CField {
    let storage = match &f.storage {
        IrStorage::I8 => CStore::Packed(0),
        IrStorage::I16 => CStore::Packed(1),
        IrStorage::Val(v) => CStore::Val(val_key(v, cref)),
    };
    CField {
        mutable: f.mutable,
        storage,
    }
}

fn val_key(v: &IrVal, cref: &impl Fn(u32) -> CanonRef) -> CVal {
    match v {
        IrVal::I32 => CVal::Num(0),
        IrVal::I64 => CVal::Num(1),
        IrVal::F32 => CVal::Num(2),
        IrVal::F64 => CVal::Num(3),
        IrVal::V128 => CVal::Num(4),
        IrVal::Ref { nullable, heap } => CVal::Ref(*nullable, heap_key(heap, cref)),
    }
}

fn heap_key(h: &IrHeap, cref: &impl Fn(u32) -> CanonRef) -> CHeap {
    match h {
        IrHeap::Concrete(id, kind) => CHeap::Concrete(*kind, cref(*id)),
        other => CHeap::Abs(ir_abs_code(other)),
    }
}

// --- public host-built types → canonical bodies (for `intern_one`) ---

pub(super) fn func_body(params: &[ValType], results: &[ValType]) -> CBody {
    CBody::Func(
        params.iter().map(pub_val).collect(),
        results.iter().map(pub_val).collect(),
    )
}

pub(super) fn struct_body(fields: &[FieldType]) -> CBody {
    CBody::Struct(fields.iter().map(pub_field).collect())
}

pub(super) fn array_body(field: &FieldType) -> CBody {
    CBody::Array(pub_field(field))
}

fn pub_val(v: &ValType) -> CVal {
    match v {
        ValType::I32 => CVal::Num(0),
        ValType::I64 => CVal::Num(1),
        ValType::F32 => CVal::Num(2),
        ValType::F64 => CVal::Num(3),
        ValType::V128 => CVal::Num(4),
        ValType::Ref(rt) => CVal::Ref(rt.is_nullable(), pub_heap(rt.heap_type())),
    }
}

fn pub_heap(h: &HeapType) -> CHeap {
    let concrete = |k: AggKind, id: CanonicalTypeId| CHeap::Concrete(k, CanonRef::Canon(id));
    match h {
        HeapType::ConcreteFunc(t) => concrete(AggKind::Func, t.canonical_id()),
        HeapType::ConcreteStruct(t) => concrete(AggKind::Struct, t.canonical_id()),
        HeapType::ConcreteArray(t) => concrete(AggKind::Array, t.canonical_id()),
        other => CHeap::Abs(abs_code(other)),
    }
}

fn pub_field(f: &FieldType) -> CField {
    let storage = match f.element_type() {
        StorageType::I8 => CStore::Packed(0),
        StorageType::I16 => CStore::Packed(1),
        StorageType::ValType(v) => CStore::Val(pub_val(v)),
    };
    CField {
        mutable: matches!(f.mutability(), Mutability::Var),
        storage,
    }
}

// --- resolution (relative refs → absolute canonical ids) and kind ---

pub(super) fn resolve(r: &CanonRef, members: &[CanonicalTypeId]) -> CanonicalTypeId {
    match r {
        CanonRef::Rel(p) => members[*p as usize],
        CanonRef::Canon(c) => *c,
    }
}

pub(super) fn resolve_body(b: &CBody, members: &[CanonicalTypeId]) -> CBody {
    let rv = |v: &CVal| match v {
        CVal::Ref(n, CHeap::Concrete(k, r)) => CVal::Ref(
            *n,
            CHeap::Concrete(*k, CanonRef::Canon(resolve(r, members))),
        ),
        other => other.clone(),
    };
    let rf = |f: &CField| CField {
        mutable: f.mutable,
        storage: match &f.storage {
            CStore::Val(v) => CStore::Val(rv(v)),
            other @ CStore::Packed(_) => other.clone(),
        },
    };
    match b {
        CBody::Func(p, r) => CBody::Func(p.iter().map(rv).collect(), r.iter().map(rv).collect()),
        CBody::Struct(fs) => CBody::Struct(fs.iter().map(rf).collect()),
        CBody::Array(f) => CBody::Array(rf(f)),
    }
}

pub(super) fn body_kind(b: &CBody) -> AggKind {
    match b {
        CBody::Func(..) => AggKind::Func,
        CBody::Struct(_) => AggKind::Struct,
        CBody::Array(_) => AggKind::Array,
    }
}

// --- abstract heap-type codecs (compact u8 ↔ public/IR variant) ---

pub(super) fn num_decode(c: u8) -> ValType {
    match c {
        0 => ValType::I32,
        1 => ValType::I64,
        2 => ValType::F32,
        3 => ValType::F64,
        _ => ValType::V128,
    }
}

fn ir_abs_code(h: &IrHeap) -> u8 {
    match h {
        IrHeap::Func => 0,
        IrHeap::NoFunc => 1,
        IrHeap::Extern => 2,
        IrHeap::NoExtern => 3,
        IrHeap::Any => 4,
        IrHeap::Eq => 5,
        IrHeap::I31 => 6,
        IrHeap::Struct => 7,
        IrHeap::Array => 8,
        IrHeap::Exn => 9,
        IrHeap::NoExn => 10,
        IrHeap::None => 11,
        IrHeap::Concrete(..) => unreachable!("concrete handled separately"),
    }
}

fn abs_code(h: &HeapType) -> u8 {
    use HeapType as H;
    match h {
        H::Func => 0,
        H::NoFunc => 1,
        H::Extern => 2,
        H::NoExtern => 3,
        H::Any => 4,
        H::Eq => 5,
        H::I31 => 6,
        H::Struct => 7,
        H::Array => 8,
        H::Exn => 9,
        H::NoExn => 10,
        _ => 11,
    }
}

pub(super) fn abs_decode(c: u8) -> HeapType {
    use HeapType as H;
    match c {
        0 => H::Func,
        1 => H::NoFunc,
        2 => H::Extern,
        3 => H::NoExtern,
        4 => H::Any,
        5 => H::Eq,
        6 => H::I31,
        7 => H::Struct,
        8 => H::Array,
        9 => H::Exn,
        10 => H::NoExn,
        _ => H::None,
    }
}
