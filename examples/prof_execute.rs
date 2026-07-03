//! Focused execution-profiling harness: compiles + instantiates once, then hammers guest
//! execution so a Time Profiler run has a hot, symbol-rich interpreter loop to sample.
//! Usage: prof_execute [coremark|sieve] [secs]
//!   coremark — CoreMark's own run() (self-times ~18 s; `secs` ignored)
//!   sieve    — the run-once fixture's sieve(1M) in a timed loop
//! Temporary — used for the execution-speed spike; safe to delete.
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

use std::time::{Duration, Instant};

#[path = "../benches/support.rs"]
mod support;
use support::{submilli, COREMARK};

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "coremark".into());
    let secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);

    let e = submilli::engine();
    #[cfg(feature = "async")]
    if which == "hostcall-async" {
        let m = submilli::compile(&e, &support::host_call_wasm());
        let mut l = submilli::empty_linker(&e);
        l.func_wrap_async("env", "ping", |_c, (x,): (i32,)| {
            Box::new(async move { x + 1 })
        })
        .unwrap();
        let mut s = submilli::store(&e);
        pollster::block_on(async {
            let inst = l.instantiate_async(&mut s, &m).await.unwrap();
            let f = inst.get_typed_func::<i32, i32>(&mut s, "run").unwrap();
            let deadline = Instant::now() + Duration::from_secs(secs);
            let mut n = 0u64;
            while Instant::now() < deadline {
                assert_eq!(f.call_async(&mut s, 100_000).await.unwrap(), 100_000);
                n += 1;
            }
            eprintln!("did {n} async ping-x100k runs in {secs}s");
        });
        return;
    }
    if which == "hostcall" {
        let m = submilli::compile(&e, &support::host_call_wasm());
        let l = submilli::ping_linker(&e);
        let mut s = submilli::store(&e);
        let inst = submilli::instantiate(&l, &mut s, &m);
        let t = Instant::now();
        assert_eq!(submilli::run_i32(&inst, &mut s, 100_000), 100_000);
        eprintln!(
            "ping x100k once = {:.2} ms",
            t.elapsed().as_secs_f64() * 1e3
        );
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut n = 0u64;
        while Instant::now() < deadline {
            assert_eq!(submilli::run_i32(&inst, &mut s, 100_000), 100_000);
            n += 1;
        }
        eprintln!("did {n} ping-x100k runs in {secs}s");
    } else if which == "sieve" {
        let wasm = support::run_once_wasm();
        let m = submilli::compile(&e, &wasm);
        let l = submilli::empty_linker(&e);
        let mut s = submilli::store(&e);
        let inst = submilli::instantiate(&l, &mut s, &m);
        let t = Instant::now();
        assert_eq!(submilli::run_i32(&inst, &mut s, 1_000_000), 78_498);
        eprintln!("sieve(1M) once = {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut n = 0u64;
        while Instant::now() < deadline {
            assert_eq!(submilli::run_i32(&inst, &mut s, 1_000_000), 78_498);
            n += 1;
        }
        eprintln!("did {n} sieve(1M) runs in {secs}s");
    } else {
        let m = submilli::compile(&e, COREMARK);
        let l = submilli::coremark_linker(&e);
        let mut s = submilli::store(&e);
        let inst = submilli::instantiate(&l, &mut s, &m);
        let score = submilli::run_coremark(&inst, &mut s);
        eprintln!("coremark score = {score:.0}");
    }
}
