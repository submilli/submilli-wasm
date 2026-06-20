//! Engine-canonical GC type identity.
//!
//! Two representations live here:
//! - The **module IR** (this file): a serializable, module-relative type form used inside a
//!   `Module` (so the compiled artifact serializes without engine-bound handles). Concrete
//!   references are `u32` indices (module-relative until interned, then rewritten to canonical).
//! - The engine [`TypeRegistry`] (in [`registry`]): hash-cons rec groups → canonical type ids
//!   and **materializes** the public handle types (`FuncType`/`StructType`/`ArrayType`).

mod keys;
mod layout;
mod registry;

pub(crate) use layout::{Layout, RefKind, ScalarKind, Slot};
pub(crate) use registry::TypeRegistry;

use crate::engine::Engine;
use crate::value::{
    ArrayType, Finality, FuncType, GlobalType, HeapType, Mutability, RefType, StructType,
    TableType, ValType,
};

/// An engine-canonical type id: an index into the registry's per-type arena, and the identity
/// every runtime check compares on. **Distinct from a module-relative (decoder-local) type
/// index** — the two must never be interchanged (CVE-2024-12053). Opaque crate-wide;
/// constructed only inside `canon`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct CanonicalTypeId(u32);

/// An interned rec-group id: an index into the registry's per-group arena. A `Module` (and each
/// host type) holds these and hands them back to release the groups on drop.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct GroupId(u32);

impl CanonicalTypeId {
    pub(crate) fn new(raw: u32) -> Self {
        CanonicalTypeId(raw)
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

impl GroupId {
    fn new(raw: u32) -> Self {
        GroupId(raw)
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Which of the three GC hierarchies a concrete (defined) type belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum AggKind {
    Func,
    Struct,
    Array,
}

/// A heap type in the module IR. Abstract hierarchies + a concrete reference (a type index +
/// kind: module-relative on decode, rewritten to an engine-canonical id by `intern`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum IrHeap {
    Func,
    NoFunc,
    Extern,
    NoExtern,
    Any,
    Eq,
    I31,
    Struct,
    Array,
    Exn,
    NoExn,
    None,
    Concrete(u32, AggKind),
}

/// A value type in the module IR.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum IrVal {
    I32,
    I64,
    F32,
    F64,
    V128,
    Ref { nullable: bool, heap: IrHeap },
}

/// A storage type (struct field / array element) in the module IR.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum IrStorage {
    I8,
    I16,
    Val(IrVal),
}

/// A struct field / array element in the module IR.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct IrField {
    pub mutable: bool,
    pub storage: IrStorage,
}

/// A composite type body in the module IR.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum CompositeBody {
    Func {
        params: Vec<IrVal>,
        results: Vec<IrVal>,
    },
    Struct(Vec<IrField>),
    Array(IrField),
}

/// One module-relative type definition (a rec-group member).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModuleType {
    /// Module-local rec-group id; members of one rec group share it (and are contiguous).
    pub group: u32,
    pub finality: Finality,
    /// Module-relative type index of the declared supertype, if any.
    pub supertype: Option<u32>,
    pub body: CompositeBody,
}

/// A reference type in the module IR.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct IrRef {
    pub nullable: bool,
    pub heap: IrHeap,
}

/// A table type in the module IR.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct IrTableType {
    pub element: IrRef,
    pub min: u32,
    pub max: Option<u32>,
}

/// A global type in the module IR.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct IrGlobalType {
    pub content: IrVal,
    pub mutable: bool,
}

impl ModuleType {
    pub(crate) fn kind(&self) -> AggKind {
        match self.body {
            CompositeBody::Func { .. } => AggKind::Func,
            CompositeBody::Struct(_) => AggKind::Struct,
            CompositeBody::Array(_) => AggKind::Array,
        }
    }

    /// The (params, results) of a func type — panics on struct/array (callers gate by kind;
    /// validation guarantees func indices name func types).
    pub(crate) fn func_sig(&self) -> (&[IrVal], &[IrVal]) {
        match &self.body {
            CompositeBody::Func { params, results } => (params, results),
            _ => unreachable!("not a function type"),
        }
    }
}

// --- IR → public boundary materialization (translating module-relative refs to canonical) ---

/// Materializes an IR value type to the public boundary type, mapping concrete references from
/// module-relative indices to engine-canonical descriptor handles via `type_ids`.
pub(crate) fn materialize_val(engine: &Engine, type_ids: &[CanonicalTypeId], v: &IrVal) -> ValType {
    match v {
        IrVal::I32 => ValType::I32,
        IrVal::I64 => ValType::I64,
        IrVal::F32 => ValType::F32,
        IrVal::F64 => ValType::F64,
        IrVal::V128 => ValType::V128,
        IrVal::Ref { nullable, heap } => ValType::Ref(RefType::new(
            *nullable,
            materialize_heap(engine, type_ids, heap),
        )),
    }
}

pub(crate) fn materialize_heap(
    engine: &Engine,
    type_ids: &[CanonicalTypeId],
    h: &IrHeap,
) -> HeapType {
    match h {
        IrHeap::Func => HeapType::Func,
        IrHeap::NoFunc => HeapType::NoFunc,
        IrHeap::Extern => HeapType::Extern,
        IrHeap::NoExtern => HeapType::NoExtern,
        IrHeap::Any => HeapType::Any,
        IrHeap::Eq => HeapType::Eq,
        IrHeap::I31 => HeapType::I31,
        IrHeap::Struct => HeapType::Struct,
        IrHeap::Array => HeapType::Array,
        IrHeap::Exn => HeapType::Exn,
        IrHeap::NoExn => HeapType::NoExn,
        IrHeap::None => HeapType::None,
        IrHeap::Concrete(idx, kind) => {
            let id = type_ids[*idx as usize];
            match kind {
                AggKind::Func => HeapType::ConcreteFunc(FuncType::from_id(engine, id)),
                AggKind::Struct => HeapType::ConcreteStruct(StructType::from_id(engine, id)),
                AggKind::Array => HeapType::ConcreteArray(ArrayType::from_id(engine, id)),
            }
        }
    }
}

pub(crate) fn materialize_ref(engine: &Engine, type_ids: &[CanonicalTypeId], r: &IrRef) -> RefType {
    RefType::new(r.nullable, materialize_heap(engine, type_ids, &r.heap))
}

pub(crate) fn materialize_global(
    engine: &Engine,
    type_ids: &[CanonicalTypeId],
    g: &IrGlobalType,
) -> GlobalType {
    let mutability = if g.mutable {
        Mutability::Var
    } else {
        Mutability::Const
    };
    GlobalType::new(materialize_val(engine, type_ids, &g.content), mutability)
}

pub(crate) fn materialize_table(
    engine: &Engine,
    type_ids: &[CanonicalTypeId],
    t: &IrTableType,
) -> TableType {
    TableType::new(materialize_ref(engine, type_ids, &t.element), t.min, t.max)
}

/// Wraps a func type's canonical id as a public handle (for `func_type` reflection).
pub(crate) fn func_handle(engine: &Engine, canonical_id: CanonicalTypeId) -> FuncType {
    FuncType::from_id(engine, canonical_id)
}
