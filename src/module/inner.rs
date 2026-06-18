//! The compiled, immutable module representation (`ModuleInner`) and its
//! descriptor / segment / constant-expression types. Built by [`super::parse`],
//! shared behind an `Arc` inside [`Module`](super::Module), and consumed by
//! `instance::init` at instantiation time.

use std::sync::Arc;

use crate::module::op::CompiledFunc;
use crate::value::{ExternType, FuncType, GlobalType, MemoryType, RefType, TableType};

/// A module decoded and compiled to internal bytecode. Immutable and shareable.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModuleInner {
    pub types: Vec<FuncType>,
    /// Type index for every function — imported functions first, then defined.
    pub func_types: Vec<u32>,
    pub num_imported_funcs: u32,
    /// Compiled bodies for *defined* functions (index = module func idx
    /// minus [`num_imported_funcs`](Self::num_imported_funcs)).
    pub functions: Vec<Arc<CompiledFunc>>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    /// *Defined* memory/table/global descriptors (imported ones live in `imports`).
    pub memories: Vec<MemoryType>,
    pub tables: Vec<TableDef>,
    pub globals: Vec<GlobalDef>,
    pub datas: Vec<DataSegment>,
    pub elems: Vec<ElemSegment>,
    pub start: Option<u32>,
}

/// One module import, in declaration order.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Import {
    pub module: String,
    pub name: String,
    pub kind: ImportKind,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum ImportKind {
    Func(u32),
    Table(TableType),
    Memory(MemoryType),
    Global(GlobalType),
}

/// One module export, in declaration order. Indices are module-space.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Export {
    pub name: String,
    pub kind: ExportKind,
}

#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum ExportKind {
    Func(u32),
    Table(u32),
    Memory(u32),
    Global(u32),
}

/// A defined table: its type plus an optional constant initializer expression
/// (function-references; `None` means default-null per the reference-types form).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct TableDef {
    pub ty: TableType,
    pub init: Option<ConstExpr>,
}

/// A defined global: its type plus the constant initializer expression.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct GlobalDef {
    pub ty: GlobalType,
    pub init: ConstExpr,
}

/// A data segment. Active segments copy into a memory at instantiation; passive
/// ones are consumed by `memory.init`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct DataSegment {
    pub mode: DataMode,
    pub bytes: Box<[u8]>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum DataMode {
    Passive,
    Active { memory: u32, offset: ConstExpr },
}

/// An element segment. Active segments write into a table at instantiation;
/// passive/declared ones are stored inert (table.init/elem.drop are deferred).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ElemSegment {
    pub mode: ElemMode,
    pub items: ElemItems,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum ElemMode {
    Passive,
    Declared,
    Active { table: u32, offset: ConstExpr },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum ElemItems {
    Funcs(Box<[u32]>),
    Exprs(Box<[ConstExpr]>),
}

/// An owned constant expression, decoupled from the input bytes so it can be
/// evaluated at instantiation. Only the constant forms the validator admits.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum ConstExpr {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    RefNull(RefType),
    RefFunc(u32),
    /// `global.get` of an imported, immutable global (guaranteed by validation).
    GlobalGet(u32),
}

impl ModuleInner {
    /// The compiled body for a *defined* module-space function index.
    pub(crate) fn compiled(&self, module_func_idx: u32) -> Arc<CompiledFunc> {
        self.functions[(module_func_idx - self.num_imported_funcs) as usize].clone()
    }

    /// The signature of any module-space function index (imported or defined).
    pub(crate) fn func_type(&self, module_func_idx: u32) -> &FuncType {
        &self.types[self.func_types[module_func_idx as usize] as usize]
    }

    /// The external type of an import (for [`Module::imports`](super::Module::imports)).
    pub(crate) fn import_extern_type(&self, kind: &ImportKind) -> ExternType {
        match kind {
            ImportKind::Func(t) => ExternType::Func(self.types[*t as usize].clone()),
            ImportKind::Table(tt) => ExternType::Table(tt.clone()),
            ImportKind::Memory(mt) => ExternType::Memory(mt.clone()),
            ImportKind::Global(gt) => ExternType::Global(gt.clone()),
        }
    }

    /// The external type of an export (for [`Module::exports`](super::Module::exports)),
    /// resolving the module-space index across imported + defined entities.
    pub(crate) fn export_extern_type(&self, kind: ExportKind) -> ExternType {
        match kind {
            ExportKind::Func(i) => ExternType::Func(self.func_type(i).clone()),
            ExportKind::Table(i) => ExternType::Table(self.nth_table(i)),
            ExportKind::Memory(i) => ExternType::Memory(self.nth_memory(i)),
            ExportKind::Global(i) => ExternType::Global(self.nth_global(i)),
        }
    }

    fn nth_memory(&self, idx: u32) -> MemoryType {
        let mut n = 0;
        for imp in &self.imports {
            if let ImportKind::Memory(mt) = &imp.kind {
                if n == idx {
                    return mt.clone();
                }
                n += 1;
            }
        }
        self.memories[(idx - n) as usize].clone()
    }

    fn nth_table(&self, idx: u32) -> TableType {
        let mut n = 0;
        for imp in &self.imports {
            if let ImportKind::Table(tt) = &imp.kind {
                if n == idx {
                    return tt.clone();
                }
                n += 1;
            }
        }
        self.tables[(idx - n) as usize].ty.clone()
    }

    fn nth_global(&self, idx: u32) -> GlobalType {
        let mut n = 0;
        for imp in &self.imports {
            if let ImportKind::Global(gt) = &imp.kind {
                if n == idx {
                    return gt.clone();
                }
                n += 1;
            }
        }
        self.globals[(idx - n) as usize].ty.clone()
    }
}
