//! Instantiation + cross-function/indirect-call + segment tests. We drive the
//! interpreter through the internal `exec` path (the public `Func::call` is covered elsewhere).
#![allow(clippy::unwrap_used)]

use super::Instance;
use crate::engine::Engine;
use crate::extern_::Extern;
use crate::module::Module;
use crate::store::Store;
use crate::trap::Trap;
use crate::value::Val;
use crate::{Error, Result};

fn module(engine: &Engine, wat: &str) -> Module {
    let bytes = wat::parse_str(wat).unwrap();
    Module::new(engine, &bytes).unwrap()
}

/// Resolves `name` on `instance` and runs it through the public `Func::call`.
fn call(store: &mut Store<()>, instance: Instance, name: &str, args: Vec<Val>) -> Result<Vec<Val>> {
    let f = instance
        .get_func(&mut *store, name)
        .ok_or_else(|| Error::msg("no such export"))?;
    let mut results = vec![Val::I32(0); f.ty(&*store).results().len()];
    f.call(&mut *store, &args, &mut results)?;
    Ok(results)
}

fn trap_of(r: Result<Vec<Val>>) -> Trap {
    *r.unwrap_err().downcast_ref::<Trap>().expect("a trap")
}

#[test]
fn direct_call_between_functions() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (func $helper (param i32) (result i32) local.get 0 i32.const 1 i32.add)
            (func (export \"main\") (param i32) (result i32) local.get 0 call $helper))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = call(&mut store, inst, "main", vec![Val::I32(41)]).unwrap();
    assert_eq!(r[0].unwrap_i32(), 42);
}

#[test]
fn imported_global_and_const_init() {
    let engine = Engine::default();
    let provider = module(
        &engine,
        "(module (global (export \"g\") i32 (i32.const 100)))",
    );
    let consumer = module(
        &engine,
        "(module
            (import \"p\" \"g\" (global i32))
            (global $d i32 (global.get 0))
            (func (export \"get\") (result i32) global.get $d))",
    );
    let mut store = Store::new(&engine, ());
    let pinst = Instance::new(&mut store, &provider, &[]).unwrap();
    let g = pinst.get_global(&mut store, "g").unwrap();
    let cinst = Instance::new(&mut store, &consumer, &[Extern::Global(g)]).unwrap();
    let r = call(&mut store, cinst, "get", vec![]).unwrap();
    assert_eq!(r[0].unwrap_i32(), 100);
}

#[test]
fn active_data_segment_loads() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (memory 1)
            (data (i32.const 8) \"\\2a\\00\\00\\00\")
            (func (export \"load\") (result i32) i32.const 8 i32.load))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = call(&mut store, inst, "load", vec![]).unwrap();
    assert_eq!(r[0].unwrap_i32(), 42);
}

const INDIRECT: &str = "(module
    (type $t (func (param i32) (result i32)))
    (type $t2 (func (result i32)))
    (table 2 funcref)
    (elem (i32.const 0) $f0 $g)
    (func $f0 (param i32) (result i32) local.get 0 i32.const 10 i32.add)
    (func $g (result i32) i32.const 7)
    (func (export \"call\") (param i32 i32) (result i32)
        local.get 1 local.get 0 call_indirect (type $t)))";

#[test]
fn call_indirect_dispatches() {
    let engine = Engine::default();
    let m = module(&engine, INDIRECT);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = call(&mut store, inst, "call", vec![Val::I32(0), Val::I32(5)]).unwrap();
    assert_eq!(r[0].unwrap_i32(), 15);
}

#[test]
fn call_indirect_traps() {
    let engine = Engine::default();
    let m = module(&engine, INDIRECT);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();

    let bad_sig = call(&mut store, inst, "call", vec![Val::I32(1), Val::I32(5)]);
    assert_eq!(trap_of(bad_sig), Trap::BadSignature);

    let oob = call(&mut store, inst, "call", vec![Val::I32(7), Val::I32(5)]);
    assert_eq!(trap_of(oob), Trap::TableOutOfBounds);
}

#[test]
fn memory_init_and_data_drop() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (memory 1)
            (data $d \"\\07\\00\\00\\00\")
            (func (export \"init\") (param i32 i32 i32)
                local.get 0 local.get 1 local.get 2 memory.init $d)
            (func (export \"drop\") data.drop $d)
            (func (export \"load\") (result i32) i32.const 0 i32.load))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();

    call(
        &mut store,
        inst,
        "init",
        vec![Val::I32(0), Val::I32(0), Val::I32(4)],
    )
    .unwrap();
    let loaded = call(&mut store, inst, "load", vec![]).unwrap();
    assert_eq!(loaded[0].unwrap_i32(), 7);

    call(&mut store, inst, "drop", vec![]).unwrap();
    let after_drop = call(
        &mut store,
        inst,
        "init",
        vec![Val::I32(0), Val::I32(0), Val::I32(4)],
    );
    assert_eq!(trap_of(after_drop), Trap::MemoryOutOfBounds);
}

const TABLE_BULK: &str = "(module
    (type $t (func (result i32)))
    (table 4 funcref)
    (elem func $a $b)
    (func $a (result i32) i32.const 10)
    (func $b (result i32) i32.const 20)
    (func (export \"init\") (param i32 i32 i32)
        local.get 0 local.get 1 local.get 2 table.init 0)
    (func (export \"copy\") (param i32 i32 i32)
        local.get 0 local.get 1 local.get 2 table.copy)
    (func (export \"drop\") elem.drop 0)
    (func (export \"call\") (param i32) (result i32) local.get 0 call_indirect (type $t)))";

#[test]
fn table_init_then_indirect_call() {
    let engine = Engine::default();
    let m = module(&engine, TABLE_BULK);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    call(
        &mut store,
        inst,
        "init",
        vec![Val::I32(0), Val::I32(0), Val::I32(2)],
    )
    .unwrap();
    assert_eq!(
        call(&mut store, inst, "call", vec![Val::I32(0)]).unwrap()[0].unwrap_i32(),
        10
    );
    assert_eq!(
        call(&mut store, inst, "call", vec![Val::I32(1)]).unwrap()[0].unwrap_i32(),
        20
    );
}

#[test]
fn table_copy_moves_elements() {
    let engine = Engine::default();
    let m = module(&engine, TABLE_BULK);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    call(
        &mut store,
        inst,
        "init",
        vec![Val::I32(0), Val::I32(0), Val::I32(2)],
    )
    .unwrap();
    // copy slot 1 ($b) to slot 2, then dispatch slot 2.
    call(
        &mut store,
        inst,
        "copy",
        vec![Val::I32(2), Val::I32(1), Val::I32(1)],
    )
    .unwrap();
    assert_eq!(
        call(&mut store, inst, "call", vec![Val::I32(2)]).unwrap()[0].unwrap_i32(),
        20
    );
}

#[test]
fn elem_drop_then_init_traps() {
    let engine = Engine::default();
    let m = module(&engine, TABLE_BULK);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    call(&mut store, inst, "drop", vec![]).unwrap();
    let after = call(
        &mut store,
        inst,
        "init",
        vec![Val::I32(0), Val::I32(0), Val::I32(2)],
    );
    assert_eq!(trap_of(after), Trap::TableOutOfBounds);
}

#[test]
fn table_init_out_of_bounds_traps() {
    let engine = Engine::default();
    let m = module(&engine, TABLE_BULK);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    // dst 3 + len 2 > table size 4.
    let r = call(
        &mut store,
        inst,
        "init",
        vec![Val::I32(3), Val::I32(0), Val::I32(2)],
    );
    assert_eq!(trap_of(r), Trap::TableOutOfBounds);
}

#[test]
fn unbounded_recursion_traps_stack_overflow() {
    let engine = Engine::default();
    let m = module(&engine, "(module (func $f (export \"run\") (call $f)))");
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = call(&mut store, inst, "run", vec![]);
    assert_eq!(trap_of(r), Trap::StackOverflow);
}

// --- fuel metering ---

use crate::Config;

/// `count(n)` loops n times and returns n; each iteration runs a fixed op count.
const COUNTER: &str = "(module (func (export \"count\") (param i32) (result i32)
    (local i32)
    (block $b (loop $l
        local.get 0 i32.eqz br_if $b
        local.get 0 i32.const 1 i32.sub local.set 0
        local.get 1 i32.const 1 i32.add local.set 1
        br $l))
    local.get 1))";

fn fuel_engine() -> Engine {
    let mut config = Config::new();
    config.consume_fuel(true);
    Engine::new(&config).unwrap()
}

#[test]
fn fuel_requires_config() {
    let mut store = Store::new(&Engine::default(), ());
    assert!(store.set_fuel(100).is_err());
    assert!(store.get_fuel().is_err());
}

#[test]
fn fuel_completes_and_decreases_deterministically() {
    let engine = fuel_engine();
    let m = module(&engine, COUNTER);

    let run_once = || {
        let mut store = Store::new(&engine, ());
        store.set_fuel(1_000_000).unwrap();
        let inst = Instance::new(&mut store, &m, &[]).unwrap();
        let r = call(&mut store, inst, "count", vec![Val::I32(20)]).unwrap();
        assert_eq!(r[0].unwrap_i32(), 20);
        store.get_fuel().unwrap()
    };
    let a = run_once();
    let b = run_once();
    assert!(a < 1_000_000); // fuel was consumed
    assert_eq!(a, b); // deterministic
}

#[test]
fn fuel_exhaustion_traps() {
    let engine = fuel_engine();
    let m = module(&engine, COUNTER);
    let mut store = Store::new(&engine, ());
    store.set_fuel(50).unwrap();
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = call(&mut store, inst, "count", vec![Val::I32(1_000_000)]);
    assert_eq!(trap_of(r), Trap::OutOfFuel);
}

// --- epoch interruption ---

use crate::UpdateDeadline;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SEVEN: &str = "(module (func (export \"f\") (result i32) i32.const 7))";

fn epoch_engine() -> Engine {
    let mut config = Config::new();
    config.epoch_interruption(true);
    Engine::new(&config).unwrap()
}

#[test]
fn epoch_deadline_not_reached_completes() {
    let engine = epoch_engine();
    let m = module(&engine, SEVEN);
    let mut store = Store::new(&engine, ());
    store.set_epoch_deadline(1); // deadline 1, current epoch 0
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        call(&mut store, inst, "f", vec![]).unwrap()[0].unwrap_i32(),
        7
    );
}

#[test]
fn epoch_deadline_reached_traps() {
    let engine = epoch_engine();
    let m = module(&engine, SEVEN);
    let mut store = Store::new(&engine, ());
    store.set_epoch_deadline(0); // current epoch already at the deadline
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        trap_of(call(&mut store, inst, "f", vec![])),
        Trap::Interrupt
    );
}

#[test]
fn epoch_callback_continue_resumes() {
    let engine = epoch_engine();
    let m = module(&engine, SEVEN);
    let mut store = Store::new(&engine, ());
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    store.epoch_deadline_callback(move |_| {
        c.fetch_add(1, Ordering::Relaxed);
        Ok(UpdateDeadline::Continue(1_000_000))
    });
    store.set_epoch_deadline(0);
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        call(&mut store, inst, "f", vec![]).unwrap()[0].unwrap_i32(),
        7
    );
    assert!(count.load(Ordering::Relaxed) >= 1);
}
