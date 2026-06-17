# Implementation Plan

Companion to [ARCHITECTURE.md](./ARCHITECTURE.md). Nine phases (0–8), each independently shippable and testable. Earlier phases never need rework for later ones — the `Value` enum, the `Op`/branch machinery, and the resumable loop are designed up front to absorb every later proposal.

Legend: **Deliverable** = what exists at the end; **Done when** = acceptance criteria (the gate to move on); **Risks** = what to watch.

---

## Phase 0 — Project skeleton

**Goal:** a compiling crate with the public type stubs and test harness wired up.

**Deliverable**
- Cargo crate `submilli-wasm` with deps `wasmparser` + `anyhow`; dev-deps `wast`, `wat`, and a pinned `wasmtime` (45.x, for compatibility/differential tests).
- Empty-but-named modules per the ARCHITECTURE source layout.
- Public API *types* declared with **`wasmtime`-matching signatures** (Engine, Config, Store, Module, Instance, Func, TypedFunc, Caller, Memory, Global, Table, Linker, Val, Ref, Extern, Trap, AsContext/AsContextMut) — mostly `todo!()`. `Result<T> = anyhow::Result<T>`.
- WebAssembly testsuite vendored as a submodule at `tests/testsuite` (see [TESTING.md](./TESTING.md)).
- Spec-test runner scaffold (`tests/spec.rs`) that parses a `.wast` with `wast`, iterates directives, registers a `spectest` shim, and reports pass/fail against a per-phase skip allowlist (initially all skip).
- A `use submilli_wasm as wasmtime;` smoke test that imports the public types (locks in name/signature parity early).
- CI: build + test + clippy + fmt.

**Done when:** `cargo build`, `cargo test` (empty), `cargo clippy` all green; the `.wast` runner executes against one trivial file and reports; the alias smoke test compiles.

**Risks:** none significant. Keep the `.wast` runner generic so every later phase just enables more directives; lock signatures to the pinned `wasmtime` version from day one to avoid compat drift.

---

## Phase 1 — Core interpreter (MVP + multi-value + sign-extension + mutable-globals)

**Goal:** instantiate and run real core-wasm modules end to end.

**Scope**
- Compile pass (`compile.rs`): wasmparser validation + single-pass pre-decode to `Vec<Op>`; control stack; forward/backward branch resolution with `keep`/`pop`; dead-code elision; constant folding.
- Internal `Op` set for: numeric (i32/i64/f32/f64 full set incl. sign-extension ops), parametric (`drop`, `select`/`select t`), variable (`local.*`, `global.*`), memory (all loads/stores, `memory.size/grow`, `memory.init/copy/fill`, `data.drop`), control (`block/loop/if/else/end/br/br_if/br_table/return/call/call_indirect/unreachable/nop`).
- Runtime: operand stack + frame stack + the `run` loop; zero-copy call args; multi-value blocks/calls/returns; the folded-branch executor.
- Entities: `Memory` (Vec<u8> + bounds checks + grow), `Global` (incl. mutable, imported/exported), `Table`, `Func` (wasm only).
- Instantiation: imports (positional), active/passive data & elem segments, `start` function.
- Minimal embedder API: `Engine`, `Store<T>`, `Module::new`, `Instance::new`, `Func::call` (untyped `&[Val]`), entity accessors. `AsContext`/`AsContextMut`.
- Trap model + numeric trap semantics (div-by-zero, int overflow on trunc, OOB).

**Done when:**
- The **self-contained** MVP / `multi-value` / `sign-extension-ops` / `mutable-global` **spec `.wast` suites pass** — the files that don't import the `spectest` host module (allowing for genuinely out-of-scope directives, e.g. SIMD). The ~22 `spectest`/import-dependent files (`data`, `elem`, `start`, `linking`, `imports`, …) need host functions + `Linker` and are gated at the **end of Phase 2**; the table-side bulk ops (`elem`/`bulk`/`table*`) land with the bulk-memory table-op task and Phase 4 (reference-types).
- Can instantiate a non-trivial hand-written module and call exports via `Func::call`.

**Risks:** multi-value branch arity (loop = params, block/if = results) and `keep`/`pop` computation are the subtle bits — cover with targeted unit tests before relying on the spec suite. Float semantics (NaN propagation, rounding) need care.

---

## Phase 2 — Embedder API depth (linker, typed calls, fuel, epoch, limits)

**Goal:** the full execution-control surface, sync.

**Scope**
- `Linker<T>`: `define`, `func_wrap`, `func_new`, `instance`, `module`, `instantiate`, `get`; multi-module instantiation; imported mutable globals aliasing the same store cell.
- Typed calls: `IntoFunc`, `WasmParams`/`WasmResults`, `Func::wrap`, `Func::typed`, `TypedFunc::call`, `Instance::get_typed_func`. Macro-generated tuple impls.
- Sync host functions: `Func::new`/`wrap`, `Caller<'_,T>` with `data()/data_mut()/get_export()`; host `Err` → trap.
- **Fuel**: block-batched charging in the compile pass/loop; `Config::consume_fuel`, `Store::set_fuel/get_fuel`; `Trap::OutOfFuel`.
- **Epoch**: `AtomicU64` on `Engine`, `increment_epoch`, `Store::set_epoch_deadline`, deadline checks at back-edges/calls, `Trap::Interrupt`; ticker helper using a weak engine handle.
- **Limits**: `ResourceLimiter` trait + `StoreLimitsBuilder`, `Store::limiter`; enforce on memory/table grow and instance/entity counts.
- **Stack-size limit**: enforce `Config::max_wasm_stack` — wasmtime's only stack knob and, like wasmtime, measured in **bytes** (not a frame-count depth; the spec defines no stack bound at all). Since we use no native stack for wasm calls (explicit heap `Vec<Frame>`/`Vec<Val>`), account those stacks' estimated byte footprint (frames × overhead + operand slots × `size_of::<Val>()`) against the budget; exceed → `Trap::StackOverflow` ("call stack exhausted"). The third execution-control limit alongside fuel and epoch; hardening/verification is Phase 8.

**Done when:**
- Host functions can read/write guest memory; multi-module linking works; typed and untyped calls agree.
- Fuel exhaustion traps deterministically at the same instruction; epoch deadline traps; limiter denies/【traps】 grows as configured; unbounded recursion traps `StackOverflow`.
- API tests mirroring wasmtime's fuel/epoch/linker/host-fn examples pass.
- The ~22 `spectest`/import-dependent core spec files deferred from Phase 1 now pass via the linker-registered `spectest` shim (except any also needing table-bulk/reference ops, which land with the bulk-memory table-op task and Phase 4).

**Risks:** fuel determinism (charge based on input-level cost, not incidental compile choices) — document the cost model. `IntoFunc` macro ergonomics.

---

## Phase 3 — Async & resumability

**Goal:** async execution, async host functions, cooperative yielding — all via the resumable state machine.

**Scope**
- `Execution` made fully save/restore; `Step::Suspend(SuspendReason)` plumbed through the loop.
- `TypedFunc::call_async` / `Func::call_async`; async driver loop awaiting suspends.
- Async host functions: `func_wrap_async`/`func_new_async` returning boxed futures; pending future captured as `SuspendReason::HostAsync`, awaited, result resumed.
- Fuel-yield and epoch-yield variants (yield to executor instead of trap); fuel yield interval; `UpdateDeadline`-style epoch policy (trap / continue / yield).
- Async feature flag; sync entry points reject async host fns and treat fuel/epoch as traps.
- Concurrency test: many `Store`s on one shared `Engine` driven concurrently by the executor.

**Done when:**
- A wasm module calling an async host fn that `await`s I/O runs to completion under an executor.
- Long-running wasm yields on fuel/epoch and resumes; concurrent stores make independent progress.

**Risks:** lifetime/`Send` bounds on async host closures and the in-flight future; keeping `Execution` self-contained (no borrows into transient state) so it can be parked.

---

## Phase 4 — reference-types + function-references

**Goal:** references, typed references, and the table/ref instruction set.

**Scope**
- `Value::Ref` activated: `funcref`/`externref`, `ref.null`, `ref.func`, `ref.is_null`; full `table.get/set/size/grow/fill/init/copy`, `elem.drop`; annotated `select t`.
- `externref` arena + `Rooted`/scope API on the embedder side; `ExternRef::new`/`data`.
- function-references: `(ref null? $t)` types, subtyping, `call_ref`/`return_call_ref` (null-trap, no runtime type check), `ref.as_non_null`, `br_on_null`/`br_on_non_null`.
- Validator extensions: the `C.refs` declaration rule (declarative elem segments); **non-nullable local init tracking** with block-scoped rollback; defaultability rules.
- Type canonicalization for funcref `(ref null func)` abbreviation equivalence.

**Done when:**
- `reference-types` and `function-references` spec suites pass.
- `br_on_null` vs `br_on_non_null` value placement is correct (regression test the mirror-image semantics).
- Non-nullable local init validation accepts/rejects per spec (block-scoped init does not persist).

**Risks:** init-tracking algorithm correctness; `ref.func` requiring declaration; not adding a spurious type check to `call_ref`.

---

## Phase 5 — Garbage collection

**Goal:** struct/array/i31, casts, and a working mark-sweep collector.

**Scope**
- Heap: handle table + object headers (canonical type id, mark bit, array len); `i31` unboxed; unified heap.
- Type system: rec groups, `sub`/`final`, subtyping (struct width+depth, array depth, mutable-field invariance), canonicalization + interning; the three disjoint hierarchies + bottoms.
- Instructions: `struct.new[_default]`, `struct.get[_s/_u]`, `struct.set`; `array.*` (new/new_default/new_fixed/new_data/new_elem/get/set/len/fill/copy/init_*); `ref.test`, `ref.cast`, `br_on_cast`, `br_on_cast_fail`, `ref.eq`; `ref.i31`, `i31.get_s/_u`; `any.convert_extern`, `extern.convert_any`.
- **Mark-sweep collector**: non-moving, stop-the-world. No write barriers and no refcount field — the hot store/`local` paths stay plain moves. Precise root enumeration (globals/tables/operands/locals/`exnref` payloads/host roots), unified trace through reference fields, sweep frees unmarked slots and bumps the slot generation. Collects cycles; pauses to collect (acceptable — runtime speed is secondary).
- GC heap limits via the limiter.

**Done when:**
- `gc` spec suite passes (modulo any out-of-scope interactions).
- Collection reclaims garbage including cycles; a stale `GcHandle` reused after sweep faults via the generation check; cast/test/canonicalization across two modules defining the same struct agree.
- ⚠️ Audit: no relative/canonical type-index confusion (the CVE-2024-12053 class).

**Risks:** canonicalization is the hard part; the relative-vs-canonical index hazard is a security-critical correctness bug. Get root enumeration exhaustive — a missed root frees a live object; when in doubt, scan conservatively (the safe direction).

---

## Phase 6 — Exception handling

**Goal:** `exnref` + `try_table` on the existing branch/handler machinery.

**Scope**
- Tag section; tag identity by store address.
- `exnref` value + exception instances (tag + args).
- `throw`, `throw_ref` (null-trap), `try_table` with `catch`/`catch_ref`/`catch_all`/`catch_all_ref` compiled into `BranchTarget`s + handler records on the frame.
- Unwinding: in-frame handler search (tag-address match), operand-stack restore, payload push, cross-frame propagation, uncaught → embedder. `ExnRef` payloads as GC roots.
- (Optional) decode-only acceptance of legacy `try/catch/delegate` for compat.

**Done when:**
- `exception-handling` spec suite passes; throw/catch across call frames works; `throw_ref` reproduces the instance after `catch_all_ref`; uncaught exceptions surface to the embedder.

**Risks:** matching tags by address not signature; mandatory stack restoration on catch; correct payload arity per clause (the `_ref` variants include the trailing `exnref`).

---

## Phase 7 — DWARF debug info & symbolicated backtraces

**Goal:** every trap and every uncaught exception carries an accurate, source-level backtrace; DWARF shipped in the guest module is retained and used to symbolicate frames — matching `wasmtime`'s API, and getting the exception-propagation case *right* where `wasmtime` currently doesn't.

**Scope**
- **DWARF index:** read the debug custom sections (`.debug_info`, `.debug_abbrev`, `.debug_line`, `.debug_str`, `.debug_ranges`/`.debug_rnglists`, …) via `wasmparser` custom-section access + `gimli`; build a compact per-`Module` map from code offset → `(function, file, line, column)` plus inlined-frame chains. Also retain the `name` custom section for symbolication when DWARF is absent.
- **Capture:** `WasmBacktrace` is built by walking the explicit frame stack (§7) at the point of trap/throw, recording `(func_index, code_offset)` per frame — cheap, no symbolication yet. `FrameInfo`/`FrameSymbol` expose `func_index`/`func_name`/`module_offset`/`func_offset` and `name`/`file`/`line`/`column`.
- **Lazy symbolication:** raw `(func, offset)` frames are resolved to source locations only when `frames()`/`symbols()` is inspected, keeping the trap/throw path off the DWARF cost — consistent with "fast startup ≫ runtime".
- **Config knobs (wasmtime-named):** `Config::wasm_backtrace` (capture on/off), `Config::wasm_backtrace_details(WasmBacktraceDetails::{Enable,Disable,Environment})` (gate DWARF file/line resolution), `Config::debug_info` (retain DWARF; the native-debugger aspect is a no-op for an interpreter, but the DWARF is still used for backtraces).
- **Exception-path correctness (the `wasmtime` gap):** capture the backtrace at `throw`/`throw_ref` time — snapshotting the full chain from throw site outward *before* any frame unwinds — and carry it on the exception/error as it propagates. A caught-then-rethrown exception (`throw_ref`) keeps its original throw-site backtrace; an uncaught exception surfaces to the embedder with all frames from throw site to boundary intact.

**Done when:**
- A trap in a DWARF-built module yields a backtrace with correct `file:line:col` per frame, matching source.
- An exception thrown N frames deep and left uncaught reports all N frames from throw site to the embedder boundary — regression-tested against a module/scenario where `wasmtime` drops or garbles the exception backtrace.
- `throw` → `catch` → `throw_ref` preserves the original throw-site backtrace.
- `wasm_backtrace(false)` disables capture entirely; `wasm_backtrace_details(Disable)` keeps frames but drops file/line.

**Risks:** DWARF parse cost vs. the startup-speed priority — keep it lazy and off the compile path. Correct code-offset → `.debug_line` mapping (offsets are module-relative to the code section, matching `FrameInfo::module_offset`). Inlined-frame expansion. Threading the captured backtrace through the §15 unwinder without coupling it to frame teardown.

---

## Phase 8 — Security hardening & multi-tenant isolation

**Goal:** make the interpreter safe to run **untrusted, mutually-distrusting guests** ("tenants") in one process. Every guest-reachable path is bounded (memory, CPU, stack, GC heap, allocation), every input is validated, and **no validated guest can panic or hang the host**; isolation between tenants is enforced and written down as a threat model.

Several primitives are *designed* in earlier phases — the limiter (Phase 2), fuel/epoch (Phase 2), the stack-size limit (Phase 2), GC-heap limits and the type-index audit (Phase 5). Phase 8 is the **gate that proves they actually hold under adversarial input**, plus the items with no earlier home (panic-safety audit, validation-time limits, fuzzing, the threat-model doc). Spatial memory isolation is already strong by construction: every linear-memory access is a software bounds check in **safe Rust** (zero `unsafe` in the tree), so there is no out-of-bounds read/write to harden — the work here is **resource** isolation and **panic/DoS** safety.

**Scope**

*Resource bounds (DoS):*
- **Stack-size limit** — verify `Config::max_wasm_stack` (bytes, as in wasmtime) is enforced against our heap frame/operand stacks → `Trap::StackOverflow` (impl lands in Phase 2; hardened here).
- **Limiter coverage** — route *every* growth/allocation/count path through the installed `ResourceLimiter`: `memory.grow`, `table.grow`/`fill`/`init`/`copy` growth, instance/table/memory/global counts at instantiation, and GC-heap allocation (Phase 5). The **no-limiter default must be a documented finite ceiling**, never "unbounded".
- **Validation-time limits** — bound module complexity at parse via `wasmparser`'s limits (max function body size, locals count, declared table/memory sizes, control-nesting depth, element/data segment sizes) so a hostile *module* can't OOM the compiler before it ever executes.
- **CPU bound** — confirm fuel and/or epoch interrupts *any* guest loop, including tight host-call-free loops (charge/check points exist on every back-edge). At least one must be mandatory for multi-tenant; document the recommendation.
- **Instantiation runs guest code** — the `start` function (and active-segment initializers) execute during `Instance::new`, *before* any export is called (see ARCHITECTURE §8 "Instantiation & the start function"). So fuel/epoch/stack limits must already be **armed before instantiation**, not just before the first export call — a hostile module can otherwise burn unbounded CPU or recurse in `start`. Verify limits apply to start, and document "configure limits before `Instance::new`."
- **Failed-instantiation rollback** — instantiation is currently not transactional: entities allocated before a trapping segment/`start` linger in the store until it is dropped. Decide and document the multi-tenant stance (e.g. instantiate each tenant attempt in its own short-lived store, or add rollback) so repeated failing instantiations can't accumulate dead entities in a long-lived store.

*Information disclosure / zero-on-allocation (no cross-tenant memory leakage):*
- **Invariant:** no guest can ever read memory it didn't write — not another tenant's freed heap, not the allocator's prior contents, not a recycled buffer. This holds **today by construction**: every guest-visible allocation is explicitly initialized (linear memory `vec![0;…]`/`resize(..,0)`, tables `vec![init;…]`, locals `Val::default_for`), and **zero `unsafe`** means spare `Vec` capacity is unobservable in safe Rust. Phase 8 makes it an *enforced* invariant, not an emergent one.
- **Guard the temptation:** the startup-speed priority makes uninitialized-fast-paths attractive and dangerous. Explicitly forbid (CI/review gate + threat-model note): `set_len`/`MaybeUninit` to skip zeroing on `memory.grow`; any **pooling/recycling allocator** that reuses a buffer across instances/stores **without zeroing on reuse-or-return**; `Config::memory_reservation` pre-reserving capacity that a later grow could expose unzeroed. If a pool is ever added for startup speed (likely), zero-on-reuse is mandatory and must be fuzz/test-covered.

*Panic-safety (a host panic = whole-process DoS under multi-tenant):*
- Audit every guest-reachable `unwrap`/`expect`/slice-index/`as`-truncation/arithmetic for reachability from **validated** input. Invariant: *no validated guest can panic the interpreter.* Convert any reachable case to a trap; gate the exec hot path with clippy `deny`s (`unwrap_used`, `indexing_slicing`, `arithmetic_side_effects`) with documented carve-outs where validation guarantees the bound.
- **Contain host-function panics** at the call boundary so one tenant's host-fn panic can't poison the shared engine (match `wasmtime`).

*Isolation correctness:*
- **One `Store` + one `Linker` per tenant** is the documented isolation unit. Verify a handle (`Func`/`Memory`/`Global`/`Table`/`Instance`) from store A used with store B **errors or traps, never UB** (store-binding check).
- Re-confirm the **zero-`unsafe`** invariant with a CI grep guard; document that spatial isolation rests entirely on safe-Rust bounds checks.
- **Type-index confusion audit** — the relative-vs-canonical index hazard (the **CVE-2024-12053** class) from Phase 5, re-verified here as a security gate.

*Verification:*
- **Fuzzing** (`cargo-fuzz`): (a) validator/compiler — arbitrary bytes never panic, only `Err`; (b) interpreter — `wasm-smith`-generated valid modules never panic/UB/hang, only trap/return; (c) **differential** against `wasmtime`/`wasmi` on the generated corpus.
- **Threat model** (`docs/SECURITY.md`): the trust boundary (guest = untrusted; host fns + embedder = trusted), what we guarantee (spatial isolation, bounded resources, panic-freedom on validated input, **zero-on-allocation so no cross-tenant memory is ever readable**), what we explicitly **don't** (timing/Spectre side channels — an interpreter is far less exposed than a JIT but is *not* formally isolated; non-deterministic `externref`/GC `Drop` timing), and the **required embedder configuration** for multi-tenant (limits + fuel/epoch + per-tenant store/linker + capability-scoped imports).

**Done when:**
- Fuzzers run clean in CI: no panic/OOM/hang on arbitrary bytes (validator) or on `wasm-smith` modules (interpreter); differential parity with `wasmtime` on the generated corpus.
- A guest that recurses unboundedly traps `StackOverflow`; one that grows/allocates past limits is denied or traps per the limiter; one that loops forever is interrupted by fuel/epoch.
- A cross-store handle misuse errors/traps — never UB.
- Zero-on-allocation verified: a fuzz/regression test writes a pattern into one store's memory/table, drops it, and confirms a fresh store (and `memory.grow` n pages) reads only zeros — including under any pooling allocator added for startup speed.
- `docs/SECURITY.md` is published; the no-`unsafe` and type-index audits are CI-gated.

**Risks:** the panic-safety audit is broad — lean on clippy gates + fuzzing as the real enforcement, not manual review alone. Fuel/epoch charge points must cover every back-edge or a tight loop escapes interruption. Don't regress startup speed with over-aggressive validation limits — make them configurable with safe defaults.

---

## Cross-cutting workstreams (run continuously)

- **Conformance:** keep the `.wast` runner green for every enabled proposal; treat the spec suite as the definition of done per phase. Per-phase file targets are in [TESTING.md §5](./TESTING.md).
- **`wasmtime` compatibility:** maintain the `use submilli_wasm as wasmtime;` test and a growing set of `wasmtime` example programs that must compile and run unchanged against us. Signatures track pinned `wasmtime` 45.x; deviations are tracked as the documented intentional gaps only.
- **API examples** matching wasmtime's docs (host fns, linker, fuel, epoch, async, limits) double as integration tests.
- **Docs:** keep ARCHITECTURE.md / TESTING.md in sync as decisions evolve; add per-module doc comments.
- **Security & fuzzing:** treat untrusted-guest safety as a running invariant, not just a Phase-8 milestone — keep the zero-`unsafe` and no-panic-on-validated-input properties true as each phase lands, and stand up `cargo-fuzz` targets as soon as the validator/interpreter exist (Phase 1) rather than retrofitting at the end. Phase 8 is the consolidation/audit gate.
- **Perf sanity (low priority):** a CoreMark-style benchmark to track that compile time stays small; runtime is monitored but not optimized.

## Suggested milestones

- **M1 (Phases 0–1):** runs core wasm; spec MVP/multi-value/sign-ext/mutable-global green.
- **M2 (Phases 2–3):** full sync + async embedder API; fuel/epoch/limits/linker/host-fns; concurrent stores.
- **M3 (Phase 4):** references + typed function references.
- **M4 (Phases 5–6):** GC + exception handling — feature-complete against the target set.
- **M5 (Phase 7):** DWARF-symbolicated trap/exception backtraces; exception-propagation backtrace correctness verified.
- **M6 (Phase 8):** multi-tenant-safe — bounded resources, panic-free on validated input, store isolation verified, fuzzers green, threat model published.

## Sequencing notes

- Phases 1→2→3 are the critical path for a usable engine and should go in order.
- Phase 4 (refs) is a prerequisite for 5 (GC) and partially for 6 (`exnref` is a reference). Keep 4 before 5/6.
- Phase 7 (DWARF/backtraces) comes last of the feature phases: the exception-backtrace deliverable depends on Phase 6's unwinder, though the bare frame-walking capture already exists from Phase 1's error model and can be symbolicated incrementally.
- Phase 8 (security) is the final consolidation gate, but it is **not** purely terminal: its enforcement primitives (stack-size limit, limiter wiring) live in Phase 2 and are **prerequisites for any real multi-tenant deployment** — pull them forward and run the fuzzers continuously rather than deferring all of Phase 8 to the end.
- Within a phase, land the compile-pass + interpreter support first, then the spec suite, then the embedder API niceties.
```
