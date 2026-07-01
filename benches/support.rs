//! Shared harness for the cross-engine lifecycle benchmarks.
//!
//! Exposes the *same* pipeline — compile → store → instantiate → run — for
//! `submilli-wasm`, `wasmtime`, and `wasmi` so each phase is measured on equal
//! footing. Consumed by `benches/lifecycle.rs` (criterion) and
//! `examples/bench_table.rs` (the shareable summary table).
//!
//! The `Engine` is the long-lived, shared object (holds config/compiled-code
//! state) and is created in setup, never inside a measured phase.
#![allow(clippy::unwrap_used, dead_code, unreachable_pub)]

use std::time::{SystemTime, UNIX_EPOCH};

// Bytes are embedded so the harness is hermetic (no cwd / submodule needed).
pub const COREMARK: &[u8] = include_bytes!("wasm/coremark-minimal.wasm");
pub const PULLDOWN: &[u8] = include_bytes!("wasm/pulldown-cmark.wasm");
pub const SPIDERMONKEY: &[u8] = include_bytes!("wasm/spidermonkey.wasm");

/// The compile-time workloads: a tiny module and two large real ones. Compile
/// cost scales with module size, so the small/large split tells two stories.
pub const MODULES: &[(&str, &[u8])] = &[
    ("coremark", COREMARK),
    ("pulldown-cmark", PULLDOWN),
    ("spidermonkey", SPIDERMONKEY),
];

/// CoreMark's one import: `env.clock_ms () -> i64` (monotonic-ish millis).
/// CoreMark self-times against this and reports throughput as the score.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// One namespace per engine, all with an identical surface so callers stay
/// engine-agnostic. The bodies differ only where the wasmtime-compatible API
/// and wasmi diverge (`instantiate` vs `instantiate_and_start`, `&store` vs
/// `&mut store` on typed-func lookup).
macro_rules! engine_ops {
    ($name:ident, $krate:ident, $instantiate:ident, $typed_store:tt) => {
        pub mod $name {
            use $krate as w;
            pub type Engine = w::Engine;
            pub type Store = w::Store<()>;
            pub type Module = w::Module;
            pub type Linker = w::Linker<()>;
            pub type Instance = w::Instance;

            pub fn engine() -> Engine {
                w::Engine::default()
            }
            pub fn compile(e: &Engine, bytes: &[u8]) -> Module {
                w::Module::new(e, bytes).unwrap()
            }
            pub fn store(e: &Engine) -> Store {
                w::Store::new(e, ())
            }
            /// A linker pre-populated with CoreMark's `clock_ms` import.
            pub fn coremark_linker(e: &Engine) -> Linker {
                let mut l = w::Linker::new(e);
                l.func_wrap("env", "clock_ms", || super::now_ms()).unwrap();
                l
            }
            pub fn instantiate(l: &Linker, s: &mut Store, m: &Module) -> Instance {
                l.$instantiate(&mut *s, m).unwrap()
            }
            /// Call CoreMark's `run () -> f32`, returning the score.
            pub fn run_coremark(inst: &Instance, s: &mut Store) -> f32 {
                let f = inst
                    .get_typed_func::<(), f32>(engine_ops!(@store $typed_store, s), "run")
                    .unwrap();
                f.call(&mut *s, ()).unwrap()
            }
        }
    };
    (@store mut, $s:ident) => { &mut *$s };
    (@store shared, $s:ident) => { &*$s };
}

// wasmtime-compatible API: `Linker::instantiate`, typed-func lookup takes `&mut`.
engine_ops!(submilli, submilli_wasm, instantiate, mut);
engine_ops!(wt, wasmtime, instantiate, mut);
// wasmi: folds `start` into instantiation; typed-func lookup takes `&store`.
engine_ops!(wi, wasmi, instantiate_and_start, shared);
