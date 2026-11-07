//! #24c: precompile / serialize round-trip — the compiled artifact restores and runs
//! without re-validating or recompiling, and incompatible blobs are rejected (no panic).

// The embedder legitimately calls the `unsafe` deserialize API in `unsafe {}` blocks.
#![allow(clippy::unwrap_used, unsafe_code)]

use submilli_wasm::{Engine, Instance, Module, Store, Val};

const ADD: &str = "(module (func (export \"add\") (param i32 i32) (result i32)
    local.get 0 local.get 1 i32.add))";

fn run_add(store: &mut Store<()>, inst: Instance) -> i32 {
    let add = inst.get_func(&mut *store, "add").unwrap();
    let mut out = [Val::I32(0)];
    add.call(store, &[Val::I32(40), Val::I32(2)], &mut out)
        .unwrap();
    out[0].unwrap_i32()
}

#[test]
fn serialize_deserialize_round_trip() {
    let engine = Engine::default();
    let module = Module::new(&engine, wat::parse_str(ADD).unwrap()).unwrap();
    let bytes = module.serialize().unwrap();

    // Restore without the original wasm — no validate, no recompile.
    let restored = unsafe { Module::deserialize(&engine, &bytes).unwrap() };
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &restored, &[]).unwrap();
    assert_eq!(run_add(&mut store, inst), 42);
}

#[test]
fn precompile_module_then_deserialize_runs() {
    let engine = Engine::default();
    let wasm = wat::parse_str(ADD).unwrap();
    let artifact = engine.precompile_module(&wasm).unwrap();

    let module = unsafe { Module::deserialize(&engine, &artifact).unwrap() };
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    assert_eq!(run_add(&mut store, inst), 42);
}

#[test]
fn deserialize_file_round_trip() {
    let engine = Engine::default();
    let module = Module::new(&engine, wat::parse_str(ADD).unwrap()).unwrap();
    let path = std::env::temp_dir().join("submilli_serialize_test.bin");
    std::fs::write(&path, module.serialize().unwrap()).unwrap();

    let restored = unsafe { Module::deserialize_file(&engine, &path).unwrap() };
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &restored, &[]).unwrap();
    assert_eq!(run_add(&mut store, inst), 42);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn deserialize_rejects_garbage() {
    let engine = Engine::default();
    // Wrong magic.
    assert!(unsafe { Module::deserialize(&engine, b"not an artifact") }.is_err());
    // Right magic, wrong version byte.
    let module = Module::new(&engine, wat::parse_str(ADD).unwrap()).unwrap();
    let mut bytes = module.serialize().unwrap();
    bytes[8] ^= 0xff; // corrupt the version field (just past the 8-byte magic)
    assert!(unsafe { Module::deserialize(&engine, &bytes) }.is_err());
}

/// Every single-byte corruption past the version (checksum or body) and every truncation is
/// rejected at decode — an error, never a panic or a module that misbehaves later.
#[test]
fn deserialize_rejects_corrupted_artifacts() {
    let engine = Engine::default();
    let module = Module::new(&engine, wat::parse_str(ADD).unwrap()).unwrap();
    let bytes = module.serialize().unwrap();
    for at in 12..bytes.len() {
        let mut bad = bytes.clone();
        bad[at] ^= 0x01;
        assert!(
            unsafe { Module::deserialize(&engine, &bad) }.is_err(),
            "flip at {at}"
        );
    }
    for len in 0..bytes.len() {
        let truncated = &bytes[..len];
        assert!(
            unsafe { Module::deserialize(&engine, truncated) }.is_err(),
            "length {len}"
        );
    }
}
