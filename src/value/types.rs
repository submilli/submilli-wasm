//! Type descriptors: `ValType`, `FuncType`, `MemoryType`, etc. (wasmtime-compatible).

use crate::engine::Engine;

/// A wasm value type.
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
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
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

/// The heap type of a reference. Abstract types only for now; concrete
/// (`ConcreteFunc`/struct/array) types arrive with the func-references and GC phases.
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
    Array,
    Exn,
    NoExn,
    None,
}

/// A function signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FuncType {
    params: Vec<ValType>,
    results: Vec<ValType>,
}

impl FuncType {
    pub fn new(
        engine: &Engine,
        params: impl IntoIterator<Item = ValType>,
        results: impl IntoIterator<Item = ValType>,
    ) -> FuncType {
        let _ = engine;
        FuncType {
            params: params.into_iter().collect(),
            results: results.into_iter().collect(),
        }
    }

    pub fn params(&self) -> impl ExactSizeIterator<Item = ValType> + '_ {
        self.params.iter().cloned()
    }

    pub fn results(&self) -> impl ExactSizeIterator<Item = ValType> + '_ {
        self.results.iter().cloned()
    }
}

/// A linear memory type (limits in 64 KiB pages).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

/// A global type: content type plus mutability.
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

/// A table type: element reference type plus limits.
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
