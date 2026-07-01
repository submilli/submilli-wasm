//! Shareable, stitch-style comparison table across submilli-wasm, wasmtime, and
//! wasmi. Unlike stitch's CoreMark-only table (pure execution, where every
//! interpreter loses to a JIT), this measures the *whole* lifecycle — the axis
//! this project optimizes: `fast compilation/startup >> runtime speed`.
//!
//! Startup phases are best-of-N wall clock. Execution is CoreMark's own score
//! (higher = faster), taken single-shot because each run self-times to ~18 s.
//!
//! Run with: `cargo run --release --example bench_table`
#![allow(clippy::unwrap_used)]

use std::hint::black_box;
use std::time::{Duration, Instant};

#[path = "../benches/support.rs"]
mod support;
use support::{submilli, wi, wt, COREMARK, MODULES};

/// Best-of-`iters` wall clock (one warmup discarded). Best-of resists the OS
/// scheduler adding noise upward; it's the fairest single number for a table.
/// The produced value is black-boxed *after* the timer stops so the compiler
/// can't elide the work, without charging the black-box to the measurement.
fn best_of<T>(iters: u32, mut f: impl FnMut() -> T) -> Duration {
    black_box(f());
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let t = Instant::now();
        let out = f();
        best = best.min(t.elapsed());
        black_box(out);
    }
    best
}

/// Auto-scaled duration so both a ~20 ns `Store::new` and a ~240 ms compile
/// read cleanly in the same column.
fn fmt_dur(d: Duration) -> String {
    let ns = d.as_nanos();
    if ns >= 1_000_000 {
        format!("{:.2} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.2} us", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

/// A row of three per-engine figures (submilli, wasmtime, wasmi).
fn row(label: &str, s: Duration, t: Duration, i: Duration) {
    println!(
        "{label:<28}{:>11}{:>11}{:>11}",
        fmt_dur(s),
        fmt_dur(t),
        fmt_dur(i),
    );
}

fn kib(bytes: usize) -> String {
    if bytes < 1 << 20 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Full compile → instantiate → `run()` for one engine namespace, yielding its
/// CoreMark score. A macro because the three namespaces are distinct types.
macro_rules! coremark_score {
    ($eng:ident) => {{
        let engine = $eng::engine();
        let module = $eng::compile(&engine, COREMARK);
        let linker = $eng::coremark_linker(&engine);
        let mut store = $eng::store(&engine);
        let inst = $eng::instantiate(&linker, &mut store, &module);
        $eng::run_coremark(&inst, &mut store)
    }};
}

fn main() {
    print_header();
    module_new_rows();
    store_new_row();
    cold_start_row();
    execution_row();
}

fn print_header() {
    println!("=== submilli-wasm lifecycle benchmark (lower = faster) ===\n");
    for &(name, bytes) in MODULES {
        println!("  {name:<16} {}", kib(bytes.len()));
    }
    println!(
        "\n{:<28}{:>11}{:>11}{:>11}",
        "phase", "submilli", "wasmtime", "wasmi"
    );
    println!("{}", "-".repeat(61));
}

/// Compile + validate, per module. Fewer iters for the big modules (each
/// compile is pricey). The headline row for a fast-startup interpreter.
fn module_new_rows() {
    for &(name, bytes) in MODULES {
        let iters = if bytes.len() > 1 << 20 { 8 } else { 40 };
        let se = submilli::engine();
        let te = wt::engine();
        let ie = wi::engine();
        row(
            &format!("Module::new  {name}"),
            best_of(iters, || submilli::compile(&se, bytes)),
            best_of(iters, || wt::compile(&te, bytes)),
            best_of(iters, || wi::compile(&ie, bytes)),
        );
    }
}

/// Module-independent; the engines are the shared, prebuilt object.
fn store_new_row() {
    let se = submilli::engine();
    let te = wt::engine();
    let ie = wi::engine();
    row(
        "Store::new",
        best_of(200, || submilli::store(&se)),
        best_of(200, || wt::store(&te)),
        best_of(200, || wi::store(&ie)),
    );
}

/// Compile + fresh store + instantiate: time-to-ready for a short-lived guest.
fn cold_start_row() {
    let se = submilli::engine();
    let sl = submilli::coremark_linker(&se);
    let te = wt::engine();
    let tl = wt::coremark_linker(&te);
    let ie = wi::engine();
    let il = wi::coremark_linker(&ie);
    row(
        "Cold start   coremark",
        best_of(40, || {
            let m = submilli::compile(&se, COREMARK);
            let mut s = submilli::store(&se);
            submilli::instantiate(&sl, &mut s, &m)
        }),
        best_of(40, || {
            let m = wt::compile(&te, COREMARK);
            let mut s = wt::store(&te);
            wt::instantiate(&tl, &mut s, &m)
        }),
        best_of(40, || {
            let m = wi::compile(&ie, COREMARK);
            let mut s = wi::store(&ie);
            wi::instantiate(&il, &mut s, &m)
        }),
    );
}

/// Execution: CoreMark's own score (higher = faster). Single-shot per engine
/// because each run self-times to a fixed duration (~18 s).
fn execution_row() {
    println!("{}", "-".repeat(61));
    let (s, t, i) = (
        coremark_score!(submilli),
        coremark_score!(wt),
        coremark_score!(wi),
    );
    println!(
        "{:<28}{:>11}{:>11}{:>11}",
        "CoreMark score (higher=fast)",
        format!("{s:.0}"),
        format!("{t:.0}"),
        format!("{i:.0}"),
    );
}
