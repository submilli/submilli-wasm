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

use crate::engine::Engine;
use crate::module::compile::{conv_valtype, translate_function, CompileCtx};
use crate::module::inner::{
    ConstExpr, DataMode, DataSegment, ElemItems, ElemMode, ElemSegment, Export, ExportKind,
    GlobalDef, Import, ImportKind, ModuleInner, TableDef,
};
use crate::module::op::CompiledFunc;
use crate::value::{FuncType, GlobalType, MemoryType, Mutability, RefType, TableType, ValType};
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
            Payload::TypeSection(r) => parse_types(engine, &mut m.types, r)?,
            Payload::ImportSection(r) => parse_imports(&mut m, r)?,
            Payload::FunctionSection(r) => {
                for ty in r {
                    m.func_types.push(ty.map_err(wp_err)?);
                }
            }
            Payload::TableSection(r) => {
                for t in r {
                    let t = t.map_err(wp_err)?;
                    let ty = conv_tabletype(&m.types, t.ty)?;
                    let init = match t.init {
                        wasmparser::TableInit::RefNull => None,
                        wasmparser::TableInit::Expr(e) => Some(parse_const_expr(&m.types, &e)?),
                    };
                    m.tables.push(TableDef { ty, init });
                }
            }
            Payload::MemorySection(r) => {
                for mt in r {
                    m.memories.push(conv_memtype(mt.map_err(wp_err)?));
                }
            }
            Payload::GlobalSection(r) => parse_globals(&m.types, &mut m.globals, r)?,
            Payload::ExportSection(r) => parse_exports(&mut m.exports, r)?,
            Payload::StartSection { func, .. } => m.start = Some(func),
            Payload::ElementSection(r) => parse_elems(&m.types, &mut m.elems, r)?,
            Payload::DataSection(r) => parse_datas(&m.types, &mut m.datas, r)?,
            Payload::CodeSectionEntry(body) => bodies.push(body),
            _ => {}
        }
    }

    m.functions = compile_bodies(&m.types, &m.func_types, m.num_imported_funcs, &bodies)?;
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
        datas: Vec::new(),
        elems: Vec::new(),
        start: None,
    }
}

fn parse_types(
    engine: &Engine,
    out: &mut Vec<FuncType>,
    reader: wasmparser::TypeSectionReader<'_>,
) -> Result<()> {
    for rec in reader {
        for sub in rec.map_err(wp_err)?.into_types() {
            let wasmparser::CompositeInnerType::Func(ft) = sub.composite_type.inner else {
                return Err(Error::msg("non-function composite types not supported"));
            };
            let params = conv_valtypes(out, ft.params())?;
            let results = conv_valtypes(out, ft.results())?;
            out.push(FuncType::new(engine, params, results));
        }
    }
    Ok(())
}

fn conv_valtypes(types: &[FuncType], tys: &[wasmparser::ValType]) -> Result<Vec<ValType>> {
    tys.iter()
        .copied()
        .map(|t| conv_valtype(types, t))
        .collect()
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
    let kind = match imp.ty {
        TypeRef::Func(t) => {
            m.func_types.push(t);
            m.num_imported_funcs += 1;
            ImportKind::Func(t)
        }
        TypeRef::Table(tt) => ImportKind::Table(conv_tabletype(&m.types, tt)?),
        TypeRef::Memory(mt) => ImportKind::Memory(conv_memtype(mt)),
        TypeRef::Global(gt) => ImportKind::Global(conv_globaltype(&m.types, gt)?),
        TypeRef::Tag(_) | TypeRef::FuncExact(_) => {
            return Err(Error::msg("unsupported import kind"))
        }
    };
    m.imports.push(Import {
        module: imp.module.to_string(),
        name: imp.name.to_string(),
        kind,
    });
    Ok(())
}

fn parse_globals(
    types: &[FuncType],
    out: &mut Vec<GlobalDef>,
    reader: GlobalSectionReader<'_>,
) -> Result<()> {
    for g in reader {
        let g = g.map_err(wp_err)?;
        out.push(GlobalDef {
            ty: conv_globaltype(types, g.ty)?,
            init: parse_const_expr(types, &g.init_expr)?,
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
            ExternalKind::Tag | ExternalKind::FuncExact => {
                return Err(Error::msg("unsupported export kind"))
            }
        };
        out.push(Export {
            name: e.name.to_string(),
            kind,
        });
    }
    Ok(())
}

fn parse_elems(
    types: &[FuncType],
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
                offset: parse_const_expr(types, &offset_expr)?,
            },
        };
        out.push(ElemSegment {
            mode,
            items: conv_elem_items(types, e.items)?,
        });
    }
    Ok(())
}

fn conv_elem_items(types: &[FuncType], items: ElementItems<'_>) -> Result<ElemItems> {
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
                .map(|e| parse_const_expr(types, &e.map_err(wp_err)?))
                .collect::<Result<Vec<_>>>()?;
            Ok(ElemItems::Exprs(exprs.into_boxed_slice()))
        }
    }
}

fn parse_datas(
    types: &[FuncType],
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
                offset: parse_const_expr(types, &offset_expr)?,
            },
        };
        out.push(DataSegment {
            mode,
            bytes: d.data.to_vec().into_boxed_slice(),
        });
    }
    Ok(())
}

fn parse_const_expr(types: &[FuncType], expr: &wasmparser::ConstExpr<'_>) -> Result<ConstExpr> {
    let mut ops = expr.get_operators_reader();
    let op = ops.read().map_err(wp_err)?;
    Ok(match op {
        Operator::I32Const { value } => ConstExpr::I32(value),
        Operator::I64Const { value } => ConstExpr::I64(value),
        Operator::F32Const { value } => ConstExpr::F32(value.bits()),
        Operator::F64Const { value } => ConstExpr::F64(value.bits()),
        Operator::RefNull { hty } => ConstExpr::RefNull(conv_reftype_heap(types, hty)?),
        Operator::RefFunc { function_index } => ConstExpr::RefFunc(function_index),
        Operator::GlobalGet { global_index } => ConstExpr::GlobalGet(global_index),
        _ => return Err(Error::msg("unsupported constant expression")),
    })
}

fn compile_bodies(
    types: &[FuncType],
    func_types: &[u32],
    num_imported_funcs: u32,
    bodies: &[FunctionBody<'_>],
) -> Result<Vec<Arc<CompiledFunc>>> {
    let ctx = CompileCtx { types, func_types };
    let mut out = Vec::with_capacity(bodies.len());
    for (i, body) in bodies.iter().enumerate() {
        let type_idx = func_types[num_imported_funcs as usize + i];
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

fn conv_globaltype(types: &[FuncType], gt: wasmparser::GlobalType) -> Result<GlobalType> {
    let mutability = if gt.mutable {
        Mutability::Var
    } else {
        Mutability::Const
    };
    Ok(GlobalType::new(
        conv_valtype(types, gt.content_type)?,
        mutability,
    ))
}

fn conv_tabletype(types: &[FuncType], tt: wasmparser::TableType) -> Result<TableType> {
    Ok(TableType::new(
        conv_reftype(types, tt.element_type)?,
        tt.initial as u32,
        tt.maximum.map(|m| m as u32),
    ))
}

fn conv_reftype(types: &[FuncType], rt: wasmparser::RefType) -> Result<RefType> {
    match conv_valtype(types, wasmparser::ValType::Ref(rt))? {
        ValType::Ref(r) => Ok(r),
        _ => unreachable!("Ref maps to Ref"),
    }
}

fn conv_reftype_heap(types: &[FuncType], hty: wasmparser::HeapType) -> Result<RefType> {
    conv_reftype(
        types,
        wasmparser::RefType::new(true, hty).ok_or_else(|| Error::msg("bad ref type"))?,
    )
}
