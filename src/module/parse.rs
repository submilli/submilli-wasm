//! Decode a validated wasm binary into a [`ModuleInner`]: walk the sections,
//! convert types/segments to our owned representation, and compile each function
//! body to internal bytecode via [`translate_function`].

// Memory/table limits are validated to fit u32 for 32-bit memories/tables.
#![allow(clippy::cast_possible_truncation)]

use std::sync::Arc;

use wasmparser::{
    BinaryReaderError, DataKind, ElementItems, ElementKind, ExternalKind, FuncToValidate,
    FunctionBody, GlobalSectionReader, ImportSectionReader, Parser, Payload, TypeRef, ValidPayload,
    Validator, ValidatorResources,
};

use crate::canon::{AggKind, ModuleType};
use crate::engine::Engine;
use crate::module::compile::{
    conv_globaltype, conv_memtype, conv_tabletype, translate_function, CompileCtx,
};
use crate::module::const_expr::parse_const_expr;
use crate::module::inner::{
    DataMode, DataSegment, ElemItems, ElemMode, ElemSegment, Export, ExportKind, GlobalDef, Import,
    ImportKind, ModuleInner, TableDef, TagDef,
};
use crate::module::op::CompiledFunc;
use crate::{Error, Result};

pub(crate) fn wp_err(e: BinaryReaderError) -> Error {
    Error::msg(e.to_string())
}

/// Decodes, validates, and compiles a wasm binary into a [`ModuleInner`] in a single pass:
/// the `wasmparser` [`Validator`] is driven payload-by-payload alongside our decode (so each
/// section and function body is walked exactly once — not re-parsed by a separate `validate_all`).
/// Debug retention is driven by the engine's config (#29c): the per-`Op` offsets + `name` section,
/// and `.debug_*` bytes.
pub(crate) fn parse_module(
    engine: &Engine,
    bytes: &[u8],
    max_module_bytes: usize,
) -> Result<ModuleInner> {
    // Validation-time complexity bound (#32): reject an oversize module before allocating any of
    // the decode/compile state (the streaming decode + the data-segment byte copies below are
    // O(input)). `wasmparser` enforces the per-dimension ceilings (function body size, locals,
    // segment/type/function counts); this caps their aggregate so a hostile module can't OOM the
    // compiler. The trusted-artifact `Module::deserialize` path is exempt.
    if bytes.len() > max_module_bytes {
        return Err(Error::msg(format!(
            "module size {} exceeds configured limit {}",
            bytes.len(),
            max_module_bytes
        )));
    }
    let keep_offsets = engine.wasm_backtrace_enabled();
    let keep_dwarf = engine.retain_dwarf();
    let mut m = empty_inner();
    let mut validator = Validator::new_with_features(super::enabled_features());
    // Module-level validation passes here; per-body operator validation is fused into the
    // translate pass below, so each function's operators are decoded just once.
    let mut funcs: Vec<FuncToValidate<ValidatorResources>> = Vec::new();
    let mut bodies: Vec<FunctionBody<'_>> = Vec::new();

    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(wp_err)?;
        let valid = validator.payload(&payload).map_err(wp_err)?;
        decode_payload(&mut m, payload, keep_offsets, keep_dwarf)?;
        if let ValidPayload::Func(to_validate, body) = valid {
            funcs.push(to_validate);
            bodies.push(body);
        }
    }

    m.functions = compile_bodies(&m, funcs, &bodies, keep_offsets)?;
    // Register the module's rec groups in the engine, baking canonical type ids.
    m.intern(engine);
    Ok(m)
}

/// Decodes one validated payload into the in-progress [`ModuleInner`]. Function bodies are not
/// handled here — they ride the `ValidPayload::Func` channel back in [`parse_module`].
fn decode_payload(
    m: &mut ModuleInner,
    payload: Payload<'_>,
    keep_offsets: bool,
    keep_dwarf: bool,
) -> Result<()> {
    match payload {
        Payload::TypeSection(r) => super::typesec::parse_types(&mut m.types, r)?,
        Payload::ImportSection(r) => parse_imports(m, r)?,
        Payload::FunctionSection(r) => {
            for ty in r {
                m.func_types.push(ty.map_err(wp_err)?);
            }
        }
        Payload::TableSection(r) => parse_tables(m, r)?,
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
        Payload::CodeSectionStart { range, .. } => {
            m.debug.set_code_base(range.start as u32);
        }
        Payload::CustomSection(reader) if keep_offsets || keep_dwarf => {
            retain_debug_section(m, &reader, keep_offsets, keep_dwarf);
        }
        _ => {}
    }
    Ok(())
}

fn parse_tables(m: &mut ModuleInner, reader: wasmparser::TableSectionReader<'_>) -> Result<()> {
    let kinds = m.type_kinds();
    for t in reader {
        let t = t.map_err(wp_err)?;
        let ty = conv_tabletype(&kinds, t.ty)?;
        let init = match t.init {
            wasmparser::TableInit::RefNull => None,
            wasmparser::TableInit::Expr(e) => Some(parse_const_expr(&kinds, &e)?),
        };
        m.tables.push(TableDef { ty, init });
    }
    Ok(())
}

/// Retains the `name`/`.debug_*` custom sections for backtraces, per the keep flags (#29a/#29c).
fn retain_debug_section(
    m: &mut ModuleInner,
    reader: &wasmparser::CustomSectionReader<'_>,
    keep_offsets: bool,
    keep_dwarf: bool,
) {
    let name = reader.name();
    if keep_offsets && name == "name" {
        m.debug
            .add_name_section(reader.data(), reader.data_offset());
    } else if keep_dwarf && name.starts_with(".debug") {
        m.debug.add_dwarf_section(name, reader.data());
    }
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
        debug: crate::module::debug::DebugSections::default(),
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

fn compile_bodies(
    m: &ModuleInner,
    funcs: Vec<FuncToValidate<ValidatorResources>>,
    bodies: &[FunctionBody<'_>],
    retain_offsets: bool,
) -> Result<Vec<Arc<CompiledFunc>>> {
    let kinds: Vec<AggKind> = m.types.iter().map(ModuleType::kind).collect();
    let tag_types = tag_type_indices(m);
    let ctx = CompileCtx {
        types: &m.types,
        kinds: &kinds,
        func_types: &m.func_types,
        tag_types: &tag_types,
    };
    // Recycled across bodies so per-function validation reuses one scratch arena.
    let mut allocs = wasmparser::FuncValidatorAllocations::default();
    // Recycled across bodies so per-function `ctrl`/`local_types` reuse one allocation.
    let mut scratch = super::compile::Scratch::default();
    let mut out = Vec::with_capacity(bodies.len());
    for (i, (to_validate, body)) in funcs.into_iter().zip(bodies).enumerate() {
        let type_idx = m.func_types[m.num_imported_funcs as usize + i];
        let mut validator = to_validate.into_validator(allocs);
        out.push(Arc::new(translate_function(
            &ctx,
            type_idx,
            body,
            &mut validator,
            retain_offsets,
            &mut scratch,
        )?));
        allocs = validator.into_allocations();
    }
    Ok(out)
}
