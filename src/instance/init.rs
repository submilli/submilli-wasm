//! Instantiation: link imports, allocate the defined entities, and initialize the
//! active element/data segments. (The start function is run by `Instance::new`.)

use super::const_eval::{elem_refs, eval_const, eval_const_ref, ConstCtx};
use crate::canon::{self, AggKind, IrGlobalType, IrHeap, IrTableType};
use crate::extern_::{Extern, Global, Memory, Table, Tag};
use crate::func::Func;
use crate::instance::Instance;
use crate::module::inner::{DataMode, ElemMode, ImportKind};
use crate::module::Module;
use crate::store::{
    FuncEntity, GlobalEntity, InstanceEntity, MemoryEntity, StoreInner, TableEntity, TagEntity,
};
use crate::trap::Trap;
use crate::value::{MemoryType, Mutability, Ref, Val};
use crate::{Error, Result};

/// Index spaces built from a module's imports.
type Imported = (Vec<Func>, Vec<Memory>, Vec<Global>, Vec<Table>, Vec<Tag>);

/// Instantiates `module` against `imports` (positional, matching the module's
/// import declarations), returning the new instance handle.
pub(crate) fn instantiate(
    inner: &mut StoreInner,
    module: &Module,
    imports: &[Extern],
) -> Result<Instance> {
    let m = module.inner();
    if m.imports.len() != imports.len() {
        return Err(Error::msg("wrong number of imports"));
    }

    let (mut funcs, mut memories, mut globals, mut tables, mut tags) =
        link_imports(inner, module, imports)?;
    for mt in &m.memories {
        memories.push(inner.alloc_memory(MemoryEntity::new(mt.clone())));
    }
    // Allocate a fresh entity per defined tag — this mints its store-address identity.
    for t in &m.tags {
        let ty = m.tag_type(t.type_index);
        tags.push(inner.alloc_tag(TagEntity { ty }));
    }
    // Function handles are allocated before tables/globals so a table or global
    // initializer can `ref.func` a defined function (reference-types / function-references).
    let instance = inner.reserve_instance();
    for i in 0..m.functions.len() as u32 {
        funcs.push(inner.alloc_func(FuncEntity::Wasm {
            instance,
            func_index: m.num_imported_funcs + i,
        }));
    }
    alloc_defined_tables(inner, module, &funcs, &globals, &mut tables)?;

    init_defined_globals(inner, module, &funcs, &mut globals)?;

    // Evaluate every element segment once (reference identity must be stable across
    // `table.init`/`array.new_elem`). Passive segments keep their refs as the instance's element
    // instances; active/declared are dropped (empty). The instance is allocated *before* active
    // segments are applied so that — if a later active segment traps — funcrefs already written
    // into a (possibly shared/imported) table still resolve to a live instance.
    let evaluated = eval_elems(inner, module, &funcs, &globals)?;
    let elems = passive_elem_instances(m, &evaluated);
    let allocated = inner.alloc_instance(InstanceEntity {
        module: module.clone(),
        funcs: funcs.clone(),
        memories: memories.clone(),
        globals: globals.clone(),
        tables: tables.clone(),
        tags,
        dropped_data: vec![false; m.datas.len()],
        elems,
    });
    debug_assert_eq!(allocated.index, instance.index);

    apply_active_elems(inner, module, &funcs, &tables, &globals, &evaluated)?;
    init_datas(inner, module, instance, &memories, &globals)?;
    // The start function is run by `Instance::new` (it needs the typed `Store<T>`
    // so a host-imported start can build a `Caller`).
    Ok(instance)
}

/// Allocates the module's defined tables, evaluating each table's optional initializer
/// (defaulting to a typed null), and appends their handles to `tables`.
fn alloc_defined_tables(
    inner: &mut StoreInner,
    module: &Module,
    funcs: &[Func],
    globals: &[Global],
    tables: &mut Vec<Table>,
) -> Result<()> {
    let m = module.inner();
    for td in &m.tables {
        let init = match &td.init {
            Some(e) => {
                let ctx = ConstCtx {
                    module,
                    funcs,
                    globals,
                };
                eval_const_ref(inner, &ctx, e)?
            }
            None => null_ref(&td.ty.element.heap),
        };
        let ty = canon::materialize_table(m.engine(), &m.type_ids, &td.ty);
        tables.push(inner.alloc_table(TableEntity::new(ty, init)));
    }
    Ok(())
}

/// The per-segment element instances stored on a fresh instance: passive segments keep their
/// evaluated refs; active/declared segments hold an empty (dropped) instance.
fn passive_elem_instances(
    m: &crate::module::inner::ModuleInner,
    evaluated: &[Vec<Ref>],
) -> Vec<Vec<Ref>> {
    m.elems
        .iter()
        .zip(evaluated)
        .map(|(seg, refs)| match seg.mode {
            ElemMode::Passive => refs.clone(),
            ElemMode::Active { .. } | ElemMode::Declared => Vec::new(),
        })
        .collect()
}

/// Allocates the module's defined globals, baking their types to canonical ids. (`ref.func`
/// initializers resolve against the already-allocated function handles.)
fn init_defined_globals(
    inner: &mut StoreInner,
    module: &Module,
    funcs: &[Func],
    globals: &mut Vec<Global>,
) -> Result<()> {
    let m = module.inner();
    for g in &m.globals {
        let ctx = ConstCtx {
            module,
            funcs,
            globals,
        };
        let value = eval_const(inner, &ctx, &g.init)?;
        globals.push(inner.alloc_global(GlobalEntity {
            value,
            ty: canon::materialize_global(m.engine(), &m.type_ids, &g.ty),
        }));
    }
    Ok(())
}

fn link_imports(inner: &StoreInner, module: &Module, imports: &[Extern]) -> Result<Imported> {
    let mut spaces: Imported = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (imp, ext) in module.inner().imports.iter().zip(imports) {
        match (&imp.kind, ext) {
            (ImportKind::Func(t), Extern::Func(f)) => {
                check_func(inner, module, *t, *f)?;
                spaces.0.push(*f);
            }
            (ImportKind::Memory(mt), Extern::Memory(mem)) => {
                check_memory(inner, mt, *mem)?;
                spaces.1.push(*mem);
            }
            (ImportKind::Global(gt), Extern::Global(g)) => {
                check_global(inner, module, gt, *g)?;
                spaces.2.push(*g);
            }
            (ImportKind::Table(tt), Extern::Table(t)) => {
                check_table(inner, module, tt, *t)?;
                spaces.3.push(*t);
            }
            (ImportKind::Tag(t), Extern::Tag(tag)) => {
                check_tag(inner, module, *t, *tag)?;
                spaces.4.push(*tag);
            }
            _ => return Err(Error::msg("import kind mismatch")),
        }
    }
    Ok(spaces)
}

/// Tag imports match by **exact** func-type identity (invariant — unlike covariant funcs/globals):
/// the provided tag's signature must canonically equal the importer's declared one.
fn check_tag(inner: &StoreInner, module: &Module, type_idx: u32, tag: Tag) -> Result<()> {
    let expected_id = module.inner().canonical_type_id(type_idx);
    if inner.tag(tag).ty.ty().canonical_id() == expected_id {
        Ok(())
    } else {
        Err(Error::msg("imported tag type mismatch"))
    }
}

fn check_func(inner: &StoreInner, module: &Module, type_idx: u32, f: Func) -> Result<()> {
    let expected_id = module.inner().canonical_type_id(type_idx);
    let ok = match inner.func(f) {
        FuncEntity::Wasm {
            instance,
            func_index,
        } => {
            let pmod = inner.instance(*instance).module.inner();
            let actual_id = pmod.canonical_type_id(pmod.func_types[*func_index as usize]);
            inner.engine().is_subtype(actual_id, expected_id)
        }
        // Host func types are interned too — compare canonical ids uniformly.
        FuncEntity::Host { ty, .. } => inner.engine().is_subtype(ty.canonical_id(), expected_id),
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { ty, .. } => {
            inner.engine().is_subtype(ty.canonical_id(), expected_id)
        }
    };
    if ok {
        Ok(())
    } else {
        Err(Error::msg("imported function signature mismatch"))
    }
}

fn check_memory(inner: &StoreInner, declared: &MemoryType, m: Memory) -> Result<()> {
    let entity = inner.memory(m);
    // Current page count is the effective minimum (mirrors `check_table`).
    if limits_ok(
        entity.size_pages(),
        entity.ty.maximum(),
        declared.minimum(),
        declared.maximum(),
    ) {
        Ok(())
    } else {
        Err(Error::msg("imported memory limits mismatch"))
    }
}

fn check_table(
    inner: &StoreInner,
    module: &Module,
    declared: &IrTableType,
    t: Table,
) -> Result<()> {
    let entity = inner.table(t);
    // Materialize the importer's declared element type to canonical handles (the provider
    // entity's type is already canonical), so concrete GC/func element types compare cross-module.
    let m = module.inner();
    let declared = canon::materialize_table(m.engine(), &m.type_ids, declared);
    // Import matching uses the table's *current size* as its effective minimum (a table
    // grown past its declared minimum still satisfies imports declaring that larger min).
    let ok = entity.ty.element() == declared.element()
        && limits_ok(
            entity.size(),
            entity.ty.maximum(),
            declared.minimum(),
            declared.maximum(),
        );
    if ok {
        Ok(())
    } else {
        Err(Error::msg("imported table type mismatch"))
    }
}

fn check_global(
    inner: &StoreInner,
    module: &Module,
    declared: &IrGlobalType,
    g: Global,
) -> Result<()> {
    let actual = &inner.global(g).ty;
    // Materialize the importer's declared type to canonical handles (the entity's is canonical).
    let m = module.inner();
    let declared = canon::materialize_global(m.engine(), &m.type_ids, declared);
    let ok = actual.mutability() == declared.mutability()
        && match declared.mutability() {
            // Immutable globals are read-only, so the export type need only be a subtype of
            // the import type (covariant). Mutable globals are read+written, so invariant.
            Mutability::Const => actual.content().matches(declared.content()),
            Mutability::Var => actual.content() == declared.content(),
        };
    if ok {
        Ok(())
    } else {
        Err(Error::msg("imported global type mismatch"))
    }
}

/// An actual `{amin, amax}` satisfies an import requiring `{imin, imax}`.
fn limits_ok(amin: u64, amax: Option<u64>, imin: u64, imax: Option<u64>) -> bool {
    amin >= imin && imax.is_none_or(|im| amax.is_some_and(|am| am <= im))
}

/// Evaluates every element segment's items once (their reference identity must be stable).
fn eval_elems(
    inner: &mut StoreInner,
    module: &Module,
    funcs: &[Func],
    globals: &[Global],
) -> Result<Vec<Vec<Ref>>> {
    let ctx = ConstCtx {
        module,
        funcs,
        globals,
    };
    let mut out = Vec::with_capacity(module.inner().elems.len());
    for seg in &module.inner().elems {
        out.push(elem_refs(inner, &ctx, &seg.items)?);
    }
    Ok(out)
}

/// Applies each active element segment's (pre-evaluated) refs to its table.
fn apply_active_elems(
    inner: &mut StoreInner,
    module: &Module,
    funcs: &[Func],
    tables: &[Table],
    globals: &[Global],
    evaluated: &[Vec<Ref>],
) -> Result<()> {
    let ctx = ConstCtx {
        module,
        funcs,
        globals,
    };
    for (seg, refs) in module.inner().elems.iter().zip(evaluated) {
        if let ElemMode::Active { table, offset } = &seg.mode {
            let dst = const_to_usize(eval_const(inner, &ctx, offset)?)?;
            apply_active_elem(inner, tables[*table as usize], dst, refs)?;
        }
    }
    Ok(())
}

/// Writes an active element segment's refs into `table` at `dst`, trapping if out of bounds.
fn apply_active_elem(
    inner: &mut StoreInner,
    handle: Table,
    dst: usize,
    refs: &[Ref],
) -> Result<()> {
    let size = inner.table(handle).size() as usize;
    if dst.checked_add(refs.len()).is_none_or(|end| end > size) {
        return Err(Trap::TableOutOfBounds.into());
    }
    for (i, r) in refs.iter().enumerate() {
        inner.table_mut(handle).set((dst + i) as u64, r.clone());
    }
    Ok(())
}

fn init_datas(
    inner: &mut StoreInner,
    module: &Module,
    instance: Instance,
    memories: &[Memory],
    globals: &[Global],
) -> Result<()> {
    for (seg_idx, seg) in module.inner().datas.iter().enumerate() {
        let DataMode::Active { memory, offset } = &seg.mode else {
            continue;
        };
        let ctx = ConstCtx {
            module,
            funcs: &[],
            globals,
        };
        let dst = const_to_usize(eval_const(inner, &ctx, offset)?)?;
        let mem = memories[*memory as usize];
        let len = seg.bytes.len();
        let mem_len = inner.memory(mem).bytes.len();
        if dst.checked_add(len).is_none_or(|end| end > mem_len) {
            return Err(Trap::MemoryOutOfBounds.into());
        }
        inner.memory_mut(mem).bytes[dst..dst + len].copy_from_slice(&seg.bytes);
        inner.instance_mut(instance).dropped_data[seg_idx] = true;
    }
    Ok(())
}

fn const_to_usize(v: Val) -> Result<usize> {
    match v {
        Val::I32(x) => Ok(x as u32 as usize),
        Val::I64(x) => Ok(x as u64 as usize),
        _ => Err(Error::msg("segment offset is not an integer")),
    }
}

/// The null reference for an IR heap type (by hierarchy).
fn null_ref(heap: &IrHeap) -> Ref {
    match heap {
        IrHeap::Func | IrHeap::NoFunc | IrHeap::Concrete(_, AggKind::Func) => Ref::Func(None),
        IrHeap::Extern | IrHeap::NoExtern => Ref::Extern(None),
        IrHeap::Exn | IrHeap::NoExn => Ref::Exn(None),
        _ => Ref::Any(None),
    }
}
