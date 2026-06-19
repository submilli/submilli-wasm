//! Instantiation: link imports, allocate the defined entities, and initialize the
//! active element/data segments. (The start function is run by `Instance::new`.)

use crate::canon::{self, AggKind, IrGlobalType, IrHeap, IrTableType};
use crate::extern_::{Extern, Global, Memory, Table};
use crate::func::Func;
use crate::instance::Instance;
use crate::module::inner::{ConstExpr, DataMode, ElemItems, ElemMode, ImportKind};
use crate::module::Module;
use crate::store::{
    FuncEntity, GlobalEntity, InstanceEntity, MemoryEntity, StoreInner, TableEntity,
};
use crate::trap::Trap;
use crate::value::{MemoryType, Mutability, Ref, Val};
use crate::{Error, Result};

/// Index spaces built from a module's imports.
type Imported = (Vec<Func>, Vec<Memory>, Vec<Global>, Vec<Table>);

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

    let (mut funcs, mut memories, mut globals, mut tables) = link_imports(inner, module, imports)?;
    for mt in &m.memories {
        memories.push(inner.alloc_memory(MemoryEntity::new(mt.clone())));
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
    for td in &m.tables {
        let init = match &td.init {
            Some(e) => eval_const_ref(inner, &globals, &funcs, e)?,
            None => null_ref(&td.ty.element.heap),
        };
        let ty = canon::materialize_table(m.engine(), &m.type_ids, &td.ty);
        tables.push(inner.alloc_table(TableEntity::new(ty, init)));
    }

    init_defined_globals(inner, module, &funcs, &mut globals)?;

    // Active/declared element segments are unusable by `table.init` (dropped);
    // only passive segments remain live.
    let dropped_elems = m
        .elems
        .iter()
        .map(|e| !matches!(e.mode, ElemMode::Passive))
        .collect();
    let allocated = inner.alloc_instance(InstanceEntity {
        module: module.clone(),
        funcs: funcs.clone(),
        memories: memories.clone(),
        globals: globals.clone(),
        tables: tables.clone(),
        dropped_data: vec![false; m.datas.len()],
        dropped_elems,
    });
    debug_assert_eq!(allocated.index, instance.index);

    init_elems(inner, module, &funcs, &tables, &globals)?;
    init_datas(inner, module, instance, &memories, &globals)?;
    // The start function is run by `Instance::new` (it needs the typed `Store<T>`
    // so a host-imported start can build a `Caller`).
    Ok(instance)
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
        let value = match &g.init {
            ConstExpr::RefFunc(i) => Val::FuncRef(Some(funcs[*i as usize])),
            other => eval_const(inner, globals, other)?,
        };
        globals.push(inner.alloc_global(GlobalEntity {
            value,
            ty: canon::materialize_global(m.engine(), &m.type_ids, &g.ty),
        }));
    }
    Ok(())
}

fn link_imports(inner: &StoreInner, module: &Module, imports: &[Extern]) -> Result<Imported> {
    let mut spaces: Imported = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
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
            _ => return Err(Error::msg("import kind mismatch")),
        }
    }
    Ok(spaces)
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

fn init_elems(
    inner: &mut StoreInner,
    module: &Module,
    funcs: &[Func],
    tables: &[Table],
    globals: &[Global],
) -> Result<()> {
    for seg in &module.inner().elems {
        let ElemMode::Active { table, offset } = &seg.mode else {
            continue;
        };
        let dst = const_to_usize(eval_const(inner, globals, offset)?)?;
        let refs = elem_refs(inner, globals, funcs, &seg.items)?;
        let handle = tables[*table as usize];
        let size = inner.table(handle).size() as usize;
        if dst.checked_add(refs.len()).is_none_or(|end| end > size) {
            return Err(Trap::TableOutOfBounds.into());
        }
        for (i, r) in refs.into_iter().enumerate() {
            inner.table_mut(handle).set((dst + i) as u64, r);
        }
    }
    Ok(())
}

fn elem_refs(
    inner: &StoreInner,
    globals: &[Global],
    funcs: &[Func],
    items: &ElemItems,
) -> Result<Vec<Ref>> {
    match items {
        ElemItems::Funcs(idxs) => Ok(idxs
            .iter()
            .map(|&i| Ref::Func(Some(funcs[i as usize])))
            .collect()),
        ElemItems::Exprs(exprs) => exprs
            .iter()
            .map(|e| eval_const_ref(inner, globals, funcs, e))
            .collect(),
    }
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
        let dst = const_to_usize(eval_const(inner, globals, offset)?)?;
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

fn eval_const(inner: &StoreInner, globals: &[Global], e: &ConstExpr) -> Result<Val> {
    Ok(match e {
        ConstExpr::I32(v) => Val::I32(*v),
        ConstExpr::I64(v) => Val::I64(*v),
        ConstExpr::F32(v) => Val::F32(*v),
        ConstExpr::F64(v) => Val::F64(*v),
        ConstExpr::RefNull(heap) => Val::null_for_heap(heap),
        ConstExpr::GlobalGet(g) => inner.global(globals[*g as usize]).value,
        ConstExpr::RefFunc(_) => return Err(Error::msg("ref.func outside element segment")),
    })
}

pub(crate) fn eval_const_ref(
    inner: &StoreInner,
    globals: &[Global],
    funcs: &[Func],
    e: &ConstExpr,
) -> Result<Ref> {
    Ok(match e {
        ConstExpr::RefFunc(i) => Ref::Func(Some(funcs[*i as usize])),
        ConstExpr::RefNull(heap) => null_ref(heap),
        ConstExpr::GlobalGet(g) => val_to_ref(inner.global(globals[*g as usize]).value)?,
        _ => return Err(Error::msg("non-reference element expression")),
    })
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

fn val_to_ref(v: Val) -> Result<Ref> {
    match v {
        Val::FuncRef(f) => Ok(Ref::Func(f)),
        Val::ExternRef(e) => Ok(Ref::Extern(e)),
        Val::AnyRef(a) => Ok(Ref::Any(a)),
        Val::ExnRef(x) => Ok(Ref::Exn(x)),
        _ => Err(Error::msg("global is not a reference")),
    }
}
