//! Cross-engine lifecycle benchmark (criterion): submilli-wasm vs wasmtime vs
//! wasmi across the *whole* embedder pipeline, not just execution.
//!
//! The project's thesis is `fast compilation/startup >> runtime speed`, so the
//! interesting phases are the startup ones — `Module::new`, `Store::new`,
//! instantiate, and the fused cold start. Execution (CoreMark) self-times to a
//! fixed duration (~18 s/run) and so lives in `examples/bench_table.rs` as a
//! single-shot score, not here. The `Engine` is built in setup (shared,
//! long-lived) and never inside a measured phase.
#![allow(clippy::unwrap_used, clippy::many_single_char_names)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

#[path = "support.rs"]
mod support;
use support::{COREMARK, MODULES};

/// Compile + validate. The headline: submilli skips wasmtime's JIT, so it
/// should win big on the large modules; wasmi is the near comparison.
fn module_new(c: &mut Criterion) {
    let mut g = c.benchmark_group("module_new");
    g.sample_size(20);
    for &(name, bytes) in MODULES {
        let e = support::submilli::engine();
        g.bench_function(BenchmarkId::new("submilli", name), |b| {
            b.iter(|| black_box(support::submilli::compile(&e, black_box(bytes))));
        });
        let e = support::wt::engine();
        g.bench_function(BenchmarkId::new("wasmtime", name), |b| {
            b.iter(|| black_box(support::wt::compile(&e, black_box(bytes))));
        });
        let e = support::wi::engine();
        g.bench_function(BenchmarkId::new("wasmi", name), |b| {
            b.iter(|| black_box(support::wi::compile(&e, black_box(bytes))));
        });
    }
    g.finish();
}

fn store_new(c: &mut Criterion) {
    let mut g = c.benchmark_group("store_new");
    let e = support::submilli::engine();
    g.bench_function("submilli", |b| {
        b.iter(|| black_box(support::submilli::store(&e)));
    });
    let e = support::wt::engine();
    g.bench_function("wasmtime", |b| {
        b.iter(|| black_box(support::wt::store(&e)));
    });
    let e = support::wi::engine();
    g.bench_function("wasmi", |b| {
        b.iter(|| black_box(support::wi::store(&e)));
    });
    g.finish();
}

/// Instantiate an already-compiled CoreMark into a fresh store (linker +
/// engine + module are prebuilt; only the per-request instantiation is timed).
fn instantiate(c: &mut Criterion) {
    let mut g = c.benchmark_group("instantiate/coremark");
    {
        let e = support::submilli::engine();
        let m = support::submilli::compile(&e, COREMARK);
        let l = support::submilli::coremark_linker(&e);
        g.bench_function("submilli", |b| {
            b.iter_batched(
                || support::submilli::store(&e),
                |mut s| black_box(support::submilli::instantiate(&l, &mut s, &m)),
                BatchSize::SmallInput,
            );
        });
    }
    {
        let e = support::wt::engine();
        let m = support::wt::compile(&e, COREMARK);
        let l = support::wt::coremark_linker(&e);
        g.bench_function("wasmtime", |b| {
            b.iter_batched(
                || support::wt::store(&e),
                |mut s| black_box(support::wt::instantiate(&l, &mut s, &m)),
                BatchSize::SmallInput,
            );
        });
    }
    {
        let e = support::wi::engine();
        let m = support::wi::compile(&e, COREMARK);
        let l = support::wi::coremark_linker(&e);
        g.bench_function("wasmi", |b| {
            b.iter_batched(
                || support::wi::store(&e),
                |mut s| black_box(support::wi::instantiate(&l, &mut s, &m)),
                BatchSize::SmallInput,
            );
        });
    }
    g.finish();
}

/// Cold start = compile + fresh store + instantiate, i.e. time-to-ready for a
/// short-lived guest. This is the number that sells a fast-startup interpreter.
fn cold_start(c: &mut Criterion) {
    let mut g = c.benchmark_group("cold_start/coremark");
    {
        let e = support::submilli::engine();
        let l = support::submilli::coremark_linker(&e);
        g.bench_function("submilli", |b| {
            b.iter(|| {
                let m = support::submilli::compile(&e, COREMARK);
                let mut s = support::submilli::store(&e);
                black_box(support::submilli::instantiate(&l, &mut s, &m));
            });
        });
    }
    {
        let e = support::wt::engine();
        let l = support::wt::coremark_linker(&e);
        g.bench_function("wasmtime", |b| {
            b.iter(|| {
                let m = support::wt::compile(&e, COREMARK);
                let mut s = support::wt::store(&e);
                black_box(support::wt::instantiate(&l, &mut s, &m));
            });
        });
    }
    {
        let e = support::wi::engine();
        let l = support::wi::coremark_linker(&e);
        g.bench_function("wasmi", |b| {
            b.iter(|| {
                let m = support::wi::compile(&e, COREMARK);
                let mut s = support::wi::store(&e);
                black_box(support::wi::instantiate(&l, &mut s, &m));
            });
        });
    }
    g.finish();
}

/// The whole pipeline in one timed window — fresh linker + compile + fresh
/// store + instantiate + execute — for a module that is never reused: the
/// "LLM-generated code that runs once" use case. Light workload only; the
/// execution-heavy variant lives in `bench_table` (too slow to sample here).
fn run_once(c: &mut Criterion) {
    let (label, n, expected, _) = support::RUN_ONCE[0];
    let wasm = support::run_once_wasm();
    let mut g = c.benchmark_group(format!("run_once/{label}"));
    g.sample_size(20);
    {
        let e = support::submilli::engine();
        g.bench_function("submilli", |b| {
            b.iter(|| {
                let m = support::submilli::compile(&e, &wasm);
                let l = support::submilli::empty_linker(&e);
                let mut s = support::submilli::store(&e);
                let inst = support::submilli::instantiate(&l, &mut s, &m);
                assert_eq!(support::submilli::run_i32(&inst, &mut s, n), expected);
            });
        });
    }
    {
        let e = support::wt::engine();
        g.bench_function("wasmtime", |b| {
            b.iter(|| {
                let m = support::wt::compile(&e, &wasm);
                let l = support::wt::empty_linker(&e);
                let mut s = support::wt::store(&e);
                let inst = support::wt::instantiate(&l, &mut s, &m);
                assert_eq!(support::wt::run_i32(&inst, &mut s, n), expected);
            });
        });
    }
    {
        let e = support::wi::engine();
        g.bench_function("wasmi", |b| {
            b.iter(|| {
                let m = support::wi::compile(&e, &wasm);
                let l = support::wi::empty_linker(&e);
                let mut s = support::wi::store(&e);
                let inst = support::wi::instantiate(&l, &mut s, &m);
                assert_eq!(support::wi::run_i32(&inst, &mut s, n), expected);
            });
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    module_new,
    store_new,
    instantiate,
    cold_start,
    run_once
);
criterion_main!(benches);
