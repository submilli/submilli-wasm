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
    ($name:ident, $krate:ident, $instantiate:ident, $typed_store:tt, $engine:expr) => {
        pub mod $name {
            use $krate as w;
            pub type Engine = w::Engine;
            pub type Store = w::Store<()>;
            pub type Module = w::Module;
            pub type Linker = w::Linker<()>;
            pub type Instance = w::Instance;

            pub fn engine() -> Engine {
                $engine
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
            /// A linker with no host functions, for import-free modules.
            pub fn empty_linker(e: &Engine) -> Linker {
                w::Linker::new(e)
            }
            /// Call CoreMark's `run () -> f32`, returning the score.
            pub fn run_coremark(inst: &Instance, s: &mut Store) -> f32 {
                let f = inst
                    .get_typed_func::<(), f32>(engine_ops!(@store $typed_store, s), "run")
                    .unwrap();
                f.call(&mut *s, ()).unwrap()
            }
            /// Call the run-once module's `run (i32) -> i32`.
            pub fn run_i32(inst: &Instance, s: &mut Store, arg: i32) -> i32 {
                let f = inst
                    .get_typed_func::<i32, i32>(engine_ops!(@store $typed_store, s), "run")
                    .unwrap();
                f.call(&mut *s, arg).unwrap()
            }
        }
    };
    (@store mut, $s:ident) => { &mut *$s };
    (@store shared, $s:ident) => { &*$s };
}

// wasmtime-compatible API: `Linker::instantiate`, typed-func lookup takes `&mut`.
// submilli runs with backtrace retention **off** (drops the per-op offsets table + name
// section) so the RAM rows compare pure compiled-code density against wasmi, which is
// configured to drop its custom-section retention below — apples to apples. The default
// (`wasm_backtrace` on, wasmtime-compatible) costs ~+20% module RAM and ~10% startup.
engine_ops!(submilli, submilli_wasm, instantiate, mut, {
    submilli_wasm::Engine::new(submilli_wasm::Config::new().wasm_backtrace(false)).unwrap()
});
// wasmtime runs Cranelift with **no optimization** (`OptLevel::None`) — the fair floor for a
// startup comparison, since optimization is exactly the compile-time cost this project skips.
engine_ops!(wt, wasmtime, instantiate, mut, {
    let mut c = wasmtime::Config::new();
    c.cranelift_opt_level(wasmtime::OptLevel::None);
    wasmtime::Engine::new(&c).unwrap()
});
// wasmi runs with **eager compilation** — its default (`CompilationMode::LazyTranslation`)
// validates but defers per-function translation to first call, which would make its
// `Module::new` a validate-only number next to the other engines' full compiles.
// Also: folds `start` into instantiation; typed-func lookup takes `&store`.
engine_ops!(wi, wasmi, instantiate_and_start, shared, {
    let mut c = wasmi::Config::default();
    c.compilation_mode(wasmi::CompilationMode::Eager);
    // Drop custom-section retention (name etc.), matching submilli's no-retention config
    // above — by default wasmi keeps raw custom sections in the module (pulldown-cmark's
    // name section alone is ~1.6 MiB, which would inflate its Module RAM cell).
    c.ignore_custom_sections(true);
    wasmi::Engine::new(&c)
});
/// Synthetic fixture for the run-once benchmark: the "LLM-generated code that
/// runs once" shape. ~1200 distinct small helper functions in call chains of
/// 100 (each function calls the next; `run` calls every chain head in turn),
/// so executing `run` touches *all* of the module's code — generated code is
/// written to be used, not shipped cold, which also means lazy-translation
/// engines pay their deferred work inside the timed window. Chains are capped
/// at 100 deep to stay well under every engine's default recursion limit.
/// A prime-sieve kernel is exported as `run (i32 n) -> i32` (count of primes
/// below `n`; `run` walks the chains first). No imports, so every engine
/// instantiates it with an empty linker. Assembled from WAT once in setup —
/// never inside a timed phase.
pub fn run_once_wasm() -> Vec<u8> {
    use std::fmt::Write as _;
    const BULK_FUNCS: i32 = 1200;
    const CHAIN: i32 = 100;
    let mut w = String::from("(module\n  (memory (export \"memory\") 16)\n");
    for i in 0..BULK_FUNCS {
        let (a, b, s) = (i % 97 + 3, i % 251 + 1, i % 13 + 1);
        let chain = if (i + 1) % CHAIN != 0 && i + 1 < BULK_FUNCS {
            format!("(call $f{} (local.get $t))", i + 1)
        } else {
            "(local.get $t)".to_string()
        };
        write!(
            w,
            "  (func $f{i} (param $x i32) (result i32) (local $t i32)\n    \
             (local.set $t (i32.add (i32.mul (local.get $x) (i32.const {a})) (i32.const {b})))\n    \
             (local.set $t (i32.xor (local.get $t) (i32.shr_u (local.get $t) (i32.const {s}))))\n    \
             (i32.add (i32.and {chain} (i32.const 16777215)) (i32.rotl (local.get $x) (i32.const {s}))))\n",
        )
        .unwrap();
    }
    // Sieve of Eratosthenes over one byte per candidate (memory 16 pages = 1 MiB,
    // so `n` up to 1_000_000 stays in bounds; fresh instances start zeroed).
    w.push_str(
        "  (func $sieve (param $n i32) (result i32) (local $i i32) (local $j i32) (local $count i32)\n\
         (local.set $i (i32.const 2))\n\
         (block $sieved (loop $outer\n\
           (br_if $sieved (i32.gt_u (i32.mul (local.get $i) (local.get $i)) (local.get $n)))\n\
           (if (i32.eqz (i32.load8_u (local.get $i))) (then\n\
             (local.set $j (i32.mul (local.get $i) (local.get $i)))\n\
             (block $marked (loop $mark\n\
               (br_if $marked (i32.ge_u (local.get $j) (local.get $n)))\n\
               (i32.store8 (local.get $j) (i32.const 1))\n\
               (local.set $j (i32.add (local.get $j) (local.get $i)))\n\
               (br $mark)))))\n\
           (local.set $i (i32.add (local.get $i) (i32.const 1)))\n\
           (br $outer)))\n\
         (local.set $i (i32.const 2))\n\
         (block $counted (loop $tally\n\
           (br_if $counted (i32.ge_u (local.get $i) (local.get $n)))\n\
           (local.set $count (i32.add (local.get $count) (i32.xor (i32.load8_u (local.get $i)) (i32.const 1))))\n\
           (local.set $i (i32.add (local.get $i) (i32.const 1)))\n\
           (br $tally)))\n\
         (local.get $count))\n\
         (func (export \"run\") (param $n i32) (result i32)\n",
    );
    // `run` walks every chain head, executing all of the module's code, then sieves.
    for head in (0..BULK_FUNCS).step_by(CHAIN as usize) {
        writeln!(w, "    (drop (call $f{head} (local.get $n)))").unwrap();
    }
    w.push_str("    (call $sieve (local.get $n)))\n)\n");
    wat::parse_str(&w).unwrap()
}

/// `(label, sieve bound, expected prime count, best-of iters)` rows for the
/// run-once benchmark, spanning the execution-weight curve: trivial execution
/// (startup-dominated — the fast-startup sweet spot), a light-but-real run,
/// and a heavy one (execution-dominated — where fast runtimes catch back up).
pub const RUN_ONCE: &[(&str, i32, i32, u32)] = &[
    ("sieve(1k)", 1_000, 168, 40),
    ("sieve(10k)", 10_000, 1_229, 40),
    ("sieve(1M)", 1_000_000, 78_498, 8),
];

/// Heap accounting for the density story: a counting wrapper around the system allocator,
/// installed as the `#[global_allocator]` of every bench binary that includes this module.
/// All three engines allocate through it in-process, so `live()` deltas give an
/// engine-agnostic "retained bytes" figure for a compiled module. Known blind spot:
/// wasmtime's JIT code is mmap'd outside the Rust allocator, so its cells *undercount*
/// (the caveat is documented in the README).
#[allow(unsafe_code)] // a GlobalAlloc impl is unavoidably unsafe; it only counts + delegates
pub mod mem {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};

    #[derive(Debug)]
    pub struct Tracking;

    /// Counting is off by default: the counter cache line is contended under parallel
    /// allocation (wasmtime's Cranelift threads read ~3× slower with it live), so the
    /// timing rows must run with only this read-mostly flag on the alloc path.
    static ENABLED: AtomicBool = AtomicBool::new(false);
    /// Live-byte balance while enabled. `isize`: frees of allocations made while the
    /// counter was off legitimately drive it negative — only same-window deltas matter.
    static LIVE: AtomicIsize = AtomicIsize::new(0);

    // SAFETY: pure delegation to `System`; the atomics only observe sizes.
    unsafe impl GlobalAlloc for Tracking {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() && ENABLED.load(Relaxed) {
                LIVE.fetch_add(l.size() as isize, Relaxed);
            }
            p
        }

        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc_zeroed(l) };
            if !p.is_null() && ENABLED.load(Relaxed) {
                LIVE.fetch_add(l.size() as isize, Relaxed);
            }
            p
        }

        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) };
            if ENABLED.load(Relaxed) {
                LIVE.fetch_sub(l.size() as isize, Relaxed);
            }
        }

        unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
            let q = unsafe { System.realloc(p, l, new_size) };
            if !q.is_null() && ENABLED.load(Relaxed) {
                LIVE.fetch_add(new_size as isize - l.size() as isize, Relaxed);
            }
            q
        }
    }

    /// Currently live heap bytes (meaningful as same-window deltas only).
    pub fn live() -> isize {
        LIVE.load(Relaxed)
    }

    pub fn set_enabled(on: bool) {
        ENABLED.store(on, Relaxed);
    }
}

/// Live-heap delta retained by `f()`'s return value: warms engine-lazy state with one
/// discarded call first, then measures allocation across a second. The result is held
/// until after the reading so its memory is counted, then dropped. Counting is enabled
/// only inside this window (see [`mem::Tracking`]).
pub fn retained_ram<T>(f: impl Fn() -> T) -> usize {
    mem::set_enabled(true);
    drop(f()); // warm-up: engine caches / lazy init don't count against the module
    let before = mem::live();
    let keep = f();
    let after = mem::live();
    drop(keep);
    mem::set_enabled(false);
    usize::try_from(after - before).unwrap_or(0)
}
