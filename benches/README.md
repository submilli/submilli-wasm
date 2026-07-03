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
absolutes. Lower = faster, except the CoreMark score (higher = faster).

```
phase                          submilli   wasmtime      wasmi
-------------------------------------------------------------
Module::new  coremark          51.12 us  792.71 us  100.71 us
Module::new  pulldown-cmark     1.11 ms   22.16 ms    2.31 ms
Module::new  spidermonkey      23.81 ms  192.81 ms   54.18 ms
Module RAM   coremark          66.4 KiB  78.8 KiB*   30.2 KiB
Module RAM   pulldown-cmark     1.5 MiB   1.4 MiB*  841.1 KiB
Module RAM   spidermonkey      31.0 MiB   3.8 MiB*   16.5 MiB
Module RAM   run-once         503.6 KiB  63.8 KiB*  216.7 KiB
                            (* metadata only: wasmtime JIT code is mmap'd)
Store::new                         0 ns     125 ns       0 ns
Cold start   coremark          52.04 us  769.25 us  100.83 us
Run once     sieve(1k)        468.96 us    5.72 ms  837.17 us
Run once     sieve(10k)       918.71 us    5.86 ms  919.58 us
Run once     sieve(1M)         57.39 ms    7.23 ms   11.30 ms
Host calls   ping x100k         3.52 ms  325.92 us    1.15 ms
Host calls   async x100k        7.33 ms    2.69 ms        n/a
-------------------------------------------------------------
CoreMark score (higher=fast)        715      36996       3032
```

The story: submilli's `Module::new`/cold-start beats wasmtime **~5–16×** (it
skips the Cranelift JIT), while wasmtime wins execution **~59×** — exactly the
trade this project makes on purpose. Note wasmtime here runs Cranelift with
**optimization disabled** (`OptLevel::None`, see below), yet its `Module::new`
barely moves versus the optimized build — the JIT's compile cost dominates
regardless of opt level, which is the whole point. Against wasmi — the other
non-JIT, forced to **eager compilation** so its compile column measures the
same work (see below) — submilli compiles **~2× faster at every module size**
(coremark 50 vs 98 µs, spidermonkey 24 vs 53 ms) and wins the small-module
cold start (51 vs 100 µs), while wasmi executes ~4.2× faster. The compile lead
comes from fusing validation into lowering, writing every op once straight
into module-wide arenas, and a compact 16-byte non-drop `Op`; the execution
gap (was ~17×) shrank ~3.5× across the interpreter-loop optimization passes —
see `PERF-NOTES.md` (§12–13).

The **run-once rows** are the project's target use case measured end to end:
one window covering empty linker + `Module::new` + fresh `Store` + instantiate
+ execution, on a generated ~70 KiB, ~1200-function module whose `run` executes
essentially *all* of its code (see below — generated code is written to be
used, so a run-once engine doesn't get to skip translating it), with three
sieve sizes spanning the execution-weight curve. They locate the crossovers
honestly. While execution is light, **submilli wins the total** — 469 µs vs
wasmi's 837 µs (its ~740 µs eager compile dominates) and wasmtime's 5.7 ms
(the JIT's compile bill) — and at sieve(10k) the totals are now a dead tie
(919 vs 920 µs). Only past that does the ~4.2× interpreter gap hand wasmi the
total (57 ms vs 11 ms at sieve(1M); wasmtime flattens at ~6–7 ms regardless).
The interpreter passes in `PERF-NOTES.md` §12 cut these totals ~4×
(sieve(1M) 236 → 57 ms); pushing the crossover further out is the
tail-call-dispatch class of work.

The **host-call row** measures the guest→host→guest boundary — 100k
data-dependent calls to a trivial imported function, execution only. For
IO-heavy orchestration guests (the product workload: thin glue around host
stdlib calls) this, not compute throughput, is the runtime number that
matters. Three optimization passes (cache each host fn's signature at
registration; reuse arg/result buffers so the boundary is allocation-free in
steady state; a direct scalar codec; swap-based execution parking; and
finally *loop-resident* sync host calls — the dispatch loop invokes the
callback directly instead of suspending out through an `Outcome`) took
submilli from 189 → ~35 ns/call, ~3× from wasmi (~12 ns) with
panic-containment still on every crossing. The **async row** (awaited calls
to an immediately-ready host future — the machinery under the product's real
IO path) got the same treatment plus its own pass: 200 → ~73 ns/call, ~2.7×
from wasmtime (~27 ns; wasmi has no async host functions — its answer is
resumable calls, so its cell is n/a). An async call's extra ~38 ns over sync
is the `Outcome` suspend to the driver's await point (structurally required —
a sync loop can't await) plus the per-call `Box<dyn Future>` both engines'
APIs mandate.

The **Module RAM rows** are the density axis: heap bytes a compiled module
keeps resident, which bounds how many tenants fit on a host. Both non-JIT
engines run with debug/name retention **off** (see below) so the rows compare
pure compiled-code density, and the story is uniform: **wasmi is ~2× denser
at every size** (coremark 66 vs 30 KiB, pulldown 1.5 MiB vs 841 KiB,
spidermonkey 31.0 vs 16.5 MiB) — its variable-length byte IR averages ~9
bytes/op against submilli's fixed 16-byte `Op`. Recent work already cut
submilli's footprint sharply (24→16-byte ops, then module-wide arenas: peak
compile RSS 162 → 44.5 MB, `PERF-NOTES.md` §13); closing the remaining ~2× is
the 8–12-byte encoding rung (pool the wide constants, wasmi-style). The
starred wasmtime cells are floor values — its JIT code lives in mmap'd
executable regions the heap counter cannot see.

## Methodology & fairness

- **Engine is setup, not a measured phase.** The `Engine` is the long-lived,
  shared object (config + compiled-code cache); it's built once and reused, so
  it's excluded from every timed phase — matching how embedders actually use it.
- **wasmtime runs Cranelift with optimization disabled** (`OptLevel::None`, set
  in `support.rs`). Since optimization is exactly the compile-time cost this
  project skips, disabling it is the *fairest* floor for a startup comparison —
  yet wasmtime's `Module::new` barely changes from the optimized default (the
  register allocation / lowering / encoding that dominate a JIT run at every opt
  level), so it stays ~5–16× slower to start. wasmtime 45 also ships a baseline
  compiler (Winch) and an interpreter (Pulley) that trade compile time for
  execution speed; we don't use them, to keep the default `Engine` honest.
- **wasmi runs with eager compilation** (`CompilationMode::Eager`, set in
  `support.rs`). wasmi's *default* is `LazyTranslation`: `Module::new` only
  validates, deferring each function's translation to its first call — a
  validate-only number that would sit next to the other engines' full compiles.
  Forcing eager makes every `Module::new` cell measure the same work (the same
  fairness reasoning as the wasmtime opt-level choice, applied in the opposite
  direction). The deferred cost is real, not noise: wasmi-lazy handles
  spidermonkey in ~18 ms vs ~52 ms eager — the difference is simply paid later,
  at first-call time. In the run-once rows eager is also the *representative*
  mode, not just the fair one: the fixture executes all of its code, so lazy
  translation would do the same total work inside the same timed window.
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
- **Best-of-N** for startup phases (resists upward scheduler noise); **criterion**
  for the statistically-rigorous, regression-tracking numbers.
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
