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

### Two follow-up experiments (one flat, one a small win)

Because the profile is **memory-stall-bound**, self-time % is *not* wall-clock — CPU work in the
decode's stall shadow is free. Two things the profiler flagged, tested against wall-clock:

- **`Op` 24 → 16 B** (side-table `MemArg`'s `u64` offset + the `BrOnCast` target): **flat** on
  *both* compile *and* CoreMark. The `Op` write is stall-hidden, so less write traffic buys nothing;
  a smaller hot-loop element didn't move CoreMark either. (This also retroactively shows the earlier
  32 → 24 win was the **non-drop** change, not the size.) Not worth the churn.
- **Recycle `local_types` + `ctrl` across bodies** (one `Scratch` reused for the whole module,
  cleared per function, instead of fresh `Vec::new()` each time — mirrors wasmi's recycled
  translator allocations): **~0.7–1.5 ms real** (spidermonkey ~27.1 → ~26.4 ms). Unlike the `Op`
  write, `Vec` *regrowth* (`grow_amortized`/`realloc`, ~4.5% of the profile) is **not** fully
  stall-hidden — killing it (inclusive `push_mut` 7.9% → 2.0%) shaved a measurable slice. Landed.

### What's left

The remaining ~1.45× is now **almost entirely the shared `wasmparser` decode floor plus the
`visit_operator::<ValidateThenLower>` monomorphization** — our fused visitor decodes ~8 ms slower
than wasmi's leaner `visit_operator::<FuncTranslator>` on identical, feature-neutral decode (matching
wasmi's proposal set changed nothing). Un-fusing to materialize+match is *worse* (that was the old
45 ms path). Closing it means slimming our ~500 lowering methods so `visit_operator` compiles as
tight as wasmi's — speculative; confirm the mechanism in disassembly first. The other known lever is
the architectural **in-place/sidetable** interpreter from §5 (store no op array, re-decode at
runtime — beats wasmi on startup, at a runtime cost; on-thesis).

---

## 10. Session 2 — zero-alloc arena (win), byte-encoding spike, and the wasmi diff

> **⚠️ Skepticism marker.** The *measurements* below are real (same methodology). Several *conclusions*
> are **disputed** and explicitly flagged — in particular the "stall-hidden" framing and the reading of
> what makes wasmi faster. Owner is skeptical byte-encoding is a dead end and skeptical the wasmi IR
> "folding" is a compile-time factor. Next session will re-test inlining + byte-encoding from scratch.
> All of §10 is **uncommitted spike work** on top of the §9 committed state.

### 10a. Zero-alloc arena — a real ~10% win (contradicted a prior prediction)

Moved *all* per-function buffers into module-wide arenas pre-reserved once at the start of
`Module::new` (`ModuleInner.code_ops/code_local_types/code_handlers/code_br_tables/code_offsets`);
`CompiledFunc` became a pure `Copy` struct of `Span` ranges — **no `Box`, no per-function `Vec`,
zero allocation during the per-body translation loop**. De-`Arc` (`functions: Vec<CompiledFunc>` +
a `Code { Arc<ModuleInner>, index }` handle, ops span cached to avoid a per-step lookup) came with it.

| | recycle baseline | zero-alloc arena |
|---|---|---|
| spidermonkey `Module::new` | ~27.7 ms | **~25.0 ms** (~10%; ~1.5× → **~1.41×** wasmi) |
| CoreMark (runtime) | 190 | 188 (noise) |

**This contradicts the earlier "allocation *count* is stall-hidden" claim** (de-`Arc` alone was flat).
The arena win appears to be *write pattern* (one contiguous sequential stream + killing 6596 large
per-function `Vec` mallocs), not count. Profile after: `grow_amortized`/`realloc`/`alloc` from *our*
code effectively gone; the residual is wasmparser-internal. **Owner note: this is the direction that
worked — memory layout mattered, which is why byte-encoding is worth pursuing, not dismissing.**

### 10b. Byte-encoding spike — measured slower, but *disputed / not fully optimized*

Replaced `Vec<Op>` (24 B fixed enum) with a **variable-length byte stream** (`Vec<u8>`, `Span`s,
`ip` = byte offset, branch `ip`s patched in place). A hand-rolled fixed-width codec (1-byte tag +
LE fields), generated from the `Op` enum. **Spec passes** (correct end-to-end).

| version | compile | note |
|---|---|---|
| arena-enum (baseline) | ~25.8 ms | — |
| byte, serde codec | ~35 ms | serde generic dispatch = confound |
| byte, hand-rolled, re-match in `encode` | ~32 ms | double dispatch (wasmparser + encode) |
| byte, hand-rolled, **direct write from `visit_*`** (no `Op`) | ~31 ms | removed the 2nd dispatch |
| byte, direct + **larger arena reserve** (no `Vec` regrow) | ~30 ms | fixed-width bloats size → had under-reserved |

Each confound removed shaved ~1 ms, converging toward — but **not beating** — arena-enum.

**Disputed conclusions (flagged):**
- The write-up claimed the ~44 MB `Op` write is "stall-hidden" (from `dec-only` ≈ full), so a compact
  byte stream buys nothing on compile. **Owner disagrees** — the 7 ms gap to wasmi is real
  implementation difference, and byte-encoding is suspected to be the key.
- **Known un-done optimization:** the direct writes are still *per-field* (`push(tag)` + `extend(mem)`
  + `extend(offset)` = 2–3 `Vec` ops/op), **not** the "build the op's bytes and write **once**" design.
  This is the next thing to try — it's the last confound before a fair verdict.
- Fixed-width LE is *not* compact (u64 offset = 8 B); a real compact/varint encoding was not built.
- Runtime: hand-rolled decode ran spec at ~14 s vs ~12 s baseline (small); serde decode was ~26 s
  (serde was the runtime killer, not bytes).

### 10c. The wasmi diff, read from source (`wasmi-2.0.0-beta.4`)

Confirmed by reading wasmi, not theorising:
- **Validation is identical** to ours — `ValidatingFuncTranslator::validate_then_translate` calls
  `validator.visitor(offset).visit_X(arg.clone())` per op (same `wasmparser` `FuncValidator`, same
  clone). *Not* a difference.
- **`#[inline(never)]` on every translate `visit_*` method** — keeps per-op work **out** of the
  monomorphized `visit_operator`, so the hot decode dispatch stays small. **Ours inline the lowering
  into `visit_operator`.** Strong candidate for the `decode-dispatch +1.4 ms`. *(A quick test of
  `#[inline(never)]` on the fused methods was inconclusive — but it was run on the byte-encoding build,
  which is confounded. Needs a clean test on arena-enum.)*
- **wasmi emits nothing for `i32.const`/`local.get`** — `push_immediate`/`push_local` onto a
  compile-time stack, folded into the consumer (`i32_add_ssi`). We emit an `Op` for each.
  **Disputed:** owner argues this is a *runtime* optimization — wasmi still spends compile CPU managing
  that stack memory, so fewer emitted ops is not obviously the compile-time cause. Unresolved.

wasmi stores code as a **variable-length byte stream** in a reused `Vec<u8>` scratch (recycled across
functions), copied per function into a `CodeMap` — **inline (`SmallByteSlice`, ≤22 B) or one
`Box<[u8]>`**; small functions cost zero allocation. Execution walks a raw `ip` pointer (`unsafe`) —
which we can't (zero-`unsafe` requirement).

### 10d. Per-origin gap breakdown (profiler self-time, arena-enum vs wasmi, ~25 vs ~18 ms)

```
origin              submilli  wasmi   delta
decode-dispatch       8.57    7.14   +1.43   our lowering inlined into visit_operator
our-translate         3.21    0.49   +2.71   we emit const/local ops; wasmi folds them
alloc/mem/teardown    4.29    2.82   +1.47
decode-read           2.75    2.39   +0.36   ~parity
validate              1.63    1.44   +0.19   ~parity (same wasmparser)
other                 3.10    2.13   +0.98
```

### 10e. Next session (agreed plan)

1. **Revert the byte-encoding spike**, back to the clean ~25 ms arena-enum, then:
2. Test **`#[inline(never)]` on our fused/lowering `visit_*`** on that clean baseline (targets the
   decode-dispatch delta directly).
3. Re-do **byte-encoding "write-at-once"** (build each op's bytes in a small stack buffer, single
   `extend`) — the un-done optimization from 10b — and measure fairly.
4. Owner remains skeptical of the "stall-hidden" model; treat §9/§10 conclusions as hypotheses to
   falsify, not settled.

---

## 11. Session 3 — the confound: wasmi's default `Module::new` doesn't translate

> Fresh session, fresh profiles (same `xctrace` methodology, per-compile normalization via
> compile counts), plus source reading of `wasmi-2.0.0-beta.4` and `wasmparser` 0.228/0.252.
> Every §9/§10 gap conclusion needs reinterpreting in light of 11c.

### 11a. Re-profile with better attribution (nearest-meaningful-ancestor, not leaf buckets)

Leaf-symbol bucketing had been mis-filing validator stack ops (`alloc::vec::Vec::pop`/`push_mut`)
into "alloc". Re-bucketed by walking each sample's stack to the nearest engine/wasmparser frame,
per-compile (submilli ~31 ms hot vs wasmi ~19 ms hot, same window):

| bucket | submilli | wasmi | delta |
|---|---:|---:|---:|
| `visit_operator` decode dispatch | 9.6 | 9.3 | **~parity** |
| wasmparser validate | 9.0 | 4.6 | +4.4 |
| translate / lower | 5.3 | 1.0 | +4.3 |
| decode read | 3.5 | 2.9 | +0.6 |
| allocator (from engine code) | 2.5 | 0.9 | +1.6 |

The §9 "decode-dispatch monomorphization" theory is dead: dispatch is at parity (symbol sizes in
the same binary: our `visit_operator` 7.3 KB vs wasmi's 5.1 KB — bigger, but not costlier).

### 11b. The wasmparser-version trail (real but small)

- **wasmi is not on our wasmparser.** It builds **0.228 with `default-features = false`** (no
  component-model, no serde, no hash-collections); we build **0.252 with all defaults**. The
  "identical wasmparser" premise of §5/§9 was false.
- **Pure `Validator::validate_all(spidermonkey)`, 0.228 vs 0.252 side-by-side** (renamed-package
  A/B crate, interleaved): **16.6 ms both, ratio 1.00×** — the validator itself didn't regress.
- But 0.252 grew a decode-side machine 0.228 lacks: `OperatorsReader` now keeps its **own
  syntactic `ControlStack`**, wraps the visitor in a per-op `FrameStackAdapter`, and allocates
  that stack per body. `FuncValidator::validate` internally avoids all of it by driving
  `BinaryReader::visit_operator` directly (its visitor implements `FrameStack`); our fused loop
  went through `OperatorsReader` and paid it.
- **Fix (landed, this session):** implement `FrameStack` for `ValidateThenLower` (backed by the
  translator's own `ctrl` stack, which is structurally complete even in unreachable code) and
  drive `BinaryReader::visit_operator` + `finish_expression` directly.
  **~0.6–0.8 ms real** (consistent across every interleaved round), spec suite green.
- **Slim wasmparser features** (default-features off, wasmi-style): **flat**. Not the mechanism.
- **`#[inline(never)]` on all lowering `visit_*`** (wasmi's pattern, tested clean this time):
  **flat-to-slightly-worse** (min-of-10: 25.8 vs 25.1 ms). wasmi outlines *big* translate bodies
  (register alloc, encoding); ours are a push of a 24-B enum — the call outweighs the i-cache win.
  Reverted. §10c's candidate is dead too.

### 11c. The headline: `CompilationMode::LazyTranslation` is wasmi's default

`wasmi::Config` defaults to **`LazyTranslation`** — `Module::new` **validates but does not
translate**; per-function translation happens at first call. Every wasmi `Module::new` number in
this document is a **validate-only** number. That's why wasmi's "whole compile" (~17.7 ms) ≈ the
pure `validate_all` floor (16.6 ms) ≈ our stubbed decode+validate (§9) — there was never any
tightly-folded translation to explain. Interleaved best-of, spidermonkey, one thermal window:

```
submilli (full compile)          ~25.4 ms
wasmi    default = lazy          ~17.8 ms   (validate only; translation deferred)
wasmi    CompilationMode::Eager  ~52.7 ms   (validate + translate, like ours)
```

pulldown-cmark: submilli 1.17 ms | wasmi-lazy 0.80 ms | wasmi-eager 2.35 ms.

**Apples-to-apples, submilli compiles ~2× faster than wasmi** (both eager). Our surplus over the
shared validation floor is ~8 ms of lowering; wasmi's is ~35 ms. The §9/§10 "gap" was us doing
work the competitor had deferred. (`prof_module_new` now takes `wasmi-eager` to reproduce.)

### 11d. Where this leaves the roadmap

- **The remaining ~7.8 ms gap to wasmi's *default* is translation itself.** Beating a
  validate-only number while eagerly translating means squeezing our translate cost below ~1 ms —
  not plausible. The two on-thesis levers:
  1. **Lazy translation mode of our own** (validate eagerly at `Module::new`, lower per function
     on first call — wasmi/wasmtime precedent). Puts `Module::new` at the ~18–19 ms floor
     immediately, and the §10a arena + this session's reader fix still help the per-call path.
  2. The **in-place/sidetable interpreter** (§5) remains the only design that goes *below* the
     validation floor's add-ons entirely.
- **§10a zero-alloc arena (~2.5 ms, uncommitted)** is still worth re-landing on its own merits.
- **bench_table fairness:** resolved — the harness now runs wasmi with `CompilationMode::Eager`
  (`support.rs`), so every `Module::new` cell measures the same work; the README documents the
  lazy-default caveat alongside the wasmtime opt-level note.
- Byte-encoding as a *compile-time* lever is now moot in this comparison: the encoding wasn't
  why wasmi's compile looked fast — not translating was. (It may still matter for the lazy
  path's per-first-call latency or cache footprint, i.e. as a *runtime* question.)

---

## 12. Session 3b — execution low-hanging fruit: CoreMark ~192 → ~415 (≈2.2×)

Focus shifted to execution (the run-once rows showed it, not startup, is the binding
constraint for the run-once use case). Profiled with `examples/prof_execute.rs`
(CoreMark + the run-once sieve) via the same `xctrace` methodology. Landed changes, each
measured individually (interleaved baseline A/B at the end: CoreMark 192/195 → 413/422;
sieve(1M) ~250 → ~130 ms):

| change | effect |
|---|---|
| **`#[inline(always)]` on `step`** (single call site: the `run` loop) + **`take_branch` early-out** when `pop == 0` (was calling `memmove` per branch for no-op fixups) | **CoreMark 243 → 435 — the big one.** Kills the per-op call/return + the ~40 B `Result<StepOutcome>` stack round-trip; `ip`/`base` live in registers |
| **Typed `push_i32/i64/f32/f64`** (`exec/stack.rs`): write the cell bytes directly + `NONE` shadow tag, skipping `Val` construction and the 4-deep `slot_for_val`→`write_slot`→`write_scalar` codec matches per push. Applied to arith/numeric/memory/consts; also `push_default` for locals init (zero cells / `NULL_REF`) | 206 → 243 (+18%) |
| **One combined `gated` check** for fuel/epoch/GC-pressure (default config: one predictable branch/op instead of three) + **numeric-first dispatch chain** (was gc→gc_array→cast→numeric — every `i32.add` walked 3 failed matches; now numeric→gc→gc_array→cast) | 199 → 206 |
| **Single memory lookup per load/store** — `mem_ea` was resolving `inner.memory(handle)` (with store-handle check) 3× per access | sieve 129 → 124 ms; CoreMark ~flat |

Tried and **reverted**: blanket `#[inline(always)]` on all 24 arith closure helpers —
CoreMark *dropped* ~6% (i-cache bloat in the now-monolithic `run`). The helpers stay
out-of-line.

Profile shape before → after: `step` 36% + `run` 13% + cell/shadow traffic ~24% + a
9% category-cascade + a 9.6% `gc_codec::write_slot` bucket (inlining artifact — really
`cell::encode`) → now one fully-inlined `run` at ~78% self-time, with the arith closure
helpers (~10%) and the memory path (~5%) the visible remainder.

Downstream effects: run-once sieve(10k) 2.48 → 1.51 ms, sieve(1M) 236 → 115 ms; spec
suite wall time 9.4 → 7.6 s. Structural (not low-hanging) next steps if execution stays
a priority: shrink the still-per-op `StepOutcome` protocol, cache the current memory
entity across ops (invalidate on grow/host-call), and compile-time op fusion
(`local.get+local.get+i32.add` superinstructions) — the wasmi register-IR direction.

### Round 2 (line-level profiling): CoreMark ~420 → ~560 (cumulative ≈2.9×)

Symbol-level profiling had gone blind (one inlined `run` at ~78% self), so this round used
**address-level samples symbolicated to source lines** (`dsymutil` + `atos`; scratch script).
That attributed the loop's cost line by line and produced three more wins, each A/B-measured:

| change | effect |
|---|---|
| **Per-frame `ops` slice + single fetch** — the per-op `ip >= code.ops.len()` check was 15%
of wall time (an `Arc`→`Vec`→len chase per op), and `step` then bounds-checked `ops[ip]`
*again*. Now a two-level loop re-derives `ops` on frame change; `ops.get(ip)`'s `None` case
*is* the end-of-function return; `step` takes the fetched `&Op` + `next` | 420 → 454 |
| **In-place binops/unops** (`stack.rs::binop_cells`/`unop_cell`) — result overwrites the
first operand's slot (binop: one truncate; unop: zero stack movement), replacing the
pop/pop/push round-trips in every arith helper | 454 → 494 |
| **`dispatch::<const GATED: bool>`** — monomorphize the loop on "any gate live", so the
default config (no fuel/epoch/GC-watch) runs with **no** gate branch at all; the gated copy
stays cold | 494 → **564** |

sieve(1M): 130 → **85 ms**. Session total: CoreMark **192 → ~560 (≈2.9×)**; gap to wasmi
(~3200) now **~5.7×**, from ~17× at the session start. Run-once rows after both rounds +
the all-code-executed fixture + eager wasmi: sieve(1k) **697 µs (submilli) vs 824 µs
(wasmi) vs 5.7 ms (wasmtime)** — submilli wins the light run-once outright.

### Round 3: secondary-dispatch inlining + compare-and-branch fusion — CoreMark ~660 (≈3.4×)

- **`#[inline(always)]` on `exec_numeric` + `exec_memory`** (each has a single call site,
  inside the already-inlined `step`): **564 → 671 (+19%)** — LLVM threads the secondary
  category `match` into the primary dispatch, so straight-line numeric/memory ops pay one
  table jump, not a call plus a second full match. (This delivered what "flatten the
  category dispatch" was estimated at, without moving any code.)
- **Fused compare-and-branch** (`Op::BrIfCmp { kind, negate, target }`): an i32 relop
  immediately followed by `br_if` (or by `if`, i.e. `br_if_not`) collapses into one op at
  compile time — the relop is *replaced in place*, so `offsets` stay aligned and the patch
  machinery just points at it. Fusion window (`Translator::fusable_cmp`) is set per emitted
  op and cleared at every boundary a label can land on (`block`/`loop`/`if`/`else`/`end`/
  `try_table` — a `loop` header between the pair would otherwise let a back-edge jump *onto*
  the erased `br_if`). Interleaved A/B: **+2–3% CoreMark, sieve(1M) 43 → 45 runs/3s** —
  real but modest; the data-dependent branch itself (the misprediction) survives fusion,
  only the dispatch disappears. Kept.
- Split out: `Op` payload types → `module/op_types.rs`; branch lowering + patch
  machinery → `compile/control/branch.rs`.

Session cumulative: CoreMark **192 → ~660 (≈3.4×)**, sieve(1M) **250 → ~67 ms (≈3.7×)**;
gap to wasmi (~3200) now **≈4.8×**, from ~17×.

### What's left (in rough order of leverage)

1. **More fusion classes** with the now-proven window mechanism (`local.get local.get <op>`,
   `local.get i32.const <op>`, load/store address folding) — each nets a few percent; the
   ceiling is that fusion removes dispatches, not the mispredicting branches themselves.
2. **The parked architectural option**: stitch-style closure/tail-call dispatch — wasmi 2
   landed the same idea via explicit-tail-calls (wasmi-labs/wasmi#1946, "sibling calls");
   stitch is the reference architecture. Owner note: if we go there, be safer than both —
   e.g. bounce through a trampoline after a fixed native-stack budget rather than trusting
   unbounded sibling-call chains. This is now the main lever left for execution: the profile
   is ~50% dispatch blob + ~30% loop scaffolding, exactly what tail-call dispatch attacks.

---

## 13. Session 4 — 16-byte `Op` (arena density lever, part 1)

Motivated by multi-tenant memory density (see the byte-encoding discussion): keep the
fixed-width enum (all its CPU properties — merged aligned stores, one capacity check, one
bounds check, `ip` as index) and shrink the slot instead. `Op` 24 → **16 B**:

- `MemArg.offset` u64 → u32 inline; wide offsets (memory64-only, plus the literal
  `u32::MAX` which collides with the sentinel) demote to a per-function `BigMemArg` pool
  behind `offset == u32::MAX` (`resolve()` = one predictable branch per load/store).
- `BranchTarget.keep/pop` u32 → u16 (`keep` is a label arity, spec-capped at 1000; a
  function whose operand stack outgrows u16 gets a compile *error* — a resource bound,
  never a panic).
- `BrOnCast`/`BrOnCastFail` edges move into the existing `br_tables` pool (packed index,
  bit 31 = nullable), like `br_table` cases.
- Static asserts pin `size_of::<Op>() == 16` (sans `simd`) and `!needs_drop::<Op>()`.

Measured (interleaved, though on a thermally saturated machine):

| metric | 24 B | 16 B |
|---|---:|---:|
| spidermonkey `Module::new` | 27.8 ms | 27.9 ms (flat) |
| **peak RSS, compile+hold spidermonkey** | **161.8 MB** | **127.8 MB (−21%)** |
| steady op stream (1.84 M ops) | 44.2 MB | 29.5 MB (−33%) |
| CoreMark / sieve | — | flat to ~−3% (within throttle noise; re-verify cool) |

Consistent with §9's earlier "24→16 is speed-flat" probe. The remaining density levers,
in order: the §10a module arena + spans + de-`Arc` (kills per-function boxes + `Vec`
slack; ~2.5 ms compile win when spiked), then 8–12 B ops (pool i64/f64 consts, wasmi-style).

### Part 2 — §10a re-landed: module arenas + spans + de-`Arc`

`CodeArenas` on `ModuleInner` (ops / local_types / handlers / br_tables / big_memargs /
offsets — one allocation per stream for the whole module), `CompiledFunc` reduced to a
`Copy` record of `Span`s, `functions: Vec<CompiledFunc>` (one allocation, no per-function
`Arc`/`Box`es), and a `Code { Arc<ModuleInner>, index }` runtime handle. The translator
writes straight into the arenas (per-function bases; zero allocation in the per-body loop).

Two lessons re-learned along the way:
- **Reserve the statistical size, not the upper bound, and never `shrink_to_fit`.** The
  first cut reserved total-body-bytes ops (62 MB) then shrank — reallocation churn across
  size classes ballooned peak RSS to 549 MB over 21 compiles (freed chunks linger in RSS
  accounting until memory pressure). Reserving bytes/2 (~94% accurate; over-reserve is
  untouched pages, i.e. free) with no shrink made allocations steady-size and recyclable.
- **Cache the `Copy` record in the `Frame`.** First cut re-resolved `functions[index]`
  through the `Code` handle on every call/return — a consistent ~4% CoreMark dip. Frames
  now carry the 64-byte record; the dispatch loop reads spans/arity from it directly.

Measured (interleaved vs the 16-B baseline, same window):

| metric | 16 B per-func | + arenas |
|---|---:|---:|
| spidermonkey `Module::new` | 26.5 ms | **24.7 ms (−7%)** |
| **peak RSS, compile+hold spidermonkey** | 115 MB | **44.5 MB (−61%)** |
| CoreMark / sieve | — | flat |

**Session 4 combined (24-B start → 16-B ops + arenas): peak RSS 162 → 44.5 MB (−73%),
compile 27.8 → 24.7 ms, runtime flat.** The density ladder's next rung (8–12 B ops via
const pooling) is now the only encoding lever left, and much less pressing.

---

## 14. Session 5 — the host-call boundary (the product workload's real hot path)

The compute benchmarks (CoreMark, sieve) never exercised what IO-heavy orchestration
guests actually do: cross the guest→host boundary constantly. A new `ping ×100k` row
(execution-only, trivial host fn) measured the crossing at **~189 ns/call vs wasmi
~12 ns and wasmtime ~3 ns** — a 16× gap, three times worse than the compute gap, on
exactly the traffic the product runs.

Profile said: ~35–40% allocator (libsystem_malloc innards), plus per-call
`Engine::func_sig`/`TypeRegistry` walks. The path allocated ≥4 `Vec`s per call
(param-type collect, results-defaults collect, `pop_params`' `split_off` + collect)
and re-materialized the signature from the engine registry every time.

Landed (each A/B-measured):
- **`HostSig` cached on `FuncEntity::Host` at registration** (param types + result
  defaults behind an `Arc`) — the per-call path never touches the type registry.
- **Reused arg/result scratch buffers on `StoreInner`** (taken/returned per call;
  re-entrant calls take a fresh pair, so only nesting allocates): `pop_params_into` /
  `push_results_slice` make the boundary allocation-free in steady state. 189 → 61 ns.
- **Hand-rolled the 1-element decode/encode loops + inlined the scratch accessors**
  (iterator-adaptor overhead was ~9% at per-call frequency): 61 → **~54 ns/call**.

Round 2 (line-level re-profile ranked: `Val` codec 8.6%, park/unpark drops 5.6%,
`sig` `Arc` bump):
- **Direct boundary codec** — `decode_val`/`encode_val` do one match on the type for
  scalars instead of the layered GC-slot codec (three nested matches each way);
  non-scalars fall back to the generic path. 54 → **~42 ns/call**.
- **Swap-based parking** — `exec_slot` is now a plain `Execution` (empty outside a
  host call); the crossing does two `mem::swap`s instead of `Option` take/park with
  placeholder writes and drops.
- **No `sig` `Arc` clone** — the scratch buffers are taken *first*, so the signature
  borrow can fill them and end before the store is mutably borrowed for the call.

Net: **ping ×100k 18.9 → 4.3 ms (4.4×, 189 → ~42 ns/call)** vs wasmi ~13 ns /
wasmtime ~4 ns. The profile now shows the guest's own loop (~48%) with the host-side
remainder diffuse; the residual gap is structural — the dispatch loop exits and
re-enters through `Outcome::HostCall` every crossing (loop prologue, frame
re-derivation, two enum matches, `catch_unwind`, `Caller`). Next tiers if the boundary
must go lower: a typed host-call fast path that skips the `Val` layer entirely
(wasmtime-style `IntoFunc` specialization reading operand cells directly), and keeping
the dispatch loop resident across host calls — both are architecture conversations
(typed-store access from the untyped loop; async parking interplay).

Round 3 — **loop-resident sync host calls** (and the bug it flushed out):
- `run`/`dispatch` are now generic over the store's data type `T`, so the `DoHostCall`
  arm invokes the callback *inside* the dispatch loop (execution swap-parked around it —
  re-entrant `Func::call` still shares the stacks; async host fns still suspend out
  through `Outcome`, a sync loop can't await). `Outcome::HostCall` is gone.
- Cost of genericity: the interpreter monomorphizes in the consumer crate, which
  initially **halved every benchmark** — two fixes: `#[inline(never)]` on `invoke_host`
  (its body wrecked the loop's code layout when inlined into it), and `#[inline]` on the
  hot helper surface (`stack`/`cell`/`frame`/`call` + store accessors, now split into
  `store/accessors.rs`) since cross-crate inlining needs the attribute.
- **The regression exposed a long-standing hot-path bug**: `gc_pressure_watch` armed on
  `is_collecting()` alone, and the default collector is mark-sweep — so every default
  engine ran the *gated* loop, paying a GC-mailbox check (`footprint_over_floor` + an
  atomic read) on **every op** since #27g landed. The mailbox is only ever posted when
  `gc_memory_threshold` is configured; the watch now requires that too.

Net effect on everything at once: host calls **18.9 → 3.5 ms per 100k (189 → ~35
ns/call, 5.4×; wasmi ~12, wasmtime ~3)**; CoreMark **~650 → 715–720** (all-time high);
sieve(1M) **~68 → 57 ms**; run-once sieve(10k) is now a **dead tie with wasmi** (919 vs
920 µs). Remaining boundary gap: `catch_unwind`, `Caller` construction, and the `Val`
codec — the typed host-call fast path is the next tier.

Round 4 — **the async boundary** (the product's real IO path — async host fns):
- New `async ×100k` row (submilli vs wasmtime; wasmi n/a — no async host fns, its answer
  is resumable calls). Baseline: **~200 ns/call vs wasmtime ~26 ns** — the async path had
  received none of the sync optimizations.
- Ported the full treatment: `HostSig` cached on `FuncEntity::HostAsync`, reused scratch
  buffers, swap-based parking across the await, direct codec. **200 → ~73 ns/call (2.7×)**.
- Tried and reverted: fully inlining the await into `drive_async` (flattening the nested
  async-fn state machine) — measured perf-neutral; the layering isn't where the cost is.
- What remains, structurally: the `Outcome::HostAsync` suspend to the driver's await
  point (required — a sync dispatch loop cannot await; going lower means fiber-style
  stacks, which both need `unsafe` and are the wasmtime approach), and the per-call
  `Box<dyn Future>` the wasmtime-compatible API shape mandates.
- Split out: the async driver + helpers moved to `exec/host_async.rs`.

Round 5 — **amortized GC-pressure safepoints** (owner's design): the §14 fix stopped
*default* engines from paying the per-op mailbox check, but a threshold-configured engine
(the multi-tenant product posture!) still selected the gated loop. The mailbox is advisory
— µs of latency is fine — so the check moved off the per-op path entirely:
- every **1024 guest calls** (countdown in the `DoCall`/`DoTailCall` arms — zero per-op
  cost even when armed; call sites are already ~20 ns);
- **after every async host-call await** (the natural long-latency safepoint: other tenants
  generate pressure while this guest is parked) — sync host calls deliberately skip it
  (their path is ~35 ns);
- allocating guests still respond immediately via the reservation path (`DoGcGrow`).
`GATED` now means fuel-or-epoch only. Non-calling, non-allocating spins never check — they
also add no pressure, and multi-tenant deployments run epoch/fuel regardless.
Measured: threshold-configured CoreMark **727** (== the unconfigured fast path; the gated
loop would have held it ~10% lower). Defaults unchanged (717/841 hostcall).

## 15. Session 6 — the wasm.fyi gap: per-op checks, flat dispatch, call path

Trigger: wasm.fyi (JairusSW/wasm.fyi#11) measured submilli 11–15× slower than wasmi at
execution on x86. Two corrections to our own picture first:
- **wasm.fyi builds wasmi with `auto-dispatch`** (tail-call threading) + `simd` + `memory64`
  (`scripts/lib/wasmi-proposals.mjs`). Against that, the gap is 12–19× on Apple Silicon too —
  not x86-specific. Comparing against `portable-dispatch` wasmi (~4× slower than tail-call)
  is what once made it look like "~4×, x86-only".
- The README's CoreMark ratio (vs the pinned `2.0.0-beta.4`) is stale: 2.0.0 is ~1.75× faster.

Goal set with the owner: **wasmi 1.x speed** (same `loop { match }` dispatch as ours; no
tail-call threading). Baseline gap vs wasmi 1.1 (M4 Pro, simd off): CoreMark 6.2×,
memory/sum 9×, memory_tree 8×, linked_list 7.6×, fib 3.9×, per-call 20×.

**Methodology.** A submilli-only driver over the wasm.fyi workloads (artifacts from
`/api/artifacts/<sha>.wasm`), compared by **min cycles / instructions from `/usr/bin/time -l`**
— wall time was unusable under a co-tenant's 10-core load, and cycle counts hold up under it.
Address-level `xctrace` samples, symbolicated with `dsymutil` + `atos`, as in §12.

| change | effect (cycles) |
|---|---|
| **Per-op stack checks**: `ensure_operands` re-walked the frame stack (`frames.last()`), compared value/shadow lengths and scanned shadow tags for every scalar op, out of line from 23 call sites — ~30% of CoreMark. Now inline: the lockstep compare + one compare against a **cached frame floor** (`Execution::floor`, re-synced at the 7 frame push/pop sites). The scalar tag scan is a debug assertion (reading a ref as a number can't forge one; ref pops keep their tag checks). The fail-closed underflow/lockstep tests pass unchanged — keeping them costs ~0 vs stripping them | loops **1.47–1.58×**, fib 1.13× |
| **`take_branch` `pop == 0` early-out** — §12's fix had been lost; every branch paid the checks + two zero-length `memmove`s | (in the row above) |
| **`Func::call`**: signature materialized ~6×/call (registry lock + `Vec`s each) → once, then cached per module type (`ModuleInner::func_sigs`, lazy `OnceLock`); top-level `Execution` stacks recycled in the store (≤ 64 KiB, emptied) | per-call **3.8×** |
| **`step` returns the next `ip`** (`Result<u32>`); calls/grows leave a `StepOutcome` in `Execution::control` behind the `CONTROL` sentinel, instead of every op returning the large enum | instructions −16–19%, cycles ±0 alone |
| **Flat dispatch**: numeric + load/store arms moved into `step`'s single `match`. §12 Round 3's "LLVM threads the secondary `match` into the primary" no longer held — the disassembly showed a second jump table per numeric/memory op | instructions −20–31% (with the row above); cycles ~neutral |
| **SIMD immediates pooled** (`CodeArenas::v128s`): `V128Const(u128)` (16-aligned) made `Op` 48 B under `simd`; now 16 B with the size assert unconditional. Artifact v13 | `simd` build ≈ default-build gains |
| **Call path**: one `Arc` bump per call (was ~6: `resolve`, `call_outcome`, push, `top`); `top_into` retargets the loop's `Code` in place (out of line — inlined it shifted the hot loop's layout: same instructions, +8–15% cycles); `Frame` caches only `{ops, n_results}` (120 → ~68 B); single-value return/branch fixups skip `memmove` | fib **1.12×** |

Review hardening (code-review + security + no-panic passes) restored fail-closed checks
cheaply: the scalar-op shadow-tag check runs in release again (the in-place binop/unop helpers
keep the first operand's tag, so a ref reaching a numeric op would forge a root), pop-free
branches check their `keep` underflow, a stray `CONTROL` is an internal error, `slide_down`
validates its own range, recycling requires a pristine execution, and a debug assertion pins
the cached floor to the frame stack. Instruction counts were unchanged by these.

Cumulative vs `main`: instructions **1.86×** fewer on CoreMark, 1.89–1.96× on the loops,
1.37× fib, 4.8× per call (simd build within ~5%). Cycles: default build CoreMark **1.92×**,
loops 1.96–2.08×, fib 1.33×, per-call 3.9×; simd build 1.55× / 1.52–1.57× / 1.29× / 3.9×.
**Treat the loop cycle numbers as ±20%:** the review hardening kept instruction counts flat
yet moved default-build cycles 1.2–1.3× — hot-loop code layout/alignment, which a change
anywhere in the giant dispatch function can shift (see also the `top_into` row). Instruction
ratios are the stable signal. Gap vs wasmi 1.1 (wall, before the hardening layout shift):
CoreMark **3.6×**, memory/sum 5.2×, memory_tree 5.7×, linked_list 4.1×, fib 2.8×, per-call
5.2× (from 6.2× / 9× / 8× / 7.6× / 3.9× / 20×).

Where it stands — **dispatch-bound**: instruction-level samples put ~48% of the CoreMark loop
on three loop-control instructions (the post-op join, the jump-table load, the loop head):
indirect-branch misprediction + dispatch latency, which no per-op trimming touches (hence
the instruction cuts above not moving cycles). The lever left is *executing fewer ops*:
50–58% of executed ops are `local.get/set/tee` + `i32.const` (CoreMark, memory_tree, fib).
That is the register-IR step (static-height slots, operand providers, `local.set` fusion) —
also where `v128`-as-two-cells removes the remaining ~18% `simd` cost (16-byte `Cell`).

## 16. Session 7 — step 2a: static-slot operands (no runtime stack pointer)

The first stage of the register-IR step (owner decisions: static slots, not wasmi-2.0
accumulators — they only pay off with tail-call threading and cost compile time; GC roots stay
on the runtime shadow; fuel stays 1 per executed op — SUB-2136; compile budget +15%).

**Design.** wasm's operand height is static at every op, so the compiler gives each operand a
fixed frame slot (`n_locals + height`) and every op carries its slot (`Op` stays 16 bytes):
- *Hot* ops (numeric, locals/consts/globals, loads/stores, branches, calls, tables/refs) read and
  write slots directly — no push/pop, no `Vec`-length dependency chain through memory.
- *Cold* ops (GC aggregates/casts, SIMD, host-call args) are **stack windows**: they carry `top`
  and keep their pop/push handlers over a virtual `sp` set on entry — ~270 handlers unchanged.
  Two payloads moved to pools to fit `top`: `br_on_cast` heap types (`cast_heaps`) and SIMD
  memargs (the function's `big_memargs`).
- Frames own `[base, base + n_locals + max_operands)`; the buffer grows (zeroed) on call and shrinks
  on return; branch fixups are static slot copies; returns copy results from the operand base to
  the frame base. `drop` emits nothing.
- GC roots stay precise: each frame records `live_end` at safepoints (calls, windows, `throw`,
  host returns, caught exceptions); the collector scans only live ranges and clears the tags of
  the dead slots. The epoch callback (an arbitrary op, where the top frame's `live_end` is stale)
  widens the top frame to the buffer end — sound because of that clearing.
- Frame pushes are charged against `max_wasm_stack` before allocating (a statically-sized frame
  could otherwise claim a huge extent in one call); `local.tee` reuses `LocalSet`.

**Migration that stayed green.** During the transition `values.len()` still equaled frame base +
static height, so migrated ops wrote their slots and re-synced the length for the unmigrated ones,
with a check that each op's slot ended at the live stack top (it caught slot-math errors as test
failures). The spec suite ran after each category: numeric → core → memory → branches → calls →
refs/tables → GC → SIMD → final (remove the length bookkeeping). Two bugs surfaced only in the final
step: a re-entrant host call moved the shared `sp` under the outer host call (results now pinned to
`HostCallState::results_at`), and a host tail-called from the boundary frame left stale cells above
its results.

**Review fixes** (code review, no-panic review): `drop` must clear the pending `br_if` compare
fusion (it no longer emits an op, so `i32.lt_s; drop; br_if` fused the dropped compare); the
stale-`live_end` GC roots above; absolute heights (block ends, landing pads) now count toward
`max_operands`. None moved the benchmarks (all within ±3%).

**Measured** (cycles, M4 Pro) vs step 1: loops +6–12% (default) / +7–19% (simd); calls −12% (fib)
/ −7% (per call) — the frame push/pop does more (geometry, extent/live bookkeeping), to revisit in
2b. Cumulative vs `main`: CoreMark **2.0×** (simd 1.74×), loops 2.17–2.26× (simd 1.68–1.81×), fib
1.16×, per-call 3.8×. `Module::new`: **+4% instructions** (spidermonkey, pulldown-cmark, CoreMark) —
within budget.

**Why only this much:** 2a removes per-op stack bookkeeping but not ops — the loop is still
dispatch-bound (§15). 2b (operand providers: free `local.get`/consts, `local.set` fusion) cuts the
50–58% of executed ops that are pure data movement; it builds on these slot operands.

## 17. Session 7 — step 2b: operand providers + `local.set` fusion

**Design.** Numeric ops, loads and stores became three-address (`I32Add(dst, lhs, rhs)`, still a
16-byte `Op`); `local.get/set/tee` share `Copy(dst, src)`. The translator keeps, per stack entry,
the slot its value lives in: `local.get` emits nothing, and entries are materialized into their
own slot only where needed (positional ops, branch kept values, block ends and entries). Writing a
local copies out entries still reading it; `local.set`/`tee` of the previous op's result retargets
that op's `dst` (fusion). Constant providers are deferred to 2c: without immediate operand forms a
consumed constant still needs its slot, so they would save almost nothing.

**Staged, green at each stage** (spec suite + a new `tests/operand_providers.rs` oracle grid):
encoding only (stage A, providers all in their own slots: 7–20% *slower* — three independent slot
reads bounds-check separately where adjacent slots shared checks) → local providers → fusion.
Two fusion bugs surfaced on the way: a pending compare survived a `local.get` (which emits
nothing) and fused into a `br_if` testing that local; fusion now requires the branch's condition
slot to *be* the compare's result slot (which also subsumes the `drop` fix from 2a).

**Ops executed** (vs 2a): CoreMark 54.6M → 38.8M after providers (−29%), and fusion removes most
remaining `local.set` copies; tiny/add 3 ops → 1.

**Measured** (cycles, M4 Pro) vs 2a: loops 1.38–1.51×, CoreMark 1.16×, fib 1.05×, per call 0.96×.
Cumulative vs `main`: CoreMark **2.29×** (simd 2.47×), loops 2.9–3.4× (simd 3.07–3.45×), fib
1.23× (simd 1.10×), per call 3.6× (simd 3.3×). `Module::new`: +9–11% instructions vs 2a,
**+13–16% vs `main`** cumulative — at the budget (spidermonkey 16.4%, pulldown-cmark 15.2%,
CoreMark 13.3%); the per-emit fusion matches had to move into the helpers that produce fusable ops
(+17–22% before), and the compile-time profiling pass should take it back down.

**Review** (correctness: ~310k random functions differentially fuzzed against V8, no miscompile;
hardening): two compiler DoS holes, both fixed with regression tests. (1) Provider scans walked the
whole operand stack while any local entry was live — quadratic compile time (a 640 KB body took
3.9 s vs 1.4 ms in 2a). Now at most 16 entries read locals at once (a further `local.get` is copied
right away, as in 2a), so every scan is bounded; the per-local read counts tried first cost ~5%
compile time. (2) Dead code pushed block results it never popped — compiler memory and
`max_operands` grew per dead block (a 1.2 MB module reached 808 MB). A label followed only by dead
code now pushes nothing; this also fixed a pre-existing rejection of valid modules (`unreachable` then a block
taking params from the polymorphic stack).

**Where the time goes now** (CoreMark profile): about half of `dispatch` is the dispatch itself
(loop back-edge + `match` jump), about a quarter slot access (`scalar_at`: two vectors, two bounds
checks, headers reloaded after each slot write). Next levers: 2c immediate/compare forms (constants
are 17–30% of executed ops) and one combined value+tag slot vector (one bounds check per access).

## 18. Session 7 — step 2c: immediate operands + the call path

**Immediates.** After 2b, `i32.const` was 20–31% of executed ops, mostly feeding `i32.add`, `and`,
`sub`, compare+branch, shifts and `mul`. An i32 binop whose operand is the constant the previous op
just pushed now drops that op and emits an immediate form in its place (`I32AddImm`; `sub` as
`add` of the negation; commutative ops and mirrored compares also fold a left constant). Compares
against an immediate fuse with `br_if`/`if` into `BrIfCmpImm`; `i32.eqz` fuses into a plain
`BrIfNot`/`BrIf`. Cost: ~4.5% `Module::new` instructions (the fold path re-does a binop emission
and removes the constant op).

**Wasm→wasm calls** (fib profile: `top_into` 13%, buffer growth 8% + memset, `Arc` traffic):
- Frames no longer hold an `Arc<ModuleInner>`; they record the module's address (compare-only)
  and the function index. A call resolves to a `Callee` (no clone) and the run loop re-points its
  one `Code` handle, cloning only on a module switch; a call no longer goes through `top_into`.
  The handle is stashed across suspensions (re-pointed on resume). fib 1.38× on its own — the
  atomics were mostly cycles, not instructions.
- The operand buffer only grows within a call (returns leave slots for reuse — no per-call zero
  fill); the stack charge follows the live top (top frame's end), boundaries trim to their
  results, and a host re-entry trims to the live top first. fib +8%. Review fix: a frame's `end`
  includes its caller's (a callee starts inside the caller's operand area, so its own slots can
  end lower) — without it a host re-entry from a small callee trimmed the caller's slots away.

**Host→wasm calls** (tiny profile: ~14% malloc/free): arguments are borrowed unless one needs
internalizing (externref→anyref), results decode straight from the operand buffer into the
returned `Vec` (no intermediate `Vec<Cell>`), and the duplicate result validation is gone. 1.44×.

Fuel is still 1 per executed op, so folding and fusion lower fuel consumption further (one async
yield test assumed more ops per loop iteration and now uses a 1-unit slice).

**Measured** (cycles, M4 Pro) vs 2b: CoreMark 1.38×, loops 1.40–1.52×, fib 1.69×, per call 1.44×.
Direct race against wasmi 2.0.0 built like wasm.fyi (M4, wall-clock medians, fresh instance per
sample; wasmi with simd, submilli default): memory/sum 3.1×, memory_tree 3.5×, CoreMark 3.8×,
linked_list 3.8–4.5×, per call 4.0×, fib(28) 4.8× (from 10.7–20.8× on `main`).
Cumulative vs `main`: CoreMark **3.28×** (simd 3.36×), loops 3.9–5.0× (simd 4.4–5.3×), fib 2.08×
(simd 1.72×), per call 5.5× (simd 5.2×). Estimated gap to wasmi 2.0 on wasm.fyi: CoreMark ~3.8×,
loops ~3.5–4.3×, fib ~5.3×, per call ~3.8×. `Module::new`: **+19–22% vs `main`** (immediates
+4.5%); the compile-profiling pass is owed.
