# Task Breakdown

Fine-grained checklist mirroring the live task tracker. Phase-level rationale,
scope, and acceptance criteria live in [PLAN.md](./PLAN.md); the design is in
[ARCHITECTURE.md](./ARCHITECTURE.md). Update the status boxes here as tasks land.

Legend: `[x]` done · `[ ]` pending · `[~]` in progress.

## Phase 0 — Scaffold
- [x] **#1** Scaffold crate + dependencies (Cargo.toml, lint config, module tree)
- [x] **#2** wasmtime-compatible public type stubs + dual-compile compat test
- [x] **#3** Vendor spec testsuite (submodule) + `tests/spec.rs` runner scaffold
- [x] **#4** CI (build/test/clippy/fmt) + 400-line file-size guard

## Phase 1 — Core interpreter (MVP + multi-value + sign-ext + mutable-globals)
- [x] **#5** Internal `Op` enum + `CompiledFunc` layout (`src/module/op.rs`)
- [x] **#6** Value model — `Val` accessors + `default_for` (`src/value/`)
- [x] **#7** Compile pass — wasmparser validation + linear pre-decode → `Op` stream
- [x] **#8** Compile pass — control-flow lowering (folded sidetable: br keep/pop, targets)
- [x] **#9** Store, arenas, handles, `AsContext`/`AsContextMut` (runtime side)
- [x] **#10** Entities — Memory (Vec<u8> + bounds), Global, Table (core)
- [x] **#11** Runtime — operand/frame stacks, run loop, zero-copy calls, multi-value
- [x] **#12** Numeric execution + trap semantics (div0, overflow, conversions, OOB)
- [x] **#13** Module + Instance + instantiation (imports, data/elem segments, start)
- [x] **#14** `Func::call` (untyped) end-to-end
- [x] **#14b** Bulk-memory table ops — `table.init`/`table.copy`/`elem.drop` compile lowering + exec (completes the `BULK_MEMORY` feature already enabled in the validator; per-instance dropped-elem state mirrors `dropped_data`). `table.get`/`set`/`grow`/`size`/`fill` + `ref.func`/`ref.null` stay with reference-types (#26).
- [x] **#15** Pass the **self-contained** core spec suites (gate): MVP + multi-value + sign-ext + mutable-globals — the `.wast` files that don't import `spectest`. Resilient runner in `tests/spec.rs` (managed skip inventory; per-module skip on unsupported-feature errors), `arg_to_val`/`rets_match` (NaN-aware), `Config::max_wasm_stack` → `Trap::StackOverflow`. **22,959 assertions pass**, 1,935 skipped (all categorized). (Import/`spectest`/multi-memory/ref-types files run at #24b / Phase 4.)

## Phase 2 — Embedder API depth (linker, typed calls, fuel, epoch, limits)
- [x] **#16** Sync host functions — `Func::new` (untyped `FuncType` + `&[Val]`/`&mut [Val]`), host-call boundary in the run loop, host `Err` → trap propagation (`src/func/`, `src/exec/`). `FuncEntity` is now `Wasm|Host`; closures live in `Store<T>.host_funcs`; the run loop *suspends* on host calls (`Outcome::HostCall`) and a thin generic driver `exec::host::execute<T>` builds the `Caller` and invokes them (zero `unsafe`).
- [x] **#17** `Caller<'_,T>` — `data()`/`data_mut()`/`get_export()`/`engine()`, `AsContext`/`AsContextMut` impls; read/write guest memory from a host fn
- [x] **#18** Typed calls — `WasmTy`/`WasmRet`/`WasmParams`/`WasmResults` + macro-generated tuple impls (`src/func/wasm_ty.rs`, `into_func.rs`) *(done with #19)*
- [x] **#19** Typed API surface — `IntoFunc`, `Func::wrap`, `Func::typed`, `TypedFunc::call`, `Instance::get_typed_func`; typed↔untyped agreement
- [x] **#20** `Linker<T>` — `define`/`define_name`/`get`/`get_default`, `func_wrap`/`func_new`, `instance`/`module`/`instantiate`, `alias`; multi-module instantiation. `(module,name)→Def` map; host defs (`func_*`) stored lazily and materialized into the store at `instantiate`/`get`. (Imported globals already alias the same store cell — `define` passes the handle through.)
- [x] **#21** Fuel — per-executed-op charging in the run loop (1 unit/`Op`, deterministic; structural ops compiled away); `Config::consume_fuel` retained on `Engine`, fuel counter on `StoreInner`, `Store::set_fuel`/`get_fuel` (require `consume_fuel`), `Trap::OutOfFuel`. (Chose per-op over compiler-injected block batches — simpler/precise; runtime speed secondary.)
- [x] **#22** Epoch — `Engine` `AtomicU64` + `increment_epoch`/`weak` (existing); `Config::epoch_interruption` retained on `Engine`; per-store absolute deadline on `StoreInner`; `Store::set_epoch_deadline`/`epoch_deadline_trap`/`epoch_deadline_callback`; run loop suspends per-op (uniform with fuel) → driver traps `Trap::Interrupt` or applies the `UpdateDeadline` callback (`Continue` extend-and-resume).
- [x] **#23** Limits — `Store::limiter` (closure on `Store<T>`); limiter consulted on memory/table grow (public API **and** in-wasm `memory.grow` op via the driver, `-1`/trap per `trap_on_grow_failure`), `Memory::new`/`Table::new` initial size, and instance/memory/table counts at `Instance::new`. (`Config::max_wasm_stack` (bytes) → `Trap::StackOverflow` already done in #15. Exhaustive growth-path coverage incl. table bulk ops + GC heap is #31.)
- [x] **#24** Phase-2 gate — `tests/api.rs`: 13 public-API integration tests driving the embedder surface end-to-end (typed/untyped host fns + guest-memory access via `Caller`, host `Err`→trap, `Linker` + multi-module linking, typed↔untyped agreement, deterministic fuel + `OutOfFuel`, epoch `Interrupt` + `UpdateDeadline::Continue` resume, limiter denies API/guest grows + `trap_on_grow_failure` + instance-count cap). `embedder_example.rs` stays the compile-only API-parity proof; no `src/` changes needed.
- [x] **#24b** `spectest` + import-dependent spec suites — register the `spectest` shim through the `Linker` and instantiate import/`register` files through it; implemented `register`/`assert_unlinkable`. Also **reworked the skip logic**: `Module::validate` under our feature set is now the oracle for "unsupported" (no error-string matching), so any failure from a validating module is a real bug. **179 files / 24,907 assertions pass**; skips are validation-rejected modules + out-of-scope whole files (simd/memory64/multi-memory).

## Phase 3 — Async & resumability
- [ ] **#25** Resumable suspend/resume; `call_async`, async host fns, fuel/epoch yield *(stub)*

## Phase 4 — References
- [ ] **#26** reference-types + function-references (value model + validator extensions) *(stub)*

## Phase 5 — GC
- [ ] **#27** Garbage collection — handle-table heap + mark-sweep; two collection triggers, both at safe points: per-store batch budget against the `ResourceLimiter` (collect-then-grow, retune to `live*factor`), and engine-wide pressure (`Engine` atomic GC-byte counter vs `Config::gc_memory_threshold`, default ~80% RAM → GC-requested flag checked at the fuel/epoch back-edge; request-not-force, since `Store` is `!Sync`) *(stub)*

## Phase 6 — Exception handling
- [ ] **#28** exception-handling — `exnref` + `try_table` on the branch machinery *(stub)*

## Phase 7 — DWARF & backtraces
- [ ] **#29** DWARF debug-info retention (`gimli`) + lazily-symbolicated trap/exception backtraces; `Config::wasm_backtrace[_details]`/`debug_info`; capture-at-throw so the exception proposal reports the full throw-site frame chain (closes the wasmtime gap) *(stub)*

## Phase 8 — Security hardening & multi-tenant isolation
- [ ] **#30** Stack-size limit — verify `Config::max_wasm_stack` (bytes) enforcement against our heap stacks → `Trap::StackOverflow`; unbounded recursion traps cleanly (impl in #23) *(stub)*
- [ ] **#31** Limiter coverage — route *every* growth/alloc/count path (`memory.grow`, `table.grow`/`fill`/`init`/`copy`, instance/table/memory/global counts, GC-heap alloc) through `ResourceLimiter`; no-limiter default is a finite documented ceiling, never unbounded *(stub)*
- [ ] **#32** Validation-time limits — bound module complexity at parse via `wasmparser` limits (fn body size, locals, declared table/memory sizes, control nesting, segment sizes); hostile module can't OOM the compiler *(stub)*
- [ ] **#32b** Instantiation safety — fuel/epoch/stack limits apply to the `start` function + active-segment init (guest code runs during `Instance::new`); document "arm limits before instantiation"; decide failed-instantiation rollback stance (short-lived store per attempt vs. transactional cleanup) *(stub)*
- [ ] **#33** Panic-safety — audit guest-reachable `unwrap`/`expect`/index/`as`-truncation/arith → traps; clippy `deny` gates (`unwrap_used`/`indexing_slicing`/`arithmetic_side_effects`) on the exec hot path; contain host-fn panics at the boundary *(stub)*
- [ ] **#33b** Zero-on-allocation — enforce/verify no cross-tenant memory disclosure: every guest-visible allocation initialized (holds today via safe-Rust + explicit zero/default); forbid uninit fast-paths (`set_len`/`MaybeUninit` on `memory.grow`, unzeroed buffer recycling in any pooling allocator, unzeroed `memory_reservation` capacity); regression/fuzz test write-drop-reread = zeros *(stub)*
- [ ] **#34** Store isolation — cross-store handle misuse errors/traps (never UB); zero-`unsafe` CI grep guard; type-index (CVE-2024-12053) audit gate *(stub)*
- [ ] **#35** Fuzzing — `cargo-fuzz` validator + interpreter targets + `wasm-smith` differential vs `wasmtime`/`wasmi`; CI integration *(stub)*
- [ ] **#36** Threat model — `docs/SECURITY.md`: trust boundary, guarantees, non-guarantees (side channels, non-deterministic GC `Drop`), required multi-tenant embedder config *(stub)*

## Phase 9 — Remaining standardized proposals (complete Wasm 2.0 + 3.0; spec suite zero-skip)
- [ ] **#37** Fixed-width SIMD (`v128`) — the full 236-op set on `Val::V128`: lane shapes, splat/extract/replace, arith/sat/cmp/shift/bitwise/bitmask, narrow/widen/extend/extmul/extadd/dot/q15mulr, all `v128.load*`/`store*` (`_lane`/`_splat`/`_zero`/extend); enable `WasmFeatures::SIMD` *(stub)*
- [ ] **#38** Relaxed SIMD — `relaxed_*` ops with a documented fixed deterministic lowering (depends on #37) *(stub)*
- [ ] **#39** Tail calls — `return_call`/`return_call_indirect` (+ `return_call_ref`): reposition args into the caller frame and jump; enable `TAIL_CALL` *(stub)*
- [ ] **#40** Extended const — const-expr `global.get` of prior immutable globals + `i32`/`i64` `add`/`sub`/`mul`; extend `ConstExpr` + evaluator; enable `EXTENDED_CONST` *(stub)*
- [ ] **#41** Multiple memories — explicit memory index on every memory op (`load`/`store`/`size`/`grow`/`fill`/`copy`/`init`) → `instance.memories[idx]`; enable `MULTI_MEMORY` *(stub)*
- [ ] **#42** Memory64 — `i64` index type, `memory.size`/`grow` return `i64`, honor `is_64`/`table64`; enable `MEMORY64` *(stub)*
- [ ] **#43** Phase-9 gate — `cargo test --test spec` runs with **0 whole-file skips, 0 module/assertion skips, 0 partials** (legacy EH excepted); proposal suites (`simd_*`/`relaxed_*`/`tail_call*`/`*64`/multi-memory/extended-const) all pass *(stub)*

> Out of scope (not Wasm 3.0): **threads/atomics**, **custom-page-sizes** — see ARCHITECTURE §1 non-goals.

---

**Note:** during an active session the harness task tracker is the source of truth
(it carries ownership/blocking metadata); this file is the durable, version-controlled
mirror. Tasks #25–#43 (Phases 3–9) are intentionally coarse and get broken into
fine-grained subtasks when their phase begins (as #1–#24 were).
