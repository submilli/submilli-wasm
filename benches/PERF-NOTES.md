# `Module::new` performance investigation

A record of a spike into `submilli-wasm`'s compile/startup speed: the goal, every
optimization tried and its measured result, and the profiling methodology behind the
conclusions. **The code changes from this spike were reverted** — only the benchmark
harness (`benches/`, `examples/bench_table.rs`, `scripts/fetch-bench-wasm.sh`) was kept.
This doc is the takeaway.

---

## 1. Goal

The project thesis is `fast compilation/startup ≫ runtime speed` (see `CLAUDE.md`). So,
unlike stitch's CoreMark table (which measures pure *execution*, the axis where every
interpreter loses to a JIT), we wanted to measure the **whole embedder lifecycle** —
`Store::new` → `Module::new` → `Linker`/instantiate → execution — against `wasmtime`
and `wasmi`, and then try to **close the `Module::new` gap to wasmi on large modules**,
where we started ~2.4× slower.

---

## 2. The benchmark harness (what was kept)

Two front-ends over one shared support module (`benches/support.rs`), which drives the
identical `compile → store → instantiate → run` pipeline on all three engines:

```sh
./scripts/fetch-bench-wasm.sh          # one-time: fetch large .wasm fixtures (gitignored)
cargo run --release --example bench_table   # shareable comparison table
cargo bench --bench lifecycle               # criterion, startup phases only
```

- **`bench_table`** — a stitch-style table: `Module::new` (×3 modules), `Store::new`,
  cold start, and the CoreMark execution *score* (single-shot; each run self-times ~18 s).
- **`lifecycle`** (criterion) — statistical per-phase numbers for CI/regression.
- Fixtures: `coremark-minimal.wasm` (7.6 KiB, committed), `pulldown-cmark.wasm` (1.6 MiB),
  `spidermonkey.wasm` (4.0 MiB) — the last two are fetched (gitignored). spidermonkey is
  the canonical large-module compile-time benchmark (6596 functions, 1.84M operators).

Fairness notes: the `Engine` is built once (setup, not timed — it's the shared,
long-lived object). `wasmtime` runs its default (Cranelift). All numbers are one dev
machine (Apple Silicon); treat them as **ratios**, not absolutes.

---

## 3. Baseline results (`main`, before the spike)

Cool-machine `bench_table`, lower = faster except the CoreMark score:

```
phase                          submilli   wasmtime      wasmi
-------------------------------------------------------------
Module::new  coremark         226.50 us  855.00 us   31.42 us
Module::new  pulldown-cmark     1.89 ms   24.14 ms  807.00 us
Module::new  spidermonkey      ~45 ms    219.00 ms   17.55 ms
Cold start   coremark          86.71 us  817.00 us   32.79 us
-------------------------------------------------------------
CoreMark score (higher=fast)       ~190      40000       3100
```

**The story the benchmark tells:**
- We **beat `wasmtime` 5–16×** on `Module::new`/cold-start — it pays a big Cranelift JIT
  cost we skip. This is the fast-startup thesis, validated.
- We **lose to `wasmi` ~2.4×** on large-module `Module::new` (both are non-JIT and both
  use `wasmparser` to decode) — the gap we set out to close.
- We **lose execution** ~16× to wasmi, ~200× to wasmtime — expected and deprioritized.

---

## 4. What we tried, and what it did

Target: spidermonkey `Module::new`. Numbers are approximate (see thermal caveat, §6).

| # | change | result | verdict |
|---|--------|--------|---------|
| 1 | **Fused validation** — implement `wasmparser::VisitOperator`; validate via `FuncValidator::visitor()` and lower in the *same* decode pass, deleting the old `read_with_offset` + `fv.op` double-walk that materialized a 56-byte `Operator` per op | ~45 → ~37 ms | **real win** |
| 2 | **Ops arena** — one `Arc<Vec<Op>>` per module + a reused, pre-reserved emit scratch, instead of a `Box<[Op]>` per function (grown from empty) | ~37 → ~33 ms | **real win** |
| 3 | **Inline lowering** — drop `translate()`/`Operator` reconstruction; ~500 hand/macro-generated per-op `visit_*` lowering methods | ~1–2 ms | **flat** |
| 4 | **Locals arena** — generalize the arena to `local_types` too (`ArenaSlice<T>`) | ~0–1 ms | **flat** |
| 5 | **De-`Arc` `CompiledFunc`** — `Code = Arc<ModuleInner> + index`; `functions: Vec<CompiledFunc>` instead of `Vec<Arc<CompiledFunc>>` (6596 allocs → 1) | ~0–1 ms | **flat** |

**End state:** ~29.8 ms, **1.70×** wasmi (from 2.4×). Reliable final measurement
(best-of-20, settled machine, interleaved with wasmi to share thermal state):

```
submilli 29.8 ms | wasmi 17.4 ms | ratio 1.70×
```

The honest read: **fused validation + the ops arena did all the real work** (2.4× → ~1.85×).
Everything after (inline, locals arena, de-`Arc`) sat within measurement noise.

---

## 5. Key findings

1. **`Module::new` is memory-*traffic*-bound, not allocation-*count*-bound.** On macOS's
   allocator, small allocations are thread-cached and nearly free. That's why cutting
   allocation *count* (de-`Arc`: 6596 → 1; locals arena) did ~nothing, while cutting
   *bytes written/copied* (fused avoids the 56-byte `Operator`; arena avoids regrowth
   copies) actually moved the number. **This was the central mistake mid-spike** —
   chasing count off a profile bucket ("alloc/free/memcpy 34%") that was really traffic.

2. **The `wasmparser` decode+validate floor (~9 ms) already equals wasmi's.** Both use
   the same `wasmparser` `FuncValidator` the same way (confirmed by reading wasmi's
   `ValidatingFuncTranslator`). The entire remaining gap is our **extra traffic**: each
   32-byte `Op` is written **twice** — `emit` into the scratch buffer, then `append`
   scratch → arena — ≈ 118 MB, vs wasmi writing its instructions once.

3. **You can't "just save wasmparser's parsed ops."** `wasmparser::Operator` is **56 bytes**
   (vs our 32-byte `Op`), so storing it writes *more* (103 MB), and it **borrows the input
   bytes** (`BrTable` is a reader) → self-referential, not safe Rust. More generally: *any*
   pre-decoded array **is** the write traffic (`element_size × op_count`). "Don't re-decode"
   and "don't write" are mutually exclusive — pre-decoding *is* writing.

4. **The dispatch we optimized (inline lowering) wasn't the bottleneck.** The Instruments
   profile confirmed `translate`/`straight_line` went to 0, but wall-clock barely changed:
   the CPU was stalling on memory (the `Op` writes + allocator), so removing overlapped
   CPU work didn't help.

### Remaining levers (not pursued)

| lever | est. | cost |
|-------|------|------|
| **Shrink `Op` 32 → 24 B** (box the rare `BrTable`/`BrOnCast` variants) | ~26 ms | safe; helps runtime too (smaller hot-loop element) |
| **In-place / sidetable interpreter** — store *no* op array, keep the wasm bytes + only the branch/handler sidetable, re-decode operators at runtime (Titzer 2022, "A fast in-place interpreter for WebAssembly"; Wizard engine) | **~12 ms — beats wasmi** | new interpreter core; slower execution (re-decode per op). *On-thesis*: trades runtime for startup |

Matching wasmi's 17.4 ms with a pre-decoded array likely needs a **smaller instruction
encoding** (wasmi's register IR is compact). The in-place architecture is the only
approach identified that would push *below* wasmi on startup.

---

## 6. Profiling methodology

### Tools

- **macOS Instruments via `xctrace`** (Time Profiler) — the workhorse; no `sudo` needed.
- **`sample`** (macOS) — used early for quick inclusive call-trees.
- `dtrace` — unavailable (`sudo` blocked non-interactively; SIP enabled).
- `samply` / `cargo-flamegraph` / `cargo-instruments` — **not installed** on the machine.

### How a Time Profiler run was done

1. Build a symbol-rich release binary that hammers the target for ~18–25 s:
   ```sh
   RUSTFLAGS="-C force-frame-pointers=yes" CARGO_PROFILE_RELEASE_DEBUG=1 \
     cargo build --release --example <loops Module::new(spidermonkey) in a timed while-loop>
   ```
2. Record:
   ```sh
   xctrace record --template 'Time Profiler' --output /tmp/mn.trace --launch -- <binary>
   ```
3. Export the sample table to XML and aggregate in Python:
   ```sh
   xctrace export --input /tmp/mn.trace \
     --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' > /tmp/mn.xml
   ```
   The Python step resolves the XML's `id`/`ref` frame interning, sums **self-time**
   (weight of the *leaf* frame per sample), and buckets symbols into categories
   (alloc/free/memcpy, our `emit`, our lowering, wasmparser validate, wasmparser decode,
   section setup).

### Phase-isolation experiments

To attribute cost to phases, we temporarily edited the compile loop and re-measured:
- comment out `fv.op` / the validator call → isolates **validation** marginal cost;
- skip `translate` / the lowering call → isolates **decode+validate floor**;
- skip both → isolates **pure decode**.
The deltas gave a phase breakdown independent of the profiler.

### Thermal caveat (important)

Repeated heavy compile loops heated the machine, inflating absolute times (e.g.
spidermonkey `Module::new` read 30 ms cool vs 40+ ms hot; wasmi 17.6 ms cool vs 24 ms
hot). **Reliable signals** were therefore (a) the **ratio** submilli/wasmi measured in the
*same* run, and (b) Instruments **percentages**, both thermal-independent. Absolute
single-shot numbers across different moments are not comparable.

---

## 7. Profiling results

**Self-time buckets, spidermonkey `Module::new` (Instruments Time Profiler):**

| bucket | fused only | + ops arena (reserved) |
|--------|-----------:|-----------------------:|
| alloc / free / memcpy | **42.8%** | **34.0%** |
| wasmparser validate | 9.0% | 12.6% |
| our lowering dispatch | 9.9% | 12.7% |
| our `emit` (write `Op`s) | 7.4% | 7.9% |
| wasmparser decode | 7.2% | 8.7% |
| section decode / intern | 2.2% | 2.7% |
| other (dyld, allocator internals) | ~18% | ~19% |

Top self-time symbols were consistently `Translator::emit`, `_xzm_free`, `_platform_memmove`,
`xzm_realloc`, and the `xzm_*malloc` family — i.e. **writing and moving the `Op` bytes**,
not compute. `wasmparser::…::visit_operator` (decode) and the validator sat lower.

**Phase breakdown from the isolation experiments (early, fused, spidermonkey):**

| phase | ~cost |
|-------|------:|
| decode (wasmparser, unavoidable ≈ wasmi's) | ~9 ms |
| validate (wasmparser `FuncValidator`) | included in floor |
| our translate + `Op` write + arena copy | the rest |

The decode+validate **floor** matched wasmi's whole non-alloc budget; the surplus was
entirely our `Op` traffic — which is the conclusion in §5.

---

## 8. Bottom line

- The benchmark harness (kept) shows submilli **beats wasmtime 5–16× on startup** and
  **loses ~2.4× to wasmi** on large-module `Module::new`.
- A spike closed that to **~1.70×**, but **only fused validation + the ops arena mattered**;
  count-based allocation tricks were flat because compile is **traffic-bound**.
- Going *below* wasmi on startup is possible but needs an architectural change — a smaller
  `Op` encoding, or the **in-place/sidetable** interpreter (store no ops, re-decode at
  runtime), which fits the project's `startup ≫ runtime` thesis but is a new engine core.
