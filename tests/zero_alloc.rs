//! #33b: zero-on-allocation. No guest can read memory it didn't write — not a prior tenant's freed
//! bytes, not allocator residue. Every guest-visible allocation is zero/default-initialized by
//! construction (and uninit fast-paths are forbidden by the zero-`unsafe` invariant). These tests
//! lock that as a regression: grown memory reads zero, a fresh store's memory is zero, grown table
//! slots are null, and `*.new_default` aggregate fields read their type default.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Engine, Instance, Module, Store, Val};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

const MEM: &str = "(module
    (memory (export \"mem\") 1)
    (func (export \"grow\") (param i32) (result i32) (memory.grow (local.get 0)))
    (func (export \"store8\") (param i32 i32) (local.get 0) (local.get 1) (i32.store8))
    (func (export \"load8\") (param i32) (result i32) (local.get 0) (i32.load8_u)))";

fn call1(store: &mut Store<()>, inst: Instance, name: &str, arg: i32) -> i32 {
    let f = inst.get_func(&mut *store, name).unwrap();
    let mut out = [Val::I32(0)];
    f.call(store, &[Val::I32(arg)], &mut out).unwrap();
    out[0].unwrap_i32()
}

fn call0(store: &mut Store<()>, inst: Instance, name: &str) -> i32 {
    let f = inst.get_func(&mut *store, name).unwrap();
    let mut out = [Val::I32(0)];
    f.call(store, &[], &mut out).unwrap();
    out[0].unwrap_i32()
}

/// A page grown by `memory.grow` exposes only zeros, even after the previous page was dirtied.
#[test]
fn grown_memory_reads_zero() {
    let engine = Engine::default();
    let m = module(&engine, MEM);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();

    // Dirty the first page so spare capacity (if any) holds a non-zero pattern.
    let f = inst.get_func(&mut store, "store8").unwrap();
    for off in [0, 1, 65_535] {
        f.call(&mut store, &[Val::I32(off), Val::I32(0xFF)], &mut [])
            .unwrap();
    }
    assert_eq!(call1(&mut store, inst, "grow", 1), 1, "old size was 1 page");

    // Every byte of the new page reads zero.
    for off in [65_536, 100_000, 131_071] {
        assert_eq!(call1(&mut store, inst, "load8", off), 0, "offset {off}");
    }
}

/// A fresh store's memory reads zero even after a prior store on the same engine dirtied and dropped
/// its own (memory is never pooled/recycled across stores).
#[test]
fn fresh_store_memory_reads_zero() {
    let engine = Engine::default();
    let m = module(&engine, MEM);

    {
        let mut a = Store::new(&engine, ());
        let inst = Instance::new(&mut a, &m, &[]).unwrap();
        let f = inst.get_func(&mut a, "store8").unwrap();
        for off in [0, 7, 65_000] {
            f.call(&mut a, &[Val::I32(off), Val::I32(0xAB)], &mut [])
                .unwrap();
        }
    } // store A dropped

    let mut b = Store::new(&engine, ());
    let inst = Instance::new(&mut b, &m, &[]).unwrap();
    for off in [0, 7, 65_000] {
        assert_eq!(call1(&mut b, inst, "load8", off), 0, "offset {off}");
    }
}

/// Initial and `table.grow`-added table slots read `null` (funcref tables default to null).
#[test]
fn table_slots_default_null() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (table 1 funcref)
            (func (export \"init_null\") (result i32) (ref.is_null (table.get (i32.const 0))))
            (func (export \"grow_null\") (result i32)
                (drop (table.grow (ref.null func) (i32.const 1)))
                (ref.is_null (table.get (i32.const 1)))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(call0(&mut store, inst, "init_null"), 1);
    assert_eq!(call0(&mut store, inst, "grow_null"), 1);
}

/// `struct.new_default` / `array.new_default` initialize fields/elements to their type default (0).
#[test]
fn default_aggregates_read_defaults() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (type $s (struct (field i32) (field i64)))
            (type $a (array (mut i32)))
            (func (export \"struct_default\") (result i32)
                (struct.get $s 0 (struct.new_default $s)))
            (func (export \"array_default\") (result i32)
                (array.get $a (array.new_default $a (i32.const 4)) (i32.const 2))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(call0(&mut store, inst, "struct_default"), 0);
    assert_eq!(call0(&mut store, inst, "array_default"), 0);
}
