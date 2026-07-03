//! Shareable, stitch-style comparison table across submilli-wasm, wasmtime, and
//! wasmi. Unlike stitch's CoreMark-only table (pure execution, where every
//! interpreter loses to a JIT), this measures the *whole* lifecycle — the axis
//! this project optimizes: `fast compilation/startup >> runtime speed`.
//!
//! Startup phases and the fused run-once totals are best-of-N wall clock.
//! Execution is CoreMark's own score (higher = faster), taken single-shot
//! because each run self-times to ~18 s.
//!
//! Run with: `cargo run --release --example bench_table`
#![allow(clippy::unwrap_used)]

use std::hint::black_box;
use std::time::{Duration, Instant};

#[path = "../benches/support.rs"]
mod support;
use support::{retained_ram, submilli, wi, wt, COREMARK, MODULES, RUN_ONCE};

/// Count every heap allocation so the table can report retained module RAM (see
/// `support::mem`; wasmtime's mmap'd JIT code is outside this — README caveat).
#[global_allocator]
static ALLOC: support::mem::Tracking = support::mem::Tracking;

/// Best-of-`iters` wall clock (three warmups discarded — one is not enough: the
/// process-global first use of an engine still pays dyld/allocator/page-fault
/// warm-up, which used to inflate the table's very first row). Best-of resists
/// the OS scheduler adding noise upward; it's the fairest single number for a
/// table. The produced value is black-boxed *after* the timer stops so the
/// compiler can't elide the work, without charging the black-box to the
/// measurement.
fn best_of<T>(iters: u32, mut f: impl FnMut() -> T) -> Duration {
    for _ in 0..3 {
        black_box(f());
    }
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
    // Busy-spin briefly so the scheduler moves us to a boosted P-core before the
    // first row is timed — otherwise that row alone absorbs the ramp-up and can
    // read ~2× high (it intermittently did, even with per-row warm-up calls).
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_millis(300) {
        black_box(0);
    }
    let run_once_wasm = support::run_once_wasm();
    print_header(run_once_wasm.len());
    module_new_rows();
    module_ram_rows(&run_once_wasm);
    store_new_row();
    cold_start_row();
    run_once_rows(&run_once_wasm);
    host_call_row(&support::host_call_wasm());
    execution_row();
}

/// A row of three per-engine byte figures. The wasmtime cell is starred: its JIT code
/// lives in mmap'd regions the counting allocator can't see, so it shows metadata only.
fn row_bytes(label: &str, s: usize, t: usize, i: usize) {
    let t = format!("{}*", kib(t));
    println!("{label:<28}{:>11}{t:>11}{:>11}", kib(s), kib(i));
}

/// Heap bytes retained by one compiled `Module` (live-allocation delta, engine warmed
/// first — the multi-tenant density number). The wasmtime cell undercounts: its JIT
/// code lives in mmap'd regions outside the counting allocator.
fn module_ram_rows(run_once_wasm: &[u8]) {
    let se = submilli::engine();
    let te = wt::engine();
    let ie = wi::engine();
    let ram_row = |label: &str, bytes: &[u8]| {
        row_bytes(
            label,
            retained_ram(|| submilli::compile(&se, bytes)),
            retained_ram(|| wt::compile(&te, bytes)),
            retained_ram(|| wi::compile(&ie, bytes)),
        );
    };
    for &(name, bytes) in MODULES {
        ram_row(&format!("Module RAM   {name}"), bytes);
    }
    ram_row("Module RAM   run-once", run_once_wasm);
    println!("{:<28}(* metadata only: wasmtime JIT code is mmap'd)", "");
}

fn print_header(run_once_len: usize) {
    println!("=== submilli-wasm lifecycle benchmark (lower = faster) ===\n");
    for &(name, bytes) in MODULES {
        println!("  {name:<16} {}", kib(bytes.len()));
    }
    println!("  {:<16} {} (generated)", "run-once", kib(run_once_len));
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

/// The whole pipeline in one timed window — fresh linker + `Module::new` +
/// fresh store + instantiate + execute — for a module that is **never reused**:
/// the "LLM-generated code that runs once" use case this project targets. The
/// fixture executes ~all of its code (see `run_once_wasm`), so wasmi's eager
/// mode is representative — lazy translation would just pay inside the window.
fn run_once_rows(wasm: &[u8]) {
    let se = submilli::engine();
    let te = wt::engine();
    let ie = wi::engine();
    for &(label, n, expected, iters) in RUN_ONCE {
        row(
            &format!("Run once     {label}"),
            best_of(iters, || {
                let m = submilli::compile(&se, wasm);
                let l = submilli::empty_linker(&se);
                let mut s = submilli::store(&se);
                let inst = submilli::instantiate(&l, &mut s, &m);
                assert_eq!(submilli::run_i32(&inst, &mut s, n), expected);
            }),
            best_of(iters, || {
                let m = wt::compile(&te, wasm);
                let l = wt::empty_linker(&te);
                let mut s = wt::store(&te);
                let inst = wt::instantiate(&l, &mut s, &m);
                assert_eq!(wt::run_i32(&inst, &mut s, n), expected);
            }),
            best_of(iters, || {
                let m = wi::compile(&ie, wasm);
                let l = wi::empty_linker(&ie);
                let mut s = wi::store(&ie);
                let inst = wi::instantiate(&l, &mut s, &m);
                assert_eq!(wi::run_i32(&inst, &mut s, n), expected);
            }),
        );
    }
}

/// The host-call boundary: execution-only time for 100k data-dependent calls to a
/// trivial imported host fn (everything prebuilt). This is the dominant runtime cost
/// for IO-heavy orchestration guests — the workload the compute rows don't represent.
fn host_call_row(wasm: &[u8]) {
    const N: i32 = 100_000;
    macro_rules! cell {
        ($eng:ident) => {{
            let e = $eng::engine();
            let m = $eng::compile(&e, wasm);
            let l = $eng::ping_linker(&e);
            let mut s = $eng::store(&e);
            let inst = $eng::instantiate(&l, &mut s, &m);
            best_of(20, || assert_eq!($eng::run_i32(&inst, &mut s, N), N))
        }};
    }
    row(
        "Host calls   ping x100k",
        cell!(submilli),
        cell!(wt),
        cell!(wi),
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
