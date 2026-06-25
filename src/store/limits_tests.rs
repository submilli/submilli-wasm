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

#[test]
fn instantiation_memory_initial_size_limited() {
    // A defined memory's *initial* size is consulted at instantiation (not only on grow).
    let mut store = store_with(StoreLimitsBuilder::new().memory_size(PAGE).build());
    let (_e, m) = module("(module (memory 2))");
    assert!(Instance::new(&mut store, &m, &[]).is_err());
}

#[test]
fn instantiation_table_initial_size_limited() {
    let mut store = store_with(StoreLimitsBuilder::new().table_elements(1).build());
    let (_e, m) = module("(module (table 2 funcref))");
    assert!(Instance::new(&mut store, &m, &[]).is_err());
}

#[test]
fn memory_count_limited() {
    let mut store = store_with(StoreLimitsBuilder::new().memories(1).build());
    let (_e, m) = module("(module (memory 1) (memory 1))");
    assert!(Instance::new(&mut store, &m, &[]).is_err());
}

#[test]
fn table_count_limited() {
    let mut store = store_with(StoreLimitsBuilder::new().tables(1).build());
    let (_e, m) = module("(module (table 1 funcref) (table 1 funcref))");
    assert!(Instance::new(&mut store, &m, &[]).is_err());
}

#[test]
fn default_ceiling_rejects_huge_memory64_without_limiter() {
    // No limiter installed: the finite default ceiling (4 GiB) is the bound. A memory64 declaring a
    // 2^48-byte initial is rejected cleanly at instantiation — no allocation, no abort.
    let (engine, m) = module("(module (memory i64 0x100000000))");
    let mut store = Store::new(&engine, ());
    assert!(Instance::new(&mut store, &m, &[]).is_err());
}

#[test]
fn over_large_initial_memory_errors_not_aborts() {
    // A permissive limiter authorizes a 2^63-byte memory64, but it exceeds `isize::MAX` — the
    // fallible `try_reserve` must surface a clean error rather than OOM-aborting the process.
    let mut store = store_with(StoreLimitsBuilder::new().memory_size(usize::MAX).build());
    let (_e, m) = module("(module (memory i64 0x800000000000))");
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

#[test]
fn gc_reservation_grows_past_abort_cap_with_limiter() {
    // With a limiter installed, the limiter is the sole bound — the GC reservation may grow past the
    // abort-safety cap (which only applies when no limiter is present).
    let mut store = store_with(StoreLimitsBuilder::new().memory_size(usize::MAX).build());
    let target = crate::store::gc::ABORT_SAFETY_CAP + (1 << 20);
    store.grow_gc_reservation(target, target as u64).unwrap();
    assert!(
        store.inner.gc.reserved() >= target,
        "limiter allowed growth past the abort cap"
    );
}

#[test]
fn gc_reservation_capped_at_abort_cap_without_limiter() {
    // No limiter: the abort-safety cap is the hard ceiling — growth up to it succeeds, past it traps.
    let cap = crate::store::gc::ABORT_SAFETY_CAP;
    let mut store = Store::new(&Engine::default(), ());
    store.grow_gc_reservation(cap, cap as u64).unwrap();
    assert!(store.grow_gc_reservation(cap + 1, 1).is_err());
}
