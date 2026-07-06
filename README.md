# submilli-wasm

A WebAssembly interpreter that would rather start *now* than run fast later.

## Why we built this

We're building a product that runs LLM-generated code. A model writes a program, we
compile it to wasm, run it once, and throw it away — for tenants who don't trust each
other and whom we don't trust either. The code itself is thin orchestration: call a
host function, shuffle some data, call another one. Almost all of its real work
happens on our side of the boundary.

It's an unusual workload for a wasm engine. Most engines — reasonably — optimize for
*compile once, run many times*: pay a compiler up front, win it back over many
iterations. Our modules don't have many iterations. They have one.

We started — and our product still runs — on **wasmtime**. It's the gold standard,
and we love its API. But as the workload took shape, Cranelift's compile time became
the bill we couldn't pay: ~190 ms for a 4 MiB module, *with optimization turned off*.
For code that runs once, that's not overhead — that's the whole show. So we looked
around:

- **Winch**, wasmtime's baseline compiler, has a lot of potential — much faster
  compilation, real momentum. But no GC and no exception handling, and code generated
  from higher-level languages needs both.
- **wasmi** is, honestly, amazing — tiny, fast to start, shockingly fast to execute
  for an interpreter. We benchmark against it with respect. But same story: no GC, no
  exception handling — and no async host functions (it offers resumable calls, a
  different integration model). That last one matters a lot to us: our guests spend
  their lives waiting on IO, and density means parking thousands of them on awaits
  over a small thread pool, not holding a thread per guest.

We needed the combination: the wasmtime API our product is already written against
(async host functions included), wasmi-class startup, and full Wasm 3.0 — GC and
`try_table` included, because that's what compilers emit now. We couldn't find it on
the shelf, so we started building it.

One caveat: "wasmtime-compatible" means the slice of the API our product actually
uses, not all of it. If something you need is missing, that's just us not having
needed it yet — contributions of any kind are very welcome.

To be clear about when you should *not* use this: if your modules run more than a
handful of times, wasmtime will beat us and you should use it. If you want the fastest
interpreter and can live without GC and exceptions, wasmi is excellent. Where we fit
is the rest: modules that live for exactly one run, or anywhere you need an
interpreter that speaks full Wasm 3.0 — GC and exceptions included. Either way, keep
in mind it's still experimental.

One more thing shaped everything: **every guest is hostile**. Not "possibly buggy" —
hostile. That's why the entire tree is `unsafe`-free (isolation rests on the borrow
checker and bounds checks, not on us being careful), why guest-reachable code traps
instead of panicking (a panic takes down every tenant on the process), why every
allocation a guest can see is zeroed first, and why everything growable runs through
resource limits, fuel, and epochs. The details live in [`SECURITY.md`](SECURITY.md).

## What came out

An interpreter — deliberately no JIT — with a `wasmtime`-shaped API: `Engine`,
`Store<T>`, `Module`, `Linker`, `func_wrap`, async host functions, limiters, fuel,
epochs, `Trap` and `WasmBacktrace` via `downcast_ref`. Our own embedding code moved
over with little more than a dependency swap — though as said above, the surface is
what our product needed, and it grows one missing method at a time.

And the swap really is one line, thanks to Cargo's package rename:

```toml
[dependencies]
wasmtime = { package = "submilli-wasm", version = "0.1", features = ["async"] }
```

Every `use wasmtime::…` in the embedder keeps compiling — now against the
interpreter. Or, starting fresh:

```rust
use submilli_wasm::{Engine, Linker, Module, Store};

let engine = Engine::default();
let mut store = Store::new(&engine, ());
let mut linker = Linker::new(&engine);
linker.func_wrap("host", "add", |a: i32, b: i32| a + b)?;

let module = Module::new(&engine, wasm_bytes)?; // one pass, ~1.4x the validation floor
let instance = linker.instantiate(&mut store, &module)?;
let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
let result = run.call(&mut store, 7)?;
```

Under the hood, the whole design chases the three things our workload cares about:

- **Startup.** Validation and lowering are fused into a single decode pass — the
  module's bytes are read once, and each internal op is written once, straight into
  module-wide arenas. There is no second pass to be slow in.
- **The host-call boundary.** An IO-heavy guest crosses guest↔host constantly, so the
  crossing had better be cheap: sync host calls run *inside* the dispatch loop
  (no suspend/resume round trip), allocation-free in steady state, ~35 ns each —
  with panic containment still on every crossing.
- **Density.** Ops are a fixed 16 bytes (wide immediates live in side pools), a
  compiled function is a plain `Copy` record of spans, and instances share everything
  through one `Arc`. More tenants per gigabyte is a feature.

## WebAssembly proposals

As far as the upstream Wasm 3.0 spec testsuite can tell, this is a **complete
Wasm 3.0 implementation**: with the `simd` cargo feature enabled it passes
**256 files, 2,241 modules, 62,373 assertions with zero skips** (the default build
leaves vector instructions out for leaner builds). We try not to grade our own
homework — a proposal is ✅ below only if its assertions run un-skipped in that suite.

| WebAssembly proposal | Status | Notes |
|---|:---:|---|
| [`mutable-global`] | ✅ | |
| [`saturating-float-to-int`] | ✅ | |
| [`sign-extension`] | ✅ | |
| [`multi-value`] | ✅ | |
| [`bulk-memory`] | ✅ | |
| [`reference-types`] | ✅ | |
| [`simd`] (fixed-width 128-bit) | ✅ | behind the `simd` cargo feature |
| [`relaxed-simd`] | ✅ | behind `simd`; one fixed deterministic lowering per op |
| [`tail-call`] | ✅ | |
| [`extended-const`] | ✅ | |
| [`function-references`] (typed refs) | ✅ | |
| [`gc`] | ✅ | structs/arrays/`i31`, mark-sweep collector, engine-wide pressure coordination |
| [`multi-memory`] | ✅ | |
| [`memory64`] | ✅ | memories *and* tables |
| [`exception-handling`] | ✅ | `try_table`/`exnref` (the legacy `try`/`catch` form is not supported) |
| custom annotations | ✅ | text format; the binary format is unaffected |
| [`threads`] | ❌ | not in Wasm 3.0, and out of scope — stores are single-threaded by design; density comes from many stores |
| [`custom-page-sizes`] | ❌ | post-3.0, not yet targeted |
| [`wide-arithmetic`] | ❌ | post-3.0, not yet targeted |
| component model / WASI | ❌ | core wasm only — bring your own host functions via `Linker` |

[`mutable-global`]: https://github.com/WebAssembly/mutable-global
[`saturating-float-to-int`]: https://github.com/WebAssembly/nontrapping-float-to-int-conversions
[`sign-extension`]: https://github.com/WebAssembly/sign-extension-ops
[`multi-value`]: https://github.com/WebAssembly/multi-value
[`bulk-memory`]: https://github.com/WebAssembly/bulk-memory-operations
[`reference-types`]: https://github.com/WebAssembly/reference-types
[`simd`]: https://github.com/WebAssembly/simd
[`relaxed-simd`]: https://github.com/WebAssembly/relaxed-simd
[`tail-call`]: https://github.com/WebAssembly/tail-call
[`extended-const`]: https://github.com/WebAssembly/extended-const
[`function-references`]: https://github.com/WebAssembly/function-references
[`gc`]: https://github.com/WebAssembly/gc
[`multi-memory`]: https://github.com/WebAssembly/multi-memory
[`memory64`]: https://github.com/WebAssembly/memory64
[`exception-handling`]: https://github.com/WebAssembly/exception-handling
[`threads`]: https://github.com/WebAssembly/threads
[`custom-page-sizes`]: https://github.com/WebAssembly/custom-page-sizes
[`wide-arithmetic`]: https://github.com/WebAssembly/wide-arithmetic

## How fast is it, really

Ask a wasm engine how fast it is and you'll usually get one number: how quickly it
chews through instructions. That's a fine number — it's just not how real-world usage
feels. In the real world you pay to compile the module, you pay memory to keep it
around, you pay for every host call it makes, and *then* you pay to execute it. So
our benchmark measures that whole life: **compilation speed, resident memory, host
calls (sync and async), and "compile + instantiate + run once" end to end** — with
instruction throughput as one row among many, not the headline.

Numbers from the in-repo cross-engine benchmark, on one Apple Silicon dev machine —
treat them as ratios. Every cell measures the same work: wasmtime runs Cranelift with
optimization *disabled* (the fairest floor for a startup comparison), wasmi runs eager
compilation, and both non-JIT engines run with debug retention off. The fairness
reasoning for each knob is in [`benches/README.md`](benches/README.md).

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

Reading it honestly:

- **Compilation is what we optimized for**, and on this benchmark it shows: 8–20×
  faster than wasmtime, ~2× faster than eager wasmi, sitting ~1.4× above the raw
  `wasmparser` validation floor. Cold-starting a small module takes ~52 µs.
- **"Run once" is our actual metric** — linker + compile + store + instantiate +
  execute, nothing reused, on a module that runs all of its code. We win while the
  guest's own work is light (469 µs vs wasmi's 837 µs vs wasmtime's 5.7 ms), tie
  wasmi around a millisecond of guest compute, and lose the compute-heavy end. That's
  the trade we chose.
- **The boundary is cheap.** ~35 ns per sync crossing; ~73 ns async, where the
  premium is the suspend to a real await point plus the `Box<dyn Future>` the
  wasmtime-style API requires. Under real IO the await dwarfs all of it.
- **Memory has room to improve — but look at the sizes first.** wasmi's
  variable-length IR packs code ~2× tighter than our fixed 16-byte ops, and that gap
  is real. It's also, for now, cheap: an LLM-generated module is typically tens of
  kilobytes to a couple hundred (our run-once fixture, ~70 KiB of wasm, holds
  ~500 KiB resident), so you fit thousands per gigabyte either way. Denser encoding
  is on the list, just not next — where memory actually bites at runtime is the GC
  heap, which is why it gets its own section below.
- **Pure execution is where we pay.** A JIT beats us ~50× on CoreMark and wasmi's
  register IR ~4× — wasmi has spent years earning that, and we're not pretending
  otherwise. We paid the price on the axis our workload uses least, and tuned the
  interpreter ~3.5× along the way.

## The GC, briefly

Wasm GC is where memory really matters for us: guest programs compiled from
higher-level languages allocate structs and arrays at runtime, and in a multi-tenant
host that heap — not the code — is what grows unpredictably. So the collector is
built around the same two rules as everything else here: hostile guests, and no
hot-path cost.

- **Per-store, non-moving mark–sweep** (the default; an allocate-only null collector
  is available for short runs that would rather trap on exhaustion than ever
  collect). Each store owns its heap outright — one tenant collecting never pauses
  another. Handles are indices with generation stamps, so a stale handle from a
  freed object is caught, not dereferenced.
- **Every guest allocation is bounded.** Allocations draw from a byte reservation
  granted through your `ResourceLimiter`, grown in bounded batches (64 KiB and
  doubling, capped) — the guest collects before it grows, and a hostile allocator
  hits its limit instead of your RAM.
- **When we collect**: when a store's reservation runs out (collect, then grow);
  when the engine-wide heap total crosses `Config::gc_memory_threshold` — that posts
  a request to every store, honored at amortized safepoints (every 1024 guest calls
  and after each async host call, never per instruction); and whenever you call
  `Store::gc()` yourself.
- **Roots without tags**: the operand stack is untyped 8-byte cells for speed, with
  a one-byte-per-slot shadow recording which slots hold references — the collector
  reads the shadow, so the interpreter never pays for GC bookkeeping in its value
  representation.

## Things worth knowing before embedding

- The dispatch loop is generic over your `Store<T>` data type (that's how sync host
  calls run loop-resident). It means the interpreter monomorphizes in *your* crate,
  once per store type — a little compile time traded for a much cheaper boundary.
- Backtraces are the default (wasmtime-compatible) and they're priced honestly: the
  per-op offset table behind them costs ~20% module RAM and ~10% startup. Density-
  sensitive deployments turn it off with `Config::wasm_backtrace(false)`.
- House rules: warnings are hard failures, and the
  dispatch `match` is the one sanctioned long function.

## Developing

```sh
cargo build
cargo test                        # unit tests
git submodule update --init       # once: the upstream spec test vectors
cargo test --test spec            # spec .wast conformance suite
cargo clippy --all-targets -- -D warnings
./scripts/fetch-bench-wasm.sh     # once: large benchmark fixtures (gitignored)
cargo run --release --example bench_table --features async
```

Design docs: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and
[`docs/STYLE.md`](docs/STYLE.md).
