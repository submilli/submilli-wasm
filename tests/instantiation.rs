//! #32b: instantiation safety. Guest code runs during `Instance::new` — the `start` function and
//! the active data/element segment initializers — *before* any export is callable. So fuel, epoch,
//! and stack limits must already be armed at instantiation, and a hostile module must abort
//! instantiation cleanly (a `Trap`, never a panic/hang). Stack-during-`start` is covered in
//! `tests/stack_limit.rs`; here we cover fuel + epoch during `start` and active-segment OOB.
//!
//! Instantiation is **not** transactionally rolled back: on failure the store retains the
//! partially-allocated entities (the multi-tenant stance is a per-attempt `Store` discarded on
//! failure — see `docs/ARCHITECTURE.md` §8). These tests also confirm the store is not *poisoned* —
//! a benign module still instantiates after a failed attempt.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Config, Engine, Instance, Module, Store, Trap, Val};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

fn trap_of(err: &submilli_wasm::Error) -> Trap {
    *err.downcast_ref::<Trap>()
        .unwrap_or_else(|| panic!("expected a Trap, got: {err}"))
}

/// A looping `start` function with no exports — used to prove fuel/epoch interrupt instantiation.
const LOOPING_START: &str = "(module (func $f (loop (br 0))) (start $f))";
const BENIGN: &str = "(module (func (export \"id\") (result i32) (i32.const 7)))";

/// Instantiates `BENIGN` on the given store and calls its export — proves the store is still usable.
fn assert_store_usable(store: &mut Store<()>, engine: &Engine) {
    let benign = module(engine, BENIGN);
    let inst = Instance::new(&mut *store, &benign, &[]).unwrap();
    let id = inst.get_func(&mut *store, "id").unwrap();
    let mut out = [Val::I32(0)];
    id.call(&mut *store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
}

/// Fuel must be armed before `Instance::new`: a fuel-hungry `start` aborts instantiation with
/// `OutOfFuel`, and the store stays usable afterwards.
#[test]
fn fuel_exhausts_in_start_aborts_instantiation() {
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let m = module(&engine, LOOPING_START);

    let mut store = Store::new(&engine, ());
    store.set_fuel(50).unwrap(); // armed before instantiation
    let err = Instance::new(&mut store, &m, &[]).unwrap_err();
    assert_eq!(trap_of(&err), Trap::OutOfFuel);

    // The store is not poisoned — after replenishing fuel (the embedder's normal recovery), a
    // benign module instantiates and runs. (Limits persist; only the store's integrity is at issue.)
    store.set_fuel(1_000_000).unwrap();
    assert_store_usable(&mut store, &engine);
}

/// Epoch interruption must be armed before `Instance::new`: a deadline already reached aborts a
/// looping `start` with `Interrupt`, and the store stays usable afterwards.
#[test]
fn epoch_deadline_in_start_aborts_instantiation() {
    let mut config = Config::new();
    config.epoch_interruption(true);
    let engine = Engine::new(&config).unwrap();
    let m = module(&engine, LOOPING_START);

    let mut store = Store::new(&engine, ());
    store.set_epoch_deadline(0); // already at the deadline
    let err = Instance::new(&mut store, &m, &[]).unwrap_err();
    assert_eq!(trap_of(&err), Trap::Interrupt);

    // Not poisoned: push the deadline out (the embedder's normal recovery) and a benign module runs.
    store.set_epoch_deadline(1_000_000);
    assert_store_usable(&mut store, &engine);
}

/// An out-of-bounds active *data* segment aborts instantiation with a clean `MemoryOutOfBounds`
/// trap (offset 70000 past a single 64 KiB page), never a panic.
#[test]
fn active_data_segment_oob_fails_cleanly() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module (memory 1) (data (i32.const 70000) \"x\"))",
    );
    let mut store = Store::new(&engine, ());
    let err = Instance::new(&mut store, &m, &[]).unwrap_err();
    assert_eq!(trap_of(&err), Trap::MemoryOutOfBounds);

    assert_store_usable(&mut store, &engine);
}

/// An out-of-bounds active *element* segment aborts instantiation with a clean `TableOutOfBounds`
/// trap (offset 5 into a 1-slot table), never a panic.
#[test]
fn active_elem_segment_oob_fails_cleanly() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module (table 1 funcref) (func $f) (elem (i32.const 5) $f))",
    );
    let mut store = Store::new(&engine, ());
    let err = Instance::new(&mut store, &m, &[]).unwrap_err();
    assert_eq!(trap_of(&err), Trap::TableOutOfBounds);

    assert_store_usable(&mut store, &engine);
}
