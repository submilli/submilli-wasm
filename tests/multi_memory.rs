//! #41: multiple memories — each memory op targets its own index; `memory.copy` can span two.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{Engine, Instance, Module, Store, Val};

fn geti32(store: &mut Store<()>, inst: Instance, name: &str, args: &[Val]) -> i32 {
    let f = inst.get_func(&mut *store, name).unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut *store, args, &mut out).unwrap();
    out[0].unwrap_i32()
}

#[test]
fn two_memories_are_isolated_and_copyable() {
    let engine = Engine::default();
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (memory 1) (memory 1)
                (func (export "store_b") (param i32 i32) local.get 0 local.get 1 (i32.store 1))
                (func (export "load_a") (param i32) (result i32) local.get 0 (i32.load 0))
                (func (export "load_b") (param i32) (result i32) local.get 0 (i32.load 1))
                (func (export "size_a") (result i32) (memory.size 0))
                (func (export "copy_a_from_b") (param i32 i32 i32)
                    local.get 0 local.get 1 local.get 2 (memory.copy 0 1)))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();

    // Write 42 into memory 1; memory 0 is untouched (isolation).
    inst.get_func(&mut store, "store_b")
        .unwrap()
        .call(&mut store, &[Val::I32(0), Val::I32(42)], &mut [])
        .unwrap();
    assert_eq!(geti32(&mut store, inst, "load_a", &[Val::I32(0)]), 0);
    assert_eq!(geti32(&mut store, inst, "load_b", &[Val::I32(0)]), 42);
    assert_eq!(geti32(&mut store, inst, "size_a", &[]), 1);

    // Copy 4 bytes from memory 1 into memory 0, then read memory 0.
    inst.get_func(&mut store, "copy_a_from_b")
        .unwrap()
        .call(
            &mut store,
            &[Val::I32(0), Val::I32(0), Val::I32(4)],
            &mut [],
        )
        .unwrap();
    assert_eq!(geti32(&mut store, inst, "load_a", &[Val::I32(0)]), 42);
}
