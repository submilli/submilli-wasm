//! Type descriptors: `ValType`, `FuncType`, `MemoryType`, etc. (wasmtime-compatible).

use crate::canon::CanonicalTypeId;
use crate::engine::Engine;
use crate::value::gc_type::{ArrayType, StructType};

/// A wasm value type. (Public/boundary type — the serializable internal form is `canon::IrVal`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
    V128,
    Ref(RefType),
}

/// Mutability of a global.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Mutability {
    Const,
    Var,
}

/// A reference type: nullability plus a heap type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RefType {
    nullable: bool,
    heap: HeapType,
}

impl RefType {
    pub fn new(is_nullable: bool, heap_type: HeapType) -> RefType {
        RefType {
            nullable: is_nullable,
            heap: heap_type,
        }
    }

    pub fn is_nullable(&self) -> bool {
        self.nullable
    }

    pub fn heap_type(&self) -> &HeapType {
        &self.heap
    }
}

/// The heap type of a reference: the abstract hierarchies (func/extern/any + bottoms) plus
/// concrete (defined) types, each carrying an engine-interned descriptor handle (`FuncType`/
/// `StructType`/`ArrayType`). Matches `wasmtime::HeapType`. (Boundary type — the serializable
/// internal form is `canon::IrHeap`.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HeapType {
    Func,
    NoFunc,
    Extern,
    NoExtern,
    Any,
    Eq,
    I31,
    Struct,
    /// A concrete struct type.
    ConcreteStruct(StructType),
    Array,
    /// A concrete array type.
    ConcreteArray(ArrayType),
    Exn,
    NoExn,
    None,
    /// A concrete function type.
    ConcreteFunc(FuncType),
}

impl HeapType {
    /// Is `self` a subtype of `other` in the **abstract** lattice (the three disjoint
    /// hierarchies + bottoms), plus concrete-type placement by hierarchy and id equality
    /// (`self == other`). Concrete-to-concrete *declared* subtyping (the supertype chain) is
    /// resolved separately against the type registry (`Engine::is_subtype`).
    pub(crate) fn matches(&self, other: &HeapType) -> bool {
        use HeapType as H;
        if self == other {
            return true;
        }
        matches!(
            (self, other),
            (H::NoFunc, H::Func | H::ConcreteFunc(_))
                | (H::ConcreteFunc(_), H::Func)
                | (H::NoExtern, H::Extern)
                | (H::I31 | H::Struct | H::Array | H::Eq, H::Any)
                | (H::I31 | H::Struct | H::Array, H::Eq)
                | (H::ConcreteStruct(_), H::Struct | H::Eq | H::Any)
                | (H::ConcreteArray(_), H::Array | H::Eq | H::Any)
                | (
                    H::None,
                    H::Any
                        | H::Eq
                        | H::I31
                        | H::Struct
                        | H::Array
                        | H::ConcreteStruct(_)
                        | H::ConcreteArray(_)
                )
        )
    }
}

impl RefType {
    /// Reference subtyping: heap-type subtyping, and a non-nullable ref is a subtype of a
    /// nullable one (but not vice-versa).
    pub(crate) fn matches(&self, other: &RefType) -> bool {
        (!self.nullable || other.nullable) && self.heap.matches(&other.heap)
    }
}

impl ValType {
    /// Value subtyping: references by [`RefType::matches`], everything else by equality.
    pub(crate) fn matches(&self, other: &ValType) -> bool {
        match (self, other) {
            (ValType::Ref(a), ValType::Ref(b)) => a.matches(b),
            _ => self == other,
        }
    }
}

/// A function signature — an engine-interned handle (identity by canonical type id; the
/// structure is materialized from the engine registry). Matches `wasmtime::FuncType`.
#[derive(Clone)]
pub struct FuncType {
    engine: Engine,
    id: CanonicalTypeId,
}

impl FuncType {
    pub fn new(
        engine: &Engine,
        params: impl IntoIterator<Item = ValType>,
        results: impl IntoIterator<Item = ValType>,
    ) -> FuncType {
        let params: Vec<ValType> = params.into_iter().collect();
        let results: Vec<ValType> = results.into_iter().collect();
        let id = engine.intern_func_type(&params, &results);
        FuncType {
            engine: engine.clone(),
            id,
        }
    }

    /// Wraps an already-interned canonical id (internal — used by the registry/module boundary).
    pub(crate) fn from_id(engine: &Engine, id: CanonicalTypeId) -> FuncType {
        FuncType {
            engine: engine.clone(),
            id,
        }
    }

    /// The engine-canonical type id (internal identity).
    pub(crate) fn canonical_id(&self) -> CanonicalTypeId {
        self.id
    }

    pub fn params(&self) -> impl ExactSizeIterator<Item = ValType> {
        self.engine.func_sig(self.id).0.into_iter()
    }

    pub fn results(&self) -> impl ExactSizeIterator<Item = ValType> {
        self.engine.func_sig(self.id).1.into_iter()
    }
}

impl PartialEq for FuncType {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for FuncType {}

impl core::hash::Hash for FuncType {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl core::fmt::Debug for FuncType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FuncType")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// A linear memory type (limits in 64 KiB pages).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MemoryType {
    minimum: u64,
    maximum: Option<u64>,
    memory64: bool,
}

impl MemoryType {
    pub fn new(minimum: u32, maximum: Option<u32>) -> MemoryType {
        MemoryType {
            minimum: u64::from(minimum),
            maximum: maximum.map(u64::from),
            memory64: false,
        }
    }

    pub fn new64(minimum: u64, maximum: Option<u64>) -> MemoryType {
        MemoryType {
            minimum,
            maximum,
            memory64: true,
        }
    }

    pub fn minimum(&self) -> u64 {
        self.minimum
    }

    pub fn maximum(&self) -> Option<u64> {
        self.maximum
    }

    pub fn is_64(&self) -> bool {
        self.memory64
    }
}

/// A global type: content type plus mutability. (Boundary type; internal storage is IR.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GlobalType {
    content: ValType,
    mutability: Mutability,
}

impl GlobalType {
    pub fn new(content: ValType, mutability: Mutability) -> GlobalType {
        GlobalType {
            content,
            mutability,
        }
    }

    pub fn content(&self) -> &ValType {
        &self.content
    }

    pub fn mutability(&self) -> Mutability {
        self.mutability
    }
}

/// A table type: element reference type plus limits. (Boundary type; internal storage is IR.)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TableType {
    element: RefType,
    minimum: u64,
    maximum: Option<u64>,
}

impl TableType {
    pub fn new(element: RefType, min: u32, max: Option<u32>) -> TableType {
        TableType {
            element,
            minimum: u64::from(min),
            maximum: max.map(u64::from),
        }
    }

    pub fn element(&self) -> &RefType {
        &self.element
    }

    pub fn minimum(&self) -> u64 {
        self.minimum
    }

    pub fn maximum(&self) -> Option<u64> {
        self.maximum
    }
}

/// The type of an importable/exportable external item.
#[derive(Clone, Debug)]
pub enum ExternType {
    Func(FuncType),
    Memory(MemoryType),
    Global(GlobalType),
    Table(TableType),
}

/// Metadata about a module import.
#[derive(Clone, Debug)]
pub struct ImportType<'module> {
    module: &'module str,
    name: &'module str,
    ty: ExternType,
}

impl<'module> ImportType<'module> {
    pub(crate) fn new(module: &'module str, name: &'module str, ty: ExternType) -> Self {
        ImportType { module, name, ty }
    }

    pub fn module(&self) -> &'module str {
        self.module
    }

    pub fn name(&self) -> &'module str {
        self.name
    }

    pub fn ty(&self) -> ExternType {
        self.ty.clone()
    }
}

/// Metadata about a module export.
#[derive(Clone, Debug)]
pub struct ExportType<'module> {
    name: &'module str,
    ty: ExternType,
}

impl<'module> ExportType<'module> {
    pub(crate) fn new(name: &'module str, ty: ExternType) -> Self {
        ExportType { name, ty }
    }

    pub fn name(&self) -> &'module str {
        self.name
    }

    pub fn ty(&self) -> ExternType {
        self.ty.clone()
    }
}
