//! Decode a validated wasm binary into a [`ModuleInner`]: walk the sections,
//! convert types/segments to our owned representation, and compile each function
//! body to internal bytecode via [`translate_function`].

// Memory/table limits are validated to fit u32 for 32-bit memories/tables.
#![allow(clippy::cast_possible_truncation)]

use std::sync::Arc;

use wasmparser::{
    BinaryReaderError, DataKind, ElementItems, ElementKind, ExternalKind, FunctionBody,
    GlobalSectionReader, ImportSectionReader, Operator, Parser, Payload, TypeRef,
};

use crate::canon::{AggKind, IrGlobalType, IrHeap, IrRef, IrTableType, IrVal, ModuleType};
use crate::engine::Engine;
use crate::module::compile::{conv_valtype, translate_function, CompileCtx};
use crate::module::inner::{
    ConstExpr, ConstOp, DataMode, DataSegment, ElemItems, ElemMode, ElemSegment, Export,
    ExportKind, GlobalDef, Import, ImportKind, ModuleInner, TableDef, TagDef,
};
use crate::module::op::CompiledFunc;
use crate::value::MemoryType;
use crate::{Error, Result};

fn wp_err(e: BinaryReaderError) -> Error {
    Error::msg(e.to_string())
}

/// Decodes and compiles a (pre-validated) wasm binary into a [`ModuleInner`].
pub(crate) fn parse_module(engine: &Engine, bytes: &[u8]) -> Result<ModuleInner> {
    let mut m = empty_inner();
    let mut bodies: Vec<FunctionBody<'_>> = Vec::new();

    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(wp_err)? {
            Payload::TypeSection(r) => super::typesec::parse_types(&mut m.types, r)?,
            Payload::ImportSection(r) => parse_imports(&mut m, r)?,
            Payload::FunctionSection(r) => {
                for ty in r {
                    m.func_types.push(ty.map_err(wp_err)?);
                }
            }
            Payload::TableSection(r) => {
                let kinds = m.type_kinds();
                for t in r {
                    let t = t.map_err(wp_err)?;
                    let ty = conv_tabletype(&kinds, t.ty)?;
                    let init = match t.init {
                        wasmparser::TableInit::RefNull => None,
                        wasmparser::TableInit::Expr(e) => Some(parse_const_expr(&kinds, &e)?),
                    };
                    m.tables.push(TableDef { ty, init });
                }
            }
            Payload::MemorySection(r) => {
                for mt in r {
                    m.memories.push(conv_memtype(mt.map_err(wp_err)?));
                }
            }
            Payload::GlobalSection(r) => {
                let kinds = m.type_kinds();
                parse_globals(&kinds, &mut m.globals, r)?;
            }
            Payload::TagSection(r) => parse_tags(&mut m.tags, r)?,
            Payload::ExportSection(r) => parse_exports(&mut m.exports, r)?,
            Payload::StartSection { func, .. } => m.start = Some(func),
            Payload::ElementSection(r) => {
                let kinds = m.type_kinds();
                parse_elems(&kinds, &mut m.elems, r)?;
            }
            Payload::DataSection(r) => {
                let kinds = m.type_kinds();
                parse_datas(&kinds, &mut m.datas, r)?;
            }
            Payload::CodeSectionEntry(body) => bodies.push(body),
            _ => {}
        }
    }

    m.functions = compile_bodies(&m, &bodies)?;
    // Register the module's rec groups in the engine, baking canonical type ids.
    m.intern(engine);
    Ok(m)
}

fn empty_inner() -> ModuleInner {
    ModuleInner {
        types: Vec::new(),
        func_types: Vec::new(),
        num_imported_funcs: 0,
        functions: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        memories: Vec::new(),
        tables: Vec::new(),
        globals: Vec::new(),
        tags: Vec::new(),
        num_imported_tags: 0,
        datas: Vec::new(),
        elems: Vec::new(),
        start: None,
        type_ids: Vec::new(),
        group_handles: Vec::new(),
        layouts: Vec::new(),
        engine: None,
    }
}

fn parse_imports(m: &mut ModuleInner, reader: ImportSectionReader<'_>) -> Result<()> {
    for group in reader {
        for item in group.map_err(wp_err)? {
            let (_, imp) = item.map_err(wp_err)?;
            push_import(m, &imp)?;
        }
    }
    Ok(())
}

fn push_import(m: &mut ModuleInner, imp: &wasmparser::Import<'_>) -> Result<()> {
    let kinds = m.type_kinds();
    let kind = match imp.ty {
        TypeRef::Func(t) => {
            m.func_types.push(t);
            m.num_imported_funcs += 1;
            ImportKind::Func(t)
        }
        TypeRef::Table(tt) => ImportKind::Table(conv_tabletype(&kinds, tt)?),
        TypeRef::Memory(mt) => ImportKind::Memory(conv_memtype(mt)),
        TypeRef::Global(gt) => ImportKind::Global(conv_globaltype(&kinds, gt)?),
        TypeRef::Tag(t) => {
            m.num_imported_tags += 1;
            ImportKind::Tag(t.func_type_idx)
        }
        TypeRef::FuncExact(_) => return Err(Error::msg("unsupported import kind")),
    };
    m.imports.push(Import {
        module: imp.module.to_string(),
        name: imp.name.to_string(),
        kind,
    });
    Ok(())
}

fn parse_globals(
    kinds: &[AggKind],
    out: &mut Vec<GlobalDef>,
    reader: GlobalSectionReader<'_>,
) -> Result<()> {
    for g in reader {
        let g = g.map_err(wp_err)?;
        out.push(GlobalDef {
            ty: conv_globaltype(kinds, g.ty)?,
            init: parse_const_expr(kinds, &g.init_expr)?,
        });
    }
    Ok(())
}

fn parse_tags(out: &mut Vec<TagDef>, reader: wasmparser::TagSectionReader<'_>) -> Result<()> {
    for t in reader {
        out.push(TagDef {
            type_index: t.map_err(wp_err)?.func_type_idx,
        });
    }
    Ok(())
}

fn parse_exports(out: &mut Vec<Export>, reader: wasmparser::ExportSectionReader<'_>) -> Result<()> {
    for e in reader {
        let e = e.map_err(wp_err)?;
        let kind = match e.kind {
            ExternalKind::Func => ExportKind::Func(e.index),
            ExternalKind::Table => ExportKind::Table(e.index),
            ExternalKind::Memory => ExportKind::Memory(e.index),
            ExternalKind::Global => ExportKind::Global(e.index),
            ExternalKind::Tag => ExportKind::Tag(e.index),
            ExternalKind::FuncExact => return Err(Error::msg("unsupported export kind")),
        };
        out.push(Export {
            name: e.name.to_string(),
            kind,
        });
    }
    Ok(())
}

fn parse_elems(
    kinds: &[AggKind],
    out: &mut Vec<ElemSegment>,
    reader: wasmparser::ElementSectionReader<'_>,
) -> Result<()> {
    for e in reader {
        let e = e.map_err(wp_err)?;
        let mode = match e.kind {
            ElementKind::Passive => ElemMode::Passive,
            ElementKind::Declared => ElemMode::Declared,
            ElementKind::Active {
                table_index,
                offset_expr,
            } => ElemMode::Active {
                table: table_index.unwrap_or(0),
                offset: parse_const_expr(kinds, &offset_expr)?,
            },
        };
        out.push(ElemSegment {
            mode,
            items: conv_elem_items(kinds, e.items)?,
        });
    }
    Ok(())
}

fn conv_elem_items(kinds: &[AggKind], items: ElementItems<'_>) -> Result<ElemItems> {
    match items {
        ElementItems::Functions(r) => {
            let funcs = r
                .into_iter()
                .map(|f| f.map_err(wp_err))
                .collect::<Result<Vec<_>>>()?;
            Ok(ElemItems::Funcs(funcs.into_boxed_slice()))
        }
        ElementItems::Expressions(_, r) => {
            let exprs = r
                .into_iter()
                .map(|e| parse_const_expr(kinds, &e.map_err(wp_err)?))
                .collect::<Result<Vec<_>>>()?;
            Ok(ElemItems::Exprs(exprs.into_boxed_slice()))
        }
    }
}

fn parse_datas(
    kinds: &[AggKind],
    out: &mut Vec<DataSegment>,
    reader: wasmparser::DataSectionReader<'_>,
) -> Result<()> {
    for d in reader {
        let d = d.map_err(wp_err)?;
        let mode = match d.kind {
            DataKind::Passive => DataMode::Passive,
            DataKind::Active {
                memory_index,
                offset_expr,
            } => DataMode::Active {
                memory: memory_index,
                offset: parse_const_expr(kinds, &offset_expr)?,
            },
        };
        out.push(DataSegment {
            mode,
            bytes: d.data.to_vec().into_boxed_slice(),
        });
    }
    Ok(())
}

fn parse_const_expr(kinds: &[AggKind], expr: &wasmparser::ConstExpr<'_>) -> Result<ConstExpr> {
    let mut reader = expr.get_operators_reader();
    let mut ops = Vec::new();
    while let Some(op) = conv_const_op(kinds, &reader.read().map_err(wp_err)?)? {
        ops.push(op);
    }
    Ok(ConstExpr(ops.into_boxed_slice()))
}

/// Maps one operator of a constant expression to a [`ConstOp`]; `None` marks the terminating
/// `end`. Naming the operator on the error path lets still-deferred const forms (extern
/// conversions, #27f stage 3) register as a deferred-op skip rather than a bug.
fn conv_const_op(kinds: &[AggKind], op: &Operator<'_>) -> Result<Option<ConstOp>> {
    Ok(Some(match *op {
        Operator::I32Const { value } => ConstOp::I32(value),
        Operator::I64Const { value } => ConstOp::I64(value),
        Operator::F32Const { value } => ConstOp::F32(value.bits()),
        Operator::F64Const { value } => ConstOp::F64(value.bits()),
        Operator::RefNull { hty } => ConstOp::RefNull(conv_reftype_heap(kinds, hty)?),
        Operator::RefFunc { function_index } => ConstOp::RefFunc(function_index),
        Operator::GlobalGet { global_index } => ConstOp::GlobalGet(global_index),
        Operator::RefI31 => ConstOp::RefI31,
        Operator::StructNew { struct_type_index } => ConstOp::StructNew(struct_type_index),
        Operator::StructNewDefault { struct_type_index } => {
            ConstOp::StructNewDefault(struct_type_index)
        }
        Operator::ArrayNew { array_type_index } => ConstOp::ArrayNew(array_type_index),
        Operator::ArrayNewDefault { array_type_index } => {
            ConstOp::ArrayNewDefault(array_type_index)
        }
        Operator::ArrayNewFixed {
            array_type_index,
            array_size,
        } => ConstOp::ArrayNewFixed {
            ty: array_type_index,
            n: array_size,
        },
        Operator::ArrayNewData {
            array_type_index,
            array_data_index,
        } => ConstOp::ArrayNewData {
            ty: array_type_index,
            data: array_data_index,
        },
        Operator::ArrayNewElem {
            array_type_index,
            array_elem_index,
        } => ConstOp::ArrayNewElem {
            ty: array_type_index,
            elem: array_elem_index,
        },
        Operator::AnyConvertExtern => ConstOp::AnyConvertExtern,
        Operator::ExternConvertAny => ConstOp::ExternConvertAny,
        Operator::End => return Ok(None),
        ref other => {
            return Err(Error::msg(format!(
                "unsupported constant expression: {other:?}"
            )))
        }
    }))
}

/// Tag-index → type-index (imported tags first, then defined), for `try_table` catch arity.
fn tag_type_indices(m: &ModuleInner) -> Vec<u32> {
    m.imports
        .iter()
        .filter_map(|i| match i.kind {
            ImportKind::Tag(t) => Some(t),
            _ => None,
        })
        .chain(m.tags.iter().map(|t| t.type_index))
        .collect()
}

fn compile_bodies(m: &ModuleInner, bodies: &[FunctionBody<'_>]) -> Result<Vec<Arc<CompiledFunc>>> {
    let kinds: Vec<AggKind> = m.types.iter().map(ModuleType::kind).collect();
    let tag_types = tag_type_indices(m);
    let ctx = CompileCtx {
        types: &m.types,
        kinds: &kinds,
        func_types: &m.func_types,
        tag_types: &tag_types,
    };
    let mut out = Vec::with_capacity(bodies.len());
    for (i, body) in bodies.iter().enumerate() {
        let type_idx = m.func_types[m.num_imported_funcs as usize + i];
        out.push(Arc::new(translate_function(&ctx, type_idx, body)?));
    }
    Ok(out)
}

// --- wasmparser type → our type conversions ---

fn conv_memtype(mt: wasmparser::MemoryType) -> MemoryType {
    if mt.memory64 {
        MemoryType::new64(mt.initial, mt.maximum)
    } else {
        MemoryType::new(mt.initial as u32, mt.maximum.map(|m| m as u32))
    }
}

fn conv_globaltype(kinds: &[AggKind], gt: wasmparser::GlobalType) -> Result<IrGlobalType> {
    Ok(IrGlobalType {
        content: conv_valtype(kinds, gt.content_type)?,
        mutable: gt.mutable,
    })
}

fn conv_tabletype(kinds: &[AggKind], tt: wasmparser::TableType) -> Result<IrTableType> {
    Ok(IrTableType {
        element: conv_reftype(kinds, tt.element_type)?,
        min: tt.initial as u32,
        max: tt.maximum.map(|m| m as u32),
    })
}

fn conv_reftype(kinds: &[AggKind], rt: wasmparser::RefType) -> Result<IrRef> {
    match conv_valtype(kinds, wasmparser::ValType::Ref(rt))? {
        IrVal::Ref { nullable, heap } => Ok(IrRef { nullable, heap }),
        _ => unreachable!("Ref maps to Ref"),
    }
}

fn conv_reftype_heap(kinds: &[AggKind], hty: wasmparser::HeapType) -> Result<IrHeap> {
    let rt = wasmparser::RefType::new(true, hty).ok_or_else(|| Error::msg("bad ref type"))?;
    Ok(conv_reftype(kinds, rt)?.heap)
}
