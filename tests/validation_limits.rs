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
        ..ModuleLimits::default()
    };
    let module = Module::new_with_limits(&engine, &wasm, &limits).unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    assert_eq!(run_add(&mut store, inst), 42);
}

/// `n_funcs` functions of `n_locals` i32 locals each — every one well under `wasmparser`'s
/// per-function cap, so only the module-wide aggregate can reject it.
fn many_locals(n_funcs: usize, n_locals: usize) -> Vec<u8> {
    let func = format!("(func (local {}))", "i32 ".repeat(n_locals));
    wat::parse_str(format!("(module {})", func.repeat(n_funcs))).unwrap()
}

/// The aggregate expanded-locals cap (`Config::max_expanded_locals`) rejects a module whose
/// functions are individually valid but together exceed it; exactly at the cap is accepted, and
/// the trusted-tier `ModuleLimits` override raises it per-module.
#[test]
fn aggregate_expanded_locals_are_bounded() {
    let mut config = Config::new();
    config.max_expanded_locals(30);
    let engine = Engine::new(&config).unwrap();

    let over = many_locals(4, 10);
    let err = Module::new(&engine, &over).unwrap_err();
    assert!(
        err.to_string().contains("more than 30 locals in total"),
        "{err}"
    );

    assert!(Module::new(&engine, many_locals(3, 10)).is_ok());
    assert!(Module::new(&engine, wat::parse_str(ADD).unwrap()).is_ok());

    let limits = ModuleLimits {
        max_expanded_locals: 40,
        ..ModuleLimits::default()
    };
    assert!(Module::new_with_limits(&engine, &over, &limits).is_ok());
}

/// A hostile module under the default caps: the maximum per-function locals `wasmparser`
/// allows, repeated until the aggregate crosses the default — a few hundred KiB of input that
/// would otherwise expand to hundreds of MiB — is rejected cleanly by `Module::new`.
#[test]
fn default_cap_rejects_locals_bomb() {
    let wasm = many_locals(200, 50_000);
    let err = Module::new(&Engine::default(), &wasm).unwrap_err();
    assert!(err.to_string().contains("locals in total"), "{err}");
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

/// A function body from repeated pieces: `(func (export "f") (param i32 i32 i32) <body>)`.
fn function_module(body: &str) -> Vec<u8> {
    let wat = format!("(module (func (export \"f\") (param i32 i32 i32) {body}))");
    wat::parse_str(wat).unwrap()
}

/// Step 2b's operand providers keep compile time linear on hostile stack shapes: a live
/// `local.get` deep in the stack must not make later block entries or local writes rescan it.
/// (Quadratic, each of these took seconds in a release build; linear, they take milliseconds.)
#[test]
fn hostile_operand_stacks_compile_in_linear_time() {
    let (h, n) = (40_000, 40_000);
    let bodies = [
        // Block entries while one local entry is live above a deep stack of them.
        format!(
            "{}{}{}",
            "local.get 0 ".repeat(h),
            "local.get 0 block end drop ".repeat(n),
            "drop ".repeat(h)
        ),
        // Local writes while a local entry sits under a deep stack.
        format!(
            "local.get 0 {}{}unreachable",
            "i32.const 0 ".repeat(h),
            "local.get 1 local.set 2 i32.const 0 local.set 1 ".repeat(n)
        ),
    ];
    let engine = Engine::default();
    for body in &bodies {
        let start = std::time::Instant::now();
        Module::new(&engine, function_module(body)).unwrap();
        let took = start.elapsed();
        assert!(took.as_secs() < 10, "compile took {took:?}");
    }
}

/// Dead code holds no operand entries: a block ending in dead code must not grow the frame (or
/// the compiler's memory) by its results. Each dead block below would have added 1000 slots.
#[test]
fn dead_code_does_not_inflate_the_frame() {
    let results = " i32".repeat(1000);
    let wat = format!(
        "(module (type $t (func (result{results}))) (func (export \"f\") unreachable {}))",
        "(block (type $t) unreachable) br 0 ".repeat(1000)
    );
    let mut config = Config::new();
    config.max_wasm_stack(64 << 10);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, wat::parse_str(wat).unwrap()).unwrap();
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let f = inst.get_func(&mut store, "f").unwrap();
    let err = f.call(&mut store, &[], &mut []).unwrap_err();
    assert_eq!(
        *err.downcast_ref::<submilli_wasm::Trap>().unwrap(),
        submilli_wasm::Trap::UnreachableCodeReached
    );
}

/// A block in dead code may take params from the polymorphic stack, below anything pushed.
#[test]
fn dead_code_param_blocks_compile() {
    let engine = Engine::default();
    for body in [
        "unreachable (block (param i32) drop) i32.const 0",
        "unreachable (loop (param i32 i32) drop drop) i32.const 0",
        "unreachable (if (param i32) (then drop) (else drop)) i32.const 0",
    ] {
        let wat = format!("(module (func (result i32) {body}))");
        Module::new(&engine, wat::parse_str(&wat).unwrap())
            .unwrap_or_else(|e| panic!("{body}: {e}"));
    }
}
