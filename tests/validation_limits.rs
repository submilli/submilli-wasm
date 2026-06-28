//! #32: validation-time limits — a hostile *module* can't OOM the compiler before it executes.
//!
//! `wasmparser` enforces the per-dimension ceilings (function body size, locals, segment/type/
//! function counts); these tests cover the configurable aggregate `max_module_bytes` cap layered
//! on top (the two trust tiers), confirm the per-dimension limits still reject as `Err` (not a
//! panic), and check arbitrary bytes never panic.

// The embedder legitimately calls the `unsafe` deserialize API in `unsafe {}` blocks.
#![allow(clippy::unwrap_used, unsafe_code)]

use submilli_wasm::{Config, Engine, Instance, Module, ModuleLimits, Store, Val};

const ADD: &str = "(module (func (export \"add\") (param i32 i32) (result i32)
    local.get 0 local.get 1 i32.add))";

fn engine_with_cap(cap: usize) -> Engine {
    let mut config = Config::new();
    config.max_module_bytes(cap);
    Engine::new(&config).unwrap()
}

fn run_add(store: &mut Store<()>, inst: Instance) -> i32 {
    let add = inst.get_func(&mut *store, "add").unwrap();
    let mut out = [Val::I32(0)];
    add.call(store, &[Val::I32(40), Val::I32(2)], &mut out)
        .unwrap();
    out[0].unwrap_i32()
}

/// The untrusted-tier cap (`Config::max_module_bytes`) rejects an oversize module at both
/// `Module::new` and `Module::validate`; exactly at the cap is accepted.
#[test]
fn engine_default_cap_bounds_module_size() {
    let wasm = wat::parse_str(ADD).unwrap();
    let n = wasm.len();

    let tight = engine_with_cap(n - 1);
    let err = Module::new(&tight, &wasm).unwrap_err();
    assert!(
        err.to_string().contains("exceeds configured limit"),
        "unexpected error: {err}"
    );
    assert!(Module::validate(&tight, &wasm).is_err());

    // The check is `len > cap`, so a module exactly at the cap compiles.
    let exact = engine_with_cap(n);
    assert!(Module::new(&exact, &wasm).is_ok());
    assert!(Module::validate(&exact, &wasm).is_ok());
}

/// The trusted tier: a curated package that exceeds the engine's untrusted default still compiles
/// (and runs) via `Module::new_with_limits` on the *same* engine — proving the per-module override.
#[test]
fn trusted_per_module_override_raises_ceiling() {
    let wasm = wat::parse_str(ADD).unwrap();
    let n = wasm.len();

    let engine = engine_with_cap(n - 1);
    assert!(Module::new(&engine, &wasm).is_err());

    let limits = ModuleLimits {
        max_module_bytes: n,
    };
    let module = Module::new_with_limits(&engine, &wasm, &limits).unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    assert_eq!(run_add(&mut store, inst), 42);
}

/// `wasmparser`'s per-dimension hard limits still reject a hostile module in our streaming path —
/// as a clean `Err`, never a panic/OOM. Here: more locals than `MAX_WASM_FUNCTION_LOCALS` (50 000).
#[test]
fn per_dimension_limits_still_reject() {
    let engine = Engine::default(); // generous default cap — the locals limit is what fires
    let wat = format!("(module (func (local {})))", "i32 ".repeat(50_001));
    let wasm = wat::parse_str(&wat).unwrap();
    assert!(Module::new(&engine, &wasm).is_err());
    assert!(Module::validate(&engine, &wasm).is_err());
}

/// Arbitrary / truncated bytes never panic — they error (or, for a valid empty module, succeed).
/// A smoke check ahead of the Phase-8 validator fuzzer.
#[test]
fn arbitrary_bytes_error_not_panic() {
    let engine = Engine::default();
    let cases: &[&[u8]] = &[
        &[],
        b"\0asm",                                          // truncated magic
        &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00], // header only — a valid empty module
        b"garbage bytes not wasm",
        &[0xff; 64],
    ];
    for bytes in cases {
        // The contract is "no panic"; reaching the assert means we returned a Result.
        let _ = Module::new(&engine, bytes);
        let _ = Module::validate(&engine, bytes);
    }
    // The clearly-invalid ones are rejected.
    assert!(Module::new(&engine, b"garbage bytes not wasm").is_err());
    assert!(Module::new(&engine, [0xffu8; 64]).is_err());
}

/// The size cap is a guest/source-path defense; the trusted-artifact `deserialize` path is exempt
/// (matching wasmtime's `unsafe` contract). A tightly-capped engine rejects the source wasm but
/// still restores and runs its precompiled artifact.
#[test]
fn deserialize_not_subject_to_size_cap() {
    let wasm = wat::parse_str(ADD).unwrap();
    let artifact = Module::new(&Engine::default(), &wasm)
        .unwrap()
        .serialize()
        .unwrap();

    let tight = engine_with_cap(wasm.len() - 1);
    assert!(Module::new(&tight, &wasm).is_err());

    let restored = unsafe { Module::deserialize(&tight, &artifact).unwrap() };
    let mut store = Store::new(&tight, ());
    let inst = Instance::new(&mut store, &restored, &[]).unwrap();
    assert_eq!(run_add(&mut store, inst), 42);
}
