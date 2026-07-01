# Lifecycle benchmarks

Cross-engine comparison of **submilli-wasm** vs **wasmtime** vs **wasmi** across
the *whole* embedder pipeline — not just execution.

Stitch's well-known CoreMark table measures pure execution throughput, the one
axis where every interpreter loses to a JIT. This project's thesis is the
opposite one (`fast compilation/startup ≫ runtime speed`), so we measure each
phase separately: **`Module::new` → `Store::new` → instantiate → cold start**,
plus the CoreMark execution score for context.

## Running

```sh
# One-time: fetch the large fixtures (spidermonkey, pulldown-cmark). They're
# gitignored to keep the repo lean; coremark-minimal.wasm is committed.
./scripts/fetch-bench-wasm.sh

# Shareable summary table (best-of-N startup phases + CoreMark score):
cargo run --release --example bench_table

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
Module::new  coremark         226.50 us  855.00 us   31.42 us
Module::new  pulldown-cmark     1.89 ms   23.57 ms  808.88 us
Module::new  spidermonkey      42.90 ms  216.29 ms   18.03 ms
Store::new                         ~0 ns      ~0 ns      ~0 ns
Cold start   coremark          86.71 us  869.04 us   35.38 us
-------------------------------------------------------------
CoreMark score (higher=fast)        197      40401       3184
```

The story: submilli's `Module::new`/cold-start beats wasmtime **~5–9×** (it
skips the Cranelift JIT), while wasmtime wins execution **~200×** — exactly the
trade this project makes on purpose. wasmi (also non-JIT) compiles faster than
us today and executes faster; closing the compile gap is a fair target.

## Methodology & fairness

- **Engine is setup, not a measured phase.** The `Engine` is the long-lived,
  shared object (config + compiled-code cache); it's built once and reused, so
  it's excluded from every timed phase — matching how embedders actually use it.
- **wasmtime runs its default config** (Cranelift, optimized). That's the real
  drop-in comparison. wasmtime 45 also ships a baseline compiler (Winch) and an
  interpreter (Pulley) that trade compile time for execution speed; we don't use
  them, to keep "what you get out of the box" honest.
- **Module size matters.** Compile cost scales with module size but execution
  doesn't, so we run a tiny module (`coremark`, 7.6 KiB) and two large real ones
  (`pulldown-cmark` 1.6 MiB, `spidermonkey` 4.0 MiB). The small-module cold start
  is where a fast-startup interpreter looks best; showing both avoids
  cherry-picking.
- **Best-of-N** for startup phases (resists upward scheduler noise); **criterion**
  for the statistically-rigorous, regression-tracking numbers.
- The `.wasm` fixtures under `wasm/` come from the
  [wasmi](https://github.com/wasmi-labs/wasmi) benchmark suite
  (`spidermonkey`, `pulldown-cmark`; fetched by `scripts/fetch-bench-wasm.sh`)
  and [stitch](https://github.com/makepad/stitch) (`coremark-minimal.wasm`,
  committed), all MVP-level modules every engine accepts.
```
