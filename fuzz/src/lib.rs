//! Shared logic for the `cargo-fuzz` targets (#35). The `fuzz_targets/*.rs` binaries are one-line
//! wrappers over [`validate`], [`interpret`], and [`differential`]; keeping the real code here lets it
//! be type-checked on stable (`cargo check --lib`) without the nightly libFuzzer harness.

use arbitrary::Unstructured;

/// Fuel cap per call — bounds any guest loop so it traps `OutOfFuel` instead of hanging the fuzzer.
const FUEL: u64 = 200_000;
/// Wasm stack budget (bytes) — bounds recursion to `StackOverflow` rather than a native abort.
const STACK: usize = 256 * 1024;

/// A `wasm-smith` config mirroring the interpreter's `enabled_features()` minus the proposals we don't
/// *execute* (tail-call — `return_call*` is deferred, #39) or that hurt determinism/speed (SIMD +
/// relaxed-SIMD, threads, custom page sizes, wide arithmetic). `canonicalize_nans` makes float results
/// comparable across engines; `max_imports = 0` keeps every generated module instantiable with `&[]`.
#[allow(clippy::field_reassign_with_default)] // field-by-field reads far clearer than a 25-field literal
fn smith_config() -> wasm_smith::Config {
    let mut c = wasm_smith::Config::default();

    // Not executed / nondeterministic / out of scope.
    c.simd_enabled = false;
    c.relaxed_simd_enabled = false;
    c.tail_call_enabled = false;
    c.threads_enabled = false;
    c.shared_everything_threads_enabled = false;
    c.wide_arithmetic_enabled = false;
    c.custom_descriptors_enabled = false;
    c.custom_page_sizes_enabled = false;

    // Matches `enabled_features()` — `gc_enabled` also implies function-references in wasm-smith.
    c.gc_enabled = true;
    c.exceptions_enabled = true;
    c.reference_types_enabled = true;
    c.bulk_memory_enabled = true;
    c.multi_value_enabled = true;
    c.sign_extension_ops_enabled = true;
    c.saturating_float_to_int_enabled = true;
    c.extended_const_enabled = true;
    c.memory64_enabled = true;
    c.max_memories = 2; // > 1 ⇒ multi-memory
    c.max_tables = 2;

    // Fuzzing ergonomics: comparable floats, no imports, bounded memory so instantiation is fast.
    c.canonicalize_nans = true;
    c.min_imports = 0;
    c.max_imports = 0;
    c.max_memory32_bytes = 1 << 20;
    c.max_memory64_bytes = 1 << 20;
    c
}

/// Generates a valid wasm module from fuzz bytes (`None` when the bytes are exhausted).
fn gen_wasm(data: &[u8]) -> Option<Vec<u8>> {
    let mut u = Unstructured::new(data);
    let module = wasm_smith::Module::new(smith_config(), &mut u).ok()?;
    Some(module.to_bytes())
}

// ---------------------------------------------------------------------------------------------------
// Target (a): validator/compiler — arbitrary bytes must never panic, only `Err`.
// ---------------------------------------------------------------------------------------------------

/// Feeds arbitrary bytes to both the fused validate+compile path and the validate-only path. A panic
/// is a bug (libFuzzer reports it); an `Err` is the expected outcome for malformed input.
pub fn validate(data: &[u8]) {
    let engine = submilli_wasm::Engine::default();
    let _ = submilli_wasm::Module::new(&engine, data);
    let _ = submilli_wasm::Module::validate(&engine, data);
}

// ---------------------------------------------------------------------------------------------------
// Target (b): interpreter — a valid module must instantiate + run without panic/hang.
// ---------------------------------------------------------------------------------------------------

/// Generates a module and runs every exported function under a fuel cap. A compile/instantiate `Err`
/// (a deferred op, a trapping `start`, …) is a skip, not a bug; the contract is *no panic, no hang*.
pub fn interpret(data: &[u8]) {
    let Some(wasm) = gen_wasm(data) else { return };
    run_submilli(&wasm);
}

fn run_submilli(wasm: &[u8]) {
    use submilli_wasm::{Config, Engine, ExternType, Instance, Module, Store, Val};

    let mut config = Config::new();
    config.consume_fuel(true).max_wasm_stack(STACK);
    let Ok(engine) = Engine::new(&config) else {
        return;
    };
    let Ok(module) = Module::new(&engine, wasm) else {
        return;
    };
    let mut store = Store::new(&engine, ());
    if store.set_fuel(FUEL).is_err() {
        return;
    }
    let Ok(instance) = Instance::new(&mut store, &module, &[]) else {
        return;
    };

    let names: Vec<String> = module
        .exports()
        .filter(|e| matches!(e.ty(), ExternType::Func(_)))
        .map(|e| e.name().to_string())
        .collect();
    for name in names {
        let _ = store.set_fuel(FUEL); // refuel so one long call doesn't starve the rest
        let Some(func) = instance.get_func(&mut store, &name) else {
            continue;
        };
        let ty = func.ty(&store);
        let params: Vec<Val> = ty.params().map(sm_zero).collect();
        let mut results = vec![Val::I32(0); ty.results().len()];
        let _ = func.call(&mut store, &params, &mut results); // Ok or any Trap is fine; panic is not
    }
}

// ---------------------------------------------------------------------------------------------------
// Target (c): differential vs wasmtime — same module + numeric args, compare results / trap category.
// ---------------------------------------------------------------------------------------------------

/// A normalized scalar return value, comparable across engines (floats are canonical via
/// `canonicalize_nans`, so the bits compare directly).
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum Norm {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

/// One export's outcome, reduced to what is safely comparable between engines.
#[derive(PartialEq, Eq, Debug)]
enum Outcome {
    Returned(Vec<Norm>),
    Trapped,
    /// Not comparable: a non-trap error, a halting trap (`OutOfFuel`/`Interrupt` — the two engines'
    /// fuel cost models differ), or a non-numeric signature.
    Skip,
}

/// Runs the module on both submilli and wasmtime and asserts they don't diverge.
pub fn differential(data: &[u8]) {
    let Some(wasm) = gen_wasm(data) else { return };
    let ours = diff_submilli(&wasm);
    let theirs = diff_wasmtime(&wasm);
    let theirs: std::collections::HashMap<String, Outcome> = theirs.into_iter().collect();
    for (name, a) in ours {
        let Some(b) = theirs.get(&name) else { continue };
        match (&a, b) {
            (Outcome::Skip, _) | (_, Outcome::Skip) => {}
            (Outcome::Trapped, Outcome::Trapped) => {}
            (Outcome::Returned(x), Outcome::Returned(y)) => assert_eq!(
                x, y,
                "differential value mismatch on `{name}`: submilli={x:?} wasmtime={y:?}"
            ),
            _ => panic!("differential divergence on `{name}`: submilli={a:?} wasmtime={b:?}"),
        }
    }
}

fn diff_submilli(wasm: &[u8]) -> Vec<(String, Outcome)> {
    use submilli_wasm::{Config, Engine, Instance, Module, Store, Trap, Val};

    let mut config = Config::new();
    config.consume_fuel(true).max_wasm_stack(STACK);
    let Ok(engine) = Engine::new(&config) else {
        return vec![];
    };
    let Ok(module) = Module::new(&engine, wasm) else {
        return vec![];
    };
    let mut store = Store::new(&engine, ());
    if store.set_fuel(FUEL).is_err() {
        return vec![];
    }
    let Ok(instance) = Instance::new(&mut store, &module, &[]) else {
        return vec![];
    };

    let mut out = Vec::new();
    for name in numeric_func_exports_sm(&module) {
        let _ = store.set_fuel(FUEL);
        let Some(func) = instance.get_func(&mut store, &name) else {
            continue;
        };
        let ty = func.ty(&store);
        let params: Vec<Val> = ty.params().map(sm_zero).collect();
        let mut results = vec![Val::I32(0); ty.results().len()];
        let outcome = match func.call(&mut store, &params, &mut results) {
            Ok(()) => results
                .iter()
                .map(sm_norm)
                .collect::<Option<Vec<_>>>()
                .map_or(Outcome::Skip, Outcome::Returned),
            Err(e) => match e.downcast_ref::<Trap>() {
                Some(&t) if t == Trap::OutOfFuel || t == Trap::Interrupt => Outcome::Skip,
                Some(_) => Outcome::Trapped,
                None => Outcome::Skip,
            },
        };
        out.push((name, outcome));
    }
    out
}

fn diff_wasmtime(wasm: &[u8]) -> Vec<(String, Outcome)> {
    use wasmtime::{Config, Engine, Instance, Module, Store, Trap, Val};

    let mut config = Config::new();
    config.consume_fuel(true);
    config
        .wasm_reference_types(true)
        .wasm_function_references(true)
        .wasm_gc(true)
        .wasm_exceptions(true)
        .wasm_memory64(true)
        .wasm_multi_memory(true)
        .wasm_tail_call(false)
        .wasm_relaxed_simd(false)
        .wasm_simd(false);
    let Ok(engine) = Engine::new(&config) else {
        return vec![];
    };
    let Ok(module) = Module::new(&engine, wasm) else {
        return vec![];
    };
    let mut store = Store::new(&engine, ());
    if store.set_fuel(FUEL).is_err() {
        return vec![];
    }
    // Wasmtime 45 workaround: instantiating a module whose passive element segment holds a
    // non-null externref const-expr (e.g. `extern.convert_any (ref.i31 ...)`) panics if the
    // store's lazily-created GC heap doesn't exist yet. Allocating (and instantly unrooting)
    // a throwaway externref forces the heap into existence first.
    {
        let mut scope = wasmtime::RootScope::new(&mut store);
        if wasmtime::ExternRef::new(&mut scope, ()).is_err() {
            return vec![];
        }
    }
    let Ok(instance) = Instance::new(&mut store, &module, &[]) else {
        return vec![];
    };

    let mut out = Vec::new();
    for ty in module.exports().collect::<Vec<_>>() {
        let wasmtime::ExternType::Func(ft) = ty.ty() else {
            continue;
        };
        if !ft.params().all(wt_is_numeric) || !ft.results().all(wt_is_numeric) {
            continue;
        }
        let name = ty.name().to_string();
        let _ = store.set_fuel(FUEL);
        let Some(func) = instance.get_func(&mut store, &name) else {
            continue;
        };
        let fty = func.ty(&store);
        let params: Vec<Val> = fty.params().map(wt_zero).collect();
        let mut results = vec![Val::I32(0); fty.results().len()];
        let outcome = match func.call(&mut store, &params, &mut results) {
            Ok(()) => results
                .iter()
                .map(wt_norm)
                .collect::<Option<Vec<_>>>()
                .map_or(Outcome::Skip, Outcome::Returned),
            Err(e) => match e.downcast_ref::<Trap>() {
                Some(&t) if t == Trap::OutOfFuel || t == Trap::Interrupt => Outcome::Skip,
                Some(_) => Outcome::Trapped,
                None => Outcome::Skip,
            },
        };
        out.push((name, outcome));
    }
    out
}

// --- per-engine value helpers ----------------------------------------------------------------------

fn sm_zero(t: submilli_wasm::ValType) -> submilli_wasm::Val {
    use submilli_wasm::{Val, ValType, V128};
    match t {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        ValType::V128 => Val::V128(V128::from(0)),
        ValType::Ref(_) => Val::null_func_ref(),
    }
}

fn sm_norm(v: &submilli_wasm::Val) -> Option<Norm> {
    use submilli_wasm::Val;
    Some(match v {
        Val::I32(x) => Norm::I32(*x),
        Val::I64(x) => Norm::I64(*x),
        Val::F32(x) => Norm::F32(*x),
        Val::F64(x) => Norm::F64(*x),
        _ => return None,
    })
}

fn sm_is_numeric(t: &submilli_wasm::ValType) -> bool {
    use submilli_wasm::ValType;
    matches!(t, ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64)
}

fn numeric_func_exports_sm(module: &submilli_wasm::Module) -> Vec<String> {
    use submilli_wasm::ExternType;
    module
        .exports()
        .filter_map(|e| match e.ty() {
            ExternType::Func(ft)
                if ft.params().all(|t| sm_is_numeric(&t))
                    && ft.results().all(|t| sm_is_numeric(&t)) =>
            {
                Some(e.name().to_string())
            }
            _ => None,
        })
        .collect()
}

fn wt_zero(t: wasmtime::ValType) -> wasmtime::Val {
    use wasmtime::{Val, ValType};
    match t {
        ValType::I32 => Val::I32(0),
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        ValType::V128 => Val::V128(0u128.into()),
        ValType::Ref(_) => Val::null_func_ref(),
    }
}

fn wt_norm(v: &wasmtime::Val) -> Option<Norm> {
    use wasmtime::Val;
    Some(match v {
        Val::I32(x) => Norm::I32(*x),
        Val::I64(x) => Norm::I64(*x),
        Val::F32(x) => Norm::F32(*x),
        Val::F64(x) => Norm::F64(*x),
        _ => return None,
    })
}

fn wt_is_numeric(t: wasmtime::ValType) -> bool {
    use wasmtime::ValType;
    matches!(t, ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64)
}

/// Smoke tests that exercise the three entry points on stable (the libFuzzer binaries need nightly).
/// They run the *actual* harness logic — including the differential comparison over many generated
/// modules — so a real submilli/wasmtime divergence or a false-positive in the comparison surfaces here.
#[cfg(test)]
mod tests {
    use super::{differential, interpret, validate};

    /// A varied byte buffer so wasm-smith produces a non-trivial module (with functions to run/compare).
    fn seed_bytes(seed: u64) -> Vec<u8> {
        let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        (0..2048)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn validate_never_panics() {
        let cases: &[&[u8]] = &[
            &[],
            b"\0asm",
            &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
            b"not wasm at all",
            &[0xff; 256],
        ];
        for c in cases {
            validate(c);
        }
        for s in 0..16 {
            validate(&seed_bytes(s));
        }
    }

    #[test]
    fn interpret_runs_generated_modules() {
        for s in 0..64 {
            interpret(&seed_bytes(s));
        }
    }

    /// CI crash regression: these bytes smith a module whose passive element segment holds an
    /// `extern.convert_any (ref.i31 ...)` const-expr, which panicked wasmtime 45's instantiation
    /// when the store's GC heap wasn't allocated yet (worked around in `diff_wasmtime`).
    #[test]
    fn differential_passive_externref_elem_does_not_panic_wasmtime() {
        differential(&[170, 175, 0, 0, 0, 0, 47, 47, 0, 23, 89, 193, 193]);
    }

    // Heavyweight (wasmtime debug-Cranelift-compiles every generated module), so it's `#[ignore]`d out
    // of routine `cargo test`; run with `cargo test -- --ignored` for a manual differential pass.
    #[test]
    #[ignore = "slow: debug-compiles each module through wasmtime/Cranelift"]
    fn differential_agrees_with_wasmtime() {
        // Compiles + runs each module on both engines and asserts no divergence (panics on mismatch).
        for s in 0u64..16 {
            differential(&seed_bytes(s.wrapping_mul(2_654_435_761)));
        }
    }
}
