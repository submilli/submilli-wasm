//! `ResourceLimiter`/`StoreLimits` enforcement: grow + creation + count limits.
#![allow(clippy::unwrap_used)]

use crate::{
    Engine, HeapType, Instance, Memory, MemoryType, Module, Ref, RefType, Store, StoreLimits,
    StoreLimitsBuilder, Table, TableType, Val,
};

const PAGE: usize = 64 * 1024;

fn store_with(limits: StoreLimits) -> Store<StoreLimits> {
    let mut store = Store::new(&Engine::default(), limits);
    store.limiter(|s| s);
    store
}

fn module(wat: &str) -> (Engine, Module) {
    let engine = Engine::default();
    let m = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    (engine, m)
}

#[test]
fn memory_grow_api_respects_limit() {
    let mut store = store_with(StoreLimitsBuilder::new().memory_size(2 * PAGE).build());
    let mem = Memory::new(&mut store, MemoryType::new(1, None)).unwrap();
    assert_eq!(mem.grow(&mut store, 1).unwrap(), 1); // 1 -> 2 pages, ok
    assert!(mem.grow(&mut store, 1).is_err()); // 2 -> 3 pages, denied
}

#[test]
fn memory_new_initial_size_limited() {
    let mut store = store_with(StoreLimitsBuilder::new().memory_size(PAGE).build());
    assert!(Memory::new(&mut store, MemoryType::new(2, None)).is_err());
}

#[test]
fn table_grow_api_respects_limit() {
    let mut store = store_with(StoreLimitsBuilder::new().table_elements(2).build());
    let ty = TableType::new(RefType::new(true, HeapType::Func), 1, None);
    let table = Table::new(&mut store, ty, Ref::Func(None)).unwrap();
    assert_eq!(table.grow(&mut store, 1, Ref::Func(None)).unwrap(), 1);
    assert!(table.grow(&mut store, 1, Ref::Func(None)).is_err());
}

#[test]
fn instance_count_limited() {
    let mut store = store_with(StoreLimitsBuilder::new().instances(1).build());
    let (_engine, m) = module("(module)");
    assert!(Instance::new(&mut store, &m, &[]).is_ok());
    assert!(Instance::new(&mut store, &m, &[]).is_err());
}

const GROWER: &str = "(module
    (memory 1)
    (func (export \"grow\") (param i32) (result i32) local.get 0 memory.grow))";

fn call_grow(store: &mut Store<StoreLimits>, inst: Instance, pages: i32) -> crate::Result<i32> {
    let f = inst.get_func(&mut *store, "grow").unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut *store, &[Val::I32(pages)], &mut out)?;
    Ok(out[0].unwrap_i32())
}

#[test]
fn in_wasm_memory_grow_returns_minus_one_when_denied() {
    let (_engine, m) = module(GROWER);
    let mut store = store_with(StoreLimitsBuilder::new().memory_size(2 * PAGE).build());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(call_grow(&mut store, inst, 1).unwrap(), 1); // 1 -> 2 pages
    assert_eq!(call_grow(&mut store, inst, 1).unwrap(), -1); // denied -> -1
}

#[test]
fn in_wasm_memory_grow_traps_with_trap_on_grow_failure() {
    let (_engine, m) = module(GROWER);
    let mut store = store_with(
        StoreLimitsBuilder::new()
            .memory_size(PAGE)
            .trap_on_grow_failure(true)
            .build(),
    );
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert!(call_grow(&mut store, inst, 1).is_err()); // denied + trap_on_grow_failure -> trap
}
