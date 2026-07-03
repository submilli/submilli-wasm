//! Focused profiling harness: hammers `Module::new(spidermonkey)` in a timed loop
//! so a Time Profiler run has a hot, symbol-rich target.
//! Usage: prof_module_new [submilli|wasmi] [spidermonkey|pulldown] [secs]
//! Temporary — used for the compile-speed spike; safe to delete.
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

const SPIDERMONKEY: &[u8] = include_bytes!("../benches/wasm/spidermonkey.wasm");
const PULLDOWN: &[u8] = include_bytes!("../benches/wasm/pulldown-cmark.wasm");

fn main() {
    let a = |i| std::env::args().nth(i);
    let engine = a(1).unwrap_or_else(|| "submilli".into());
    let which = a(2).unwrap_or_else(|| "spidermonkey".into());
    let secs: u64 = a(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let bytes: &[u8] = match which.as_str() {
        "pulldown" | "pulldown-cmark" => PULLDOWN,
        _ => SPIDERMONKEY,
    };

    match engine.as_str() {
        "wasmi" => run::<Wasmi>(bytes, &which, secs),
        "wasmi-eager" => run::<WasmiEager>(bytes, &which, secs),
        _ => run::<Submilli>(bytes, &which, secs),
    }
}

trait Compile {
    fn setup() -> Self;
    fn compile(&self, bytes: &[u8]);
}

struct Submilli(submilli_wasm::Engine);
impl Compile for Submilli {
    fn setup() -> Self {
        Submilli(submilli_wasm::Engine::default())
    }
    fn compile(&self, bytes: &[u8]) {
        let m = submilli_wasm::Module::new(&self.0, bytes).unwrap();
        std::hint::black_box(&m);
    }
}

struct Wasmi(wasmi::Engine);
impl Compile for Wasmi {
    fn setup() -> Self {
        Wasmi(wasmi::Engine::default())
    }
    fn compile(&self, bytes: &[u8]) {
        let m = wasmi::Module::new(&self.0, bytes).unwrap();
        std::hint::black_box(&m);
    }
}

/// wasmi with `CompilationMode::Eager` — its default is `LazyTranslation`, under which
/// `Module::new` only validates and defers per-function translation to first call.
struct WasmiEager(wasmi::Engine);
impl Compile for WasmiEager {
    fn setup() -> Self {
        let mut config = wasmi::Config::default();
        config.compilation_mode(wasmi::CompilationMode::Eager);
        WasmiEager(wasmi::Engine::new(&config))
    }
    fn compile(&self, bytes: &[u8]) {
        let m = wasmi::Module::new(&self.0, bytes).unwrap();
        std::hint::black_box(&m);
    }
}

fn run<C: Compile>(bytes: &[u8], which: &str, secs: u64) {
    let c = C::setup();
    let mut best = Duration::from_secs(999);
    for _ in 0..5 {
        let t = Instant::now();
        c.compile(bytes);
        best = best.min(t.elapsed());
    }
    eprintln!(
        "best-of-5 {which} Module::new = {:.3} ms",
        best.as_secs_f64() * 1e3
    );

    // Sustained loop; check the clock only every 16 compiles so `Instant::now`
    // doesn't pollute the profile.
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut n = 0u64;
    'outer: loop {
        for _ in 0..16 {
            c.compile(bytes);
            n += 1;
        }
        if Instant::now() >= deadline {
            break 'outer;
        }
    }
    eprintln!("did {n} compiles in {secs}s");
}
