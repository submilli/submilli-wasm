# Lifecycle benchmarks

Cross-engine comparison of **submilli-wasm** vs **wasmtime** vs **wasmi** across
the *whole* embedder pipeline — not just execution.

Stitch's well-known CoreMark table measures pure execution throughput, the one
axis where every interpreter loses to a JIT. This project's thesis is the
opposite one (`fast compilation/startup ≫ runtime speed`), so we measure each
phase separately: **`Module::new` → `Store::new` → instantiate → cold start**,
plus the CoreMark execution score for context — and one fused **run-once**
phase (linker + compile + store + instantiate + execute in a single window,
module never reused) that models the project's target workload: LLM-generated
code that runs exactly once. **Module RAM** rows report the heap retained per
compiled module — the multi-tenant *density* axis.

## Running

```sh
# One-time: fetch the large fixtures (spidermonkey, pulldown-cmark). They're
# gitignored to keep the repo lean; coremark-minimal.wasm is committed.
./scripts/fetch-bench-wasm.sh

# Shareable summary table (best-of-N startup phases + CoreMark score).
# `--features async` adds the async host-call row (the product's real IO path):
cargo run --release --example bench_table --features async

# Rigorous, statistical, CI-friendly (criterion) — startup phases only:
cargo bench --bench lifecycle
```

The build embeds the fixtures via `include_bytes!`, so `fetch-bench-wasm.sh`
must run before `cargo bench`/`cargo run --example bench_table`.

`cargo run --release --example bench_table` takes ~1 min: the three CoreMark
runs self-time to a fixed duration (~18 s each). The criterion bench omits
execution for that reason and covers only the fast startup phases.

## Sample output

Numbers are from one dev machine (Apple Silicon); treat them as ratios, not
absolutes. CoreMark is load-sensitive. Lower = faster, except the CoreMark
score (higher = faster).

```
phase                          submilli   wasmtime      wasmi
-------------------------------------------------------------
Module::new  coremark          57.62 us  732.00 us   97.12 us
Module::new  pulldown-cmark     1.23 ms   22.64 ms    2.30 ms
Module::new  spidermonkey      26.29 ms  196.80 ms   51.51 ms
Module RAM   coremark          67.4 KiB  78.8 KiB*   30.2 KiB
Module RAM   pulldown-cmark     1.5 MiB   1.4 MiB*  841.1 KiB
Module RAM   spidermonkey      31.1 MiB   3.8 MiB*   16.5 MiB
Module RAM   run-once         503.8 KiB  63.8 KiB*  216.7 KiB
                            (* metadata only: wasmtime JIT code is mmap'd)
Store::new                         0 ns     125 ns       0 ns
Cold start   coremark          59.54 us  718.62 us   98.75 us
Run once     sieve(1k)        459.88 us    5.72 ms  823.04 us
Run once     sieve(10k)       602.92 us    5.71 ms  926.96 us
Run once     sieve(1M)         17.70 ms    7.30 ms   11.63 ms
Host calls   ping x100k         4.34 ms  316.21 us    1.16 ms
Host calls   async x100k        9.55 ms    2.44 ms        n/a
-------------------------------------------------------------
CoreMark score (higher=fast)       1885      37833       3083
```

In this sample, submilli compiles each module and cold-starts CoreMark faster
than the other two engines. Wasmtime has the highest CoreMark score, followed
by wasmi. Wasmtime uses `OptLevel::None` and wasmi uses eager compilation, as
described in the methodology.

The **run-once rows** measure the complete pipeline for a generated module
that executes its code once. Submilli has the lowest total at sieve(1k) and
sieve(10k). Wasmtime has the lowest total at sieve(1M).

The **host-call rows** measure 100,000 guest-to-host-to-guest calls to a trivial
function. The async row awaits an immediately-ready future. Wasmi has no async
host functions, so its cell is n/a. The timings exclude work by a host service.

The **Module RAM rows** measure heap retained by compiled modules. Wasmi uses
less retained heap than submilli in each displayed row. Wasmtime's starred
cells count metadata only. The heap counter excludes memory mapped for JIT code.

## Methodology & fairness

- **Engine is setup, not a measured phase.** The `Engine` is the long-lived,
  shared object (config + compiled-code cache); it's built once and reused, so
  it's excluded from every timed phase — matching how embedders actually use it.
- **wasmtime runs Cranelift with optimization disabled** (`OptLevel::None`, set
  in `support.rs`). The benchmark does not use wasmtime 45's Winch baseline
  compiler or Pulley interpreter.
- **wasmi runs with eager compilation** (`CompilationMode::Eager`, set in
  `support.rs`). Its default `LazyTranslation` defers function translation
  until first call. Eager compilation includes translation in `Module::new`.
  The run-once fixture executes all its code, so lazy translation would perform
  that work inside the timed window.
- **The run-once fixture is generated, not fetched** (`run_once_wasm()` in
  `support.rs`, assembled from WAT in setup, never in a timed window). Shape:
  ~1200 distinct small helper functions in call chains of 100 (capped to stay
  under every engine's default recursion limit); `run` walks every chain head,
  so **all of the module's code executes** — modeling generated code, which is
  written to be used, not shipped cold — then runs a prime-sieve
  `run(n) -> i32` kernel. Import-free, so every engine instantiates it with an
  empty linker. Three sieve bounds (1k / 10k / 1M) span startup-dominated →
  execution-dominated totals, and each engine's result is asserted against the
  known prime count, so the row also cross-checks correctness.
- **Module size matters.** Compile cost scales with module size but execution
  doesn't, so we run a tiny module (`coremark`, 7.6 KiB) and two large real ones
  (`pulldown-cmark` 1.6 MiB, `spidermonkey` 4.0 MiB). The small-module cold start
  is where a fast-startup interpreter looks best; showing both avoids
  cherry-picking.
- **Best-of-N** for startup, run-once, and host-call timings (resists upward
  scheduler noise). Each invocation runs CoreMark once per engine. **criterion**
  provides statistical measurements for startup regression tracking.
- **Debug/name retention is off on both non-JIT engines.** submilli runs
  `wasm_backtrace(false)` (drops its per-op backtrace offset table + `name`
  section — the wasmtime-compatible default keeps them, costing ~+20% module
  RAM and ~10% startup); wasmi runs `ignore_custom_sections(true)` (by default
  it retains raw custom sections in the module — pulldown-cmark's `name`
  section alone is ~1.6 MiB, which inflated its RAM cell ~3×). Same-work
  principle as the wasmtime opt-level and wasmi eager-mode choices: every cell
  measures compiled code, not optional metadata.
- **Module RAM is a live-heap delta from a counting global allocator**
  (`support::mem`): warm the engine with one discarded compile, then measure
  allocated-minus-freed across a second, holding the module until read. All
  three engines allocate through it in-process, so the number is
  engine-agnostic — with one blind spot: wasmtime's JIT code is mmap'd outside
  the Rust allocator, so its cells are metadata-only floors (starred).
  Counting is enabled *only* during the RAM rows: the shared counter cache
  line is contended under parallel allocation, and leaving it on made
  wasmtime's multi-threaded Cranelift compiles read ~3× slower — the timing
  rows run with counting off (a single read-mostly flag on the alloc path).
- The `.wasm` fixtures under `wasm/` come from the
  [wasmi](https://github.com/wasmi-labs/wasmi) benchmark suite
  (`spidermonkey`, `pulldown-cmark`; fetched by `scripts/fetch-bench-wasm.sh`)
  and [stitch](https://github.com/makepad/stitch) (`coremark-minimal.wasm`,
  committed), all MVP-level modules every engine accepts.
