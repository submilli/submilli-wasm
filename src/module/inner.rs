//! The compiled, immutable module representation (`ModuleInner`) and its
//! descriptor / segment / constant-expression types. Built by [`super::parse`],
//! shared behind an `Arc` inside [`Module`](super::Module), and consumed by
//! `instance::init` at instantiation time.

use std::sync::Arc;

use crate::canon::{
    self, AggKind, CanonicalTypeId, GroupId, IrGlobalType, IrHeap, IrTableType, Layout, ModuleType,
};
use crate::engine::Engine;
use crate::module::op::CompiledFunc;
use crate::value::{ExternType, FuncType, GlobalType, MemoryType, TableType, TagType};

/// A module decoded and compiled to internal bytecode. Immutable and shareable.
///
/// The type table (`types`) holds module-relative type defs (the serializable form). After
/// parse/decode, [`intern`](Self::intern) registers the rec groups in the engine — filling
/// `type_ids` (module type index → engine-canonical id) and `group_handles` (for release on
/// drop). Runtime type identity compares the canonical ids; the structure stays for func
/// signatures and (later) field layout.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModuleInner {
    pub types: Vec<ModuleType>,
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
    /// *Defined* tags (imported ones live in `imports`). Each references a func type index.
    pub tags: Vec<TagDef>,
    pub num_imported_tags: u32,
    pub datas: Vec<DataSegment>,
    pub elems: Vec<ElemSegment>,
    pub start: Option<u32>,
    /// Module type index → engine-canonical id (engine-specific; recomputed by `intern`).
    #[serde(skip)]
    pub type_ids: Vec<CanonicalTypeId>,
    /// Registered canonical group ids, released on drop (engine-specific).
    #[serde(skip)]
    pub group_handles: Vec<GroupId>,
    /// Per-module-type GC byte layout (`None` for function types). Rebuilt by `intern` from the
    /// IR; the interpreter reads it lock-free for `struct.*`/`array.*` field/element access.
    #[serde(skip)]
    pub layouts: Vec<Option<Layout>>,
    /// The owning engine (for releasing `group_handles` on drop). `None` until `intern`.
    #[serde(skip)]
    pub engine: Option<Engine>,
    /// Retained DWARF/`name` custom sections + lazily-built line index (#29a). Empty unless debug
    /// retention was on at parse. Module-intrinsic (not engine-specific), so it serializes with the
    /// artifact — backtraces survive `serialize`/`deserialize`.
    pub debug: crate::module::debug::DebugSections,
}

impl ModuleInner {
    /// Registers this module's rec groups in `engine`'s canonical registry, baking the
    /// canonical ids. Called after parse (`Module::new`) and after decode (`deserialize`).
    pub(crate) fn intern(&mut self, engine: &Engine) {
        let (type_ids, group_handles) = engine.intern_types(&self.types);
        self.type_ids = type_ids;
        self.group_handles = group_handles;
        self.layouts = self
            .types
            .iter()
            .map(|t| Layout::from_body(&t.body))
            .collect();
        self.engine = Some(engine.clone());
    }

    /// The engine-canonical id of a module type index.
    pub(crate) fn canonical_type_id(&self, type_index: u32) -> CanonicalTypeId {
        self.type_ids[type_index as usize]
    }

    /// The GC byte layout of an aggregate (struct/array) module type index.
    pub(crate) fn layout(&self, type_index: u32) -> &Layout {
        self.layouts[type_index as usize]
            .as_ref()
            .expect("aggregate type has a layout")
    }

    /// The owning engine (set by `intern`).
    pub(crate) fn engine(&self) -> &Engine {
        self.engine.as_ref().expect("module not interned")
    }

    /// The per-type-index kind table (for resolving concrete references during conversion).
    pub(crate) fn type_kinds(&self) -> Vec<AggKind> {
        self.types.iter().map(ModuleType::kind).collect()
    }
}

impl Drop for ModuleInner {
    fn drop(&mut self) {
        if let Some(engine) = &self.engine {
            engine.release_types(&self.group_handles);
        }
    }
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
    Table(IrTableType),
    Memory(MemoryType),
    Global(IrGlobalType),
    /// An imported tag, carrying its referenced func-type index (module-relative).
    Tag(u32),
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
    Tag(u32),
}

/// A defined table: its type plus an optional constant initializer expression
/// (function-references; `None` means default-null per the reference-types form).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct TableDef {
    pub ty: IrTableType,
    pub init: Option<ConstExpr>,
}

/// A defined global: its type plus the constant initializer expression.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct GlobalDef {
    pub ty: IrGlobalType,
    pub init: ConstExpr,
}

/// A defined tag: the module-relative index of the func type it references.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct TagDef {
    pub type_index: u32,
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

/// An owned constant expression: the operator sequence (decoupled from the input bytes),
/// evaluated by a small stack machine at instantiation. Only the constant forms the validator
/// admits — including the GC aggregate constructors, which allocate at init time.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ConstExpr(pub Box<[ConstOp]>);

/// One operator of a [`ConstExpr`] (postfix; aggregate ops pop their operands from the
/// const-eval stack).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum ConstOp {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    RefNull(IrHeap),
    RefFunc(u32),
    /// `global.get` of an imported or prior-defined immutable global (guaranteed by validation).
    GlobalGet(u32),
    /// Extended-const arithmetic (#40); each pops two operands off the const-eval stack.
    I32Add,
    I32Sub,
    I32Mul,
    I64Add,
    I64Sub,
    I64Mul,
    RefI31,
    StructNew(u32),
    StructNewDefault(u32),
    ArrayNew(u32),
    ArrayNewDefault(u32),
    ArrayNewFixed {
        ty: u32,
        n: u32,
    },
    ArrayNewData {
        ty: u32,
        data: u32,
    },
    ArrayNewElem {
        ty: u32,
        elem: u32,
    },
    AnyConvertExtern,
    ExternConvertAny,
}

impl ModuleInner {
    /// The compiled body for a *defined* module-space function index.
    pub(crate) fn compiled(&self, module_func_idx: u32) -> Arc<CompiledFunc> {
        self.functions[(module_func_idx - self.num_imported_funcs) as usize].clone()
    }

    /// The signature handle of any module-space function index (imported or defined).
    pub(crate) fn func_type(&self, module_func_idx: u32) -> FuncType {
        canon::func_handle(
            self.engine(),
            self.canonical_type_id(self.func_types[module_func_idx as usize]),
        )
    }

    /// The external type of an import (for [`Module::imports`](super::Module::imports)).
    pub(crate) fn import_extern_type(&self, kind: &ImportKind) -> ExternType {
        let (engine, ids) = (self.engine(), &self.type_ids);
        match kind {
            ImportKind::Func(t) => {
                ExternType::Func(canon::func_handle(engine, self.canonical_type_id(*t)))
            }
            ImportKind::Table(tt) => ExternType::Table(canon::materialize_table(engine, ids, tt)),
            ImportKind::Memory(mt) => ExternType::Memory(mt.clone()),
            ImportKind::Global(gt) => {
                ExternType::Global(canon::materialize_global(engine, ids, gt))
            }
            ImportKind::Tag(t) => ExternType::Tag(TagType::new(canon::func_handle(
                engine,
                self.canonical_type_id(*t),
            ))),
        }
    }

    /// The external type of an export (for [`Module::exports`](super::Module::exports)),
    /// resolving the module-space index across imported + defined entities.
    pub(crate) fn export_extern_type(&self, kind: ExportKind) -> ExternType {
        match kind {
            ExportKind::Func(i) => ExternType::Func(self.func_type(i)),
            ExportKind::Table(i) => ExternType::Table(self.nth_table(i)),
            ExportKind::Memory(i) => ExternType::Memory(self.nth_memory(i)),
            ExportKind::Global(i) => ExternType::Global(self.nth_global(i)),
            ExportKind::Tag(i) => ExternType::Tag(self.nth_tag(i)),
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
        let (engine, ids) = (self.engine(), &self.type_ids);
        let mut n = 0;
        for imp in &self.imports {
            if let ImportKind::Table(tt) = &imp.kind {
                if n == idx {
                    return canon::materialize_table(engine, ids, tt);
                }
                n += 1;
            }
        }
        canon::materialize_table(engine, ids, &self.tables[(idx - n) as usize].ty)
    }

    fn nth_global(&self, idx: u32) -> GlobalType {
        let (engine, ids) = (self.engine(), &self.type_ids);
        let mut n = 0;
        for imp in &self.imports {
            if let ImportKind::Global(gt) = &imp.kind {
                if n == idx {
                    return canon::materialize_global(engine, ids, gt);
                }
                n += 1;
            }
        }
        canon::materialize_global(engine, ids, &self.globals[(idx - n) as usize].ty)
    }

    fn nth_tag(&self, idx: u32) -> TagType {
        let mut n = 0;
        for imp in &self.imports {
            if let ImportKind::Tag(t) = &imp.kind {
                if n == idx {
                    return self.tag_type(*t);
                }
                n += 1;
            }
        }
        self.tag_type(self.tags[(idx - n) as usize].type_index)
    }

    /// The [`TagType`] for a tag referencing the given module-relative func-type index.
    pub(crate) fn tag_type(&self, type_index: u32) -> TagType {
        TagType::new(canon::func_handle(
            self.engine(),
            self.canonical_type_id(type_index),
        ))
    }
}
