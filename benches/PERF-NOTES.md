# `Module::new` performance investigation

A record of a spike into `submilli-wasm`'s compile/startup speed: the goal, every
optimization tried and its measured result, and the profiling methodology behind the
conclusions. The spike itself was reverted; this doc is the takeaway.

> **Update — landed.** The two levers that mattered (**fused validation** #1 and **inline
> lowering** #3) plus the **write-once op buffer** were subsequently implemented on `main`
> (a `wasmparser::VisitOperator`-driven single pass in `src/module/compile/{visit,visit_simd,
> core,numeric,memory,table,ref_,gc}.rs` + `control/visit.rs`; `CompiledFunc.ops` pre-sized
> `Vec<Op>`, no `into_boxed_slice`). Measured result, same methodology:
> `spidermonkey Module::new` **~45 → 31.9 ms (1.73× wasmi, from ~2.4×)**;
> `pulldown-cmark` **1.95 → 1.33 ms (1.58×)**; `coremark` **94.8 → 77.6 µs**. The interpreter
> core and `Op` layout are unchanged, so runtime/cold-start don't regress. Everything below is
> the original spike write-up.
>
> **Update 2 — re-profile + `Op` shrink landed.** After the write-once buffer landed, a re-profile
> found the bottleneck had **moved off allocation entirely** (`alloc/free/memcpy` 34% → **10%**)
> and onto **`wasmparser` decode** — the exact same floor wasmi pays. See **§9** for the full
> decode-bound breakdown. The one surplus lever left that helped: **shrinking `Op` 32 → 24 B and
> making it non-drop** (side-table `br_table` targets so nothing inline is `Box`ed). Landed on
> `main`. Measured (interleaved best-of, same methodology): `spidermonkey Module::new`
> **31.3 → 27.5 ms (1.53× wasmi, from ~1.72×)**; `pulldown-cmark` **1.31 → 1.19 ms**; CoreMark
> score **194 → 194** (runtime unchanged — smaller hot-loop element, no regression). `Op` teardown
> (`drop_in_place` over the op stream) went from **5.2% of the profile to ~0** (a `Vec<Op>` now
> frees in one shot).

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
| ~~**Shrink `Op` 32 → 24 B**~~ — **LANDED** (§9). *Not* by boxing the fat variants (that would keep it a drop type); instead the only `Box` (`BrTable.targets`) was moved to a per-function side-table, so `Op` became 24 B **and** non-drop at once. Measured 31.3 → 27.5 ms, runtime unchanged. | ~26 ms (est.) → **27.5 ms (actual)** | done; safe; killed the 5% teardown drop too |
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

---

## 9. Re-profile after write-once landed: the regime is now *decode-bound* (+ `Op` shrink)

Once the **write-once op buffer** landed (Update 1), the profile changed shape enough that the
old "traffic-bound" conclusion no longer holds. Re-profiled with a dedicated loop harness
(`examples/prof_module_new.rs`, which hammers `Module::new(spidermonkey)` and can also drive
wasmi through the identical path), same Instruments/`xctrace` methodology as §6.

### The bottleneck moved off allocation and onto `wasmparser` decode

Self-time buckets, spidermonkey `Module::new`, **both engines profiled the same way**:

| bucket | submilli | wasmi |
|--------|---------:|------:|
| `wasmparser` decode (`BinaryReader::visit_operator` alone = 28% / 39%) | **40%** | **55%** |
| `wasmparser` validate | 11% | 13% |
| alloc / free / memcpy | **10%** *(was 34%)* | 8% |
| our `emit` (write `Op`s) | 7% | — *(inlined into its visitor)* |
| our lowering dispatch | 3% | 0.5% |
| teardown `drop_in_place` over the op stream | **5%** | 0.1% |

The write-once change did exactly what it should: `alloc/free/memcpy` **34% → 10%**. Both engines
are now dominated by the *same* function — `wasmparser`'s `visit_operator` decode — which is the
shared floor the two non-JITs cannot get under with a pre-decoded array.

### Phase-isolation: our decode+validate *floor* already ≈ wasmi's *whole* compile

Stubbing out the lowering delegation (validate + decode only) and re-measuring, interleaved with
wasmi in one thermal window (best-of):

```
submilli full            ~31 ms
submilli decode+validate ~21 ms   ← our irreducible floor
wasmi   full             ~19 ms   ← decode+validate+translate+build IR
```

So our **decode+validate alone (~21 ms) already meets wasmi's entire compile (~19 ms)** — both use
the identical `wasmparser` `FuncValidator`. wasmi just folds its IR translation *into* that pass so
tightly its total ≈ our validate-only. Our **lowering adds ~11 ms on top** — that is where the gap
lived. (This corrects §5's read: the surplus was never mainly the *double* Op write — write-once
already removed that — it is the decode dispatch plus the single 32-byte `Op` write + teardown.)

### The lever taken: `Op` 32 → 24 B and non-drop

`size_of::<Op>()` was **32** and `needs_drop::<Op>()` was **true** — both caused by the *only*
inline `Box` in the enum, `BrTable { targets: Box<[BranchTarget]>, .. }`. Fix: flatten every
`br_table`'s targets into a per-function side-table (`CompiledFunc.br_tables: Box<[BranchTarget]>`)
and shrink the variant to `BrTable(BrTableRange { base, len })`. That `Box` was `Op`'s **only** drop
field, so removing it makes `Op` **non-drop** outright — no need to `Copy`-derive `Op` or `IrHeap`
(which would only spread `trivial_copy_pass_by_ref` churn).

The 32 → 24 B shrink comes along for free but is a *separate* story from drop, and worth being
precise about (measured with `size_of` on synthetic enums): the 24-byte floor is **co-pinned** by
two things, and removing either alone would *not* get below 24 —
- the `MemArg` loads/stores: `MemArg` is 16 B (a `u64` offset + `u32` memory) at **align 8**, and
  the enum discriminant can't hide in its padding across all variants, so it rounds to 24;
- `BrOnCast`/`BrOnCastFail`: their `IrHeap` (8) + `bool` (1) + `BranchTarget` (12) = 21 B payload
  rounds to 24 on its own.
The old inline `BrTable` (28 B: `Box` 16 + `BranchTarget` 12) was what forced **32**; with it gone,
these two co-binding variants set the new 24-B ceiling. Going to 16 B would require side-tabling
`MemArg`'s `u64` offset (touches every load/store) **and** the cast variants' target — not just one.

Result (interleaved A/B/wasmi best-of, one thermal window; CoreMark from `bench_table`):

| metric | before | after |
|--------|-------:|------:|
| spidermonkey `Module::new` | ~31.3 ms | **~27.5 ms** (1.72× → **1.53×** wasmi) |
| pulldown-cmark `Module::new` | 1.31 ms | **1.19 ms** |
| `Op` teardown (`drop_in_place`) | 5.2% of profile | **~0** (single `free`) |
| CoreMark execution score | 194 | **194** (runtime unchanged) |

### What's left

The remaining ~1.5× is now **almost entirely the shared `wasmparser` decode floor plus our
out-of-line per-op lowering** — not allocation, not `Op` traffic. Shaving it further means either a
still-smaller instruction encoding (16 B needs *both* co-binding variants fixed — side-table
`MemArg`'s `u64` offset, which touches every load/store, **and** the `BrOnCast`/`BrOnCastFail`
target) or the architectural **in-place/sidetable** interpreter from §5's table (store no op array,
re-decode at runtime — beats wasmi on startup, at a runtime cost; on-thesis).
