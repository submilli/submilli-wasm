# Security model & threat model

`submilli-wasm` is built to run **untrusted, mutually-distrusting WebAssembly guests** ("tenants") in a
single process. This document is the threat model: the trust boundary, what the interpreter guarantees and
how each guarantee is enforced, what it explicitly does **not** guarantee, and the configuration an embedder
**must** apply for safe multi-tenant operation.

It describes guarantees that exist in-tree today; every claim below maps to a concrete mechanism and test.
Design rationale lives in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (§7 run loop, §8 instantiation, §14 GC).

## 1. Scope & trust boundary

| Party | Trust |
| --- | --- |
| Guest wasm module (tenant code) | **Untrusted** — adversarial on every wasm-reachable path |
| The embedder (host application) | Trusted |
| Host functions the embedder installs | Trusted (must contain their own logic; see below) |
| Curated/vetted wasm packages the embedder compiles as trusted | Trusted (higher complexity limits) |
| `Engine` / `Config` | Trusted (the embedder owns it) |

**The isolation unit is one `Store` (+ its `Linker`) per tenant.** Type *identity* is shared engine-wide
(so cooperating modules can exchange GC types), but GC *objects* and all entity handles
(`Func`/`Memory`/`Global`/`Table`/`Tag`/`Instance`) are **store-bound** — a handle from one tenant's store
cannot be used against another's.

The interpreter is a stack machine with **no JIT**: guest code never produces native machine code. This
removes the largest class of runtime-isolation bugs (codegen miscompiles, speculative type-confusion
gadgets) by construction.

## 2. What we guarantee

Each guarantee lists its enforcing mechanism and the test that backs it.

### Spatial memory isolation — no out-of-bounds access
Every linear-memory, table, and GC-object access is a software bounds check in **safe Rust**. The tree
contains **zero `unsafe`** operations (the only `unsafe` tokens are two wasmtime-API-parity `unsafe fn`
signatures — `Module::deserialize[_file]` — with no unsafe body). This is CI-gated two ways: the
`unsafe_code` lint under `cargo clippy -- -D warnings`, and `scripts/check-no-unsafe.sh` (which also rejects
a real `unsafe {}` smuggled in under an `#[allow(unsafe_code)]`). There is no OOB read/write to exploit; an
out-of-range access is a deterministic `Trap`, not undefined behavior.

### No cross-tenant memory disclosure — zero-on-allocation
A guest can never read memory it did not write — not another tenant's freed heap, not allocator residue,
not a recycled buffer. Every guest-visible allocation is zero/default-initialized before the guest can
observe it: linear memory (`resize(n, 0)`), tables (`resize(n, init)`), locals (`Val::default_for`), GC
objects (zeroed then field-filled), operand cells. No uninitialized fast-path exists, and because
`set_len`/`MaybeUninit`/`with_capacity`-then-expose all require `unsafe`, the zero-`unsafe` invariant
forbids them structurally. Tested by `tests/zero_alloc.rs` (grown memory reads zero, a fresh store reads
zero after a prior store dirtied+dropped its own, table slots null, defaulted aggregates).

### Bounded CPU — no hang
Fuel (charged per executed op, deterministic) and/or epoch interruption interrupt **any** guest loop,
including tight host-call-free loops (the charge/check sits on every back-edge). At least one must be
enabled for untrusted code. Both also bound the `start` function and active-segment initializers, which run
*during* `Instance::new` (see §4). Tested by `tests/instantiation.rs` (fuel/epoch interrupt `start`) and
`tests/api.rs`.

### Bounded memory, stack, and allocation — no OOM-DoS
- The installed `ResourceLimiter` gates every growth/allocation/count path: `memory.grow`, `table.grow`,
  instance/memory/table counts at instantiation, and GC-heap allocation.
- `Config::max_wasm_stack` (bytes) bounds recursion → `Trap::StackOverflow`, including across the host↔wasm
  re-entry boundary, so host/guest ping-pong traps rather than aborting the native stack (#30).
- Validation-time `Config::max_module_bytes` bounds the *compiler* against a hostile module before it runs
  (#32), on top of `wasmparser`'s per-dimension limits.
- **With no limiter installed, the defaults are finite ceilings, never "unbounded"** — a deliberate
  deviation from wasmtime (see §5).

Tested by `tests/stack_limit.rs`, `tests/validation_limits.rs`, `src/store/limits_tests.rs`,
`tests/gc_collect.rs`.

### Panic-freedom on validated input
No validated guest module can panic the interpreter (a panic would be a whole-process DoS). The exec hot
path is clippy-gated (`panic`/`todo`/`unimplemented`/`indexing_slicing` denied, with documented
post-validation carve-outs), and the fuzzer is the real net (§6). **Host-function panics are contained** at
the call boundary (`catch_unwind` → restore store state → re-raise, matching wasmtime), and the engine's
shared type-registry lock is poison-recovering, so one tenant's host-fn panic cannot poison the shared
engine or other tenants. Tested by `tests/panic_safety.rs`.

### Store isolation — cross-store handle misuse is caught
A handle (`Func`/`Memory`/`Global`/`Table`/`Tag`/`Instance`) minted by store A and used against store B is
detected and faults, rather than silently resolving to the wrong entity. This is never UB (it was already
memory-safe; this makes the isolation breach *observable* instead of silent). Tested by `tests/isolation.rs`.

### Type-confusion safety
Module-relative (decoder-local) type indices and engine-canonical type ids are kept strictly separate;
every runtime type check (`call_indirect`, `ref.test`/`ref.cast`, `struct.*`/`array.*`) compares canonical
ids. This is the **CVE-2024-12053** class, regression-tested in `tests/isolation.rs` (two modules whose
matching type sits at different relative indices link via canonical id; a same-index decoy does not).

## 3. What we do NOT guarantee

- **Side channels (timing / Spectre / microarchitectural).** An interpreter is far less exposed than a JIT
  — there is no guest-controlled generated code — but submilli is **not** formally isolated against timing
  or microarchitectural side channels and makes **no constant-time guarantees**. A tenant can observe coarse
  timing of shared resources. Do not use it as a boundary against confidential-data side-channel leakage.
- **Deterministic finalization.** `externref` payload `Drop` and GC-object reclamation run at
  collection time (non-moving mark-sweep), not deterministically. Host-side `Drop` timing/ordering is not a
  contract to depend on.
- **Transactional instantiation.** A failed `Instance::new` is **not** rolled back — entities allocated
  before the failing step linger in the store until it is dropped (matching wasmtime). Mitigation is the
  per-tenant short-lived-store stance in §4.
- **wasmtime-compatible fuel.** Fuel is deterministic within submilli but uses a different per-op cost model
  than wasmtime; do not compare fuel counts or fuel-trap outcomes across engines.
- **Real-time / wall-clock bounds.** None. Use epoch interruption from a watchdog for wall-clock deadlines.

## 4. Required embedder configuration (multi-tenant)

For untrusted, multi-tenant operation the embedder **must**:

1. **One `Store` + one `Linker` per tenant.** Treat the `Store` as the isolation/resource unit. **Discard it
   on any instantiation or execution failure** — do not reuse it (failed instantiation is not rolled back).
2. **Install a `ResourceLimiter`** sizing memory/table/instance/GC-heap caps to the tenant's budget. Do not
   rely on the finite-but-large no-limiter defaults for production.
3. **Enable fuel and/or epoch interruption** (at least one is mandatory for untrusted CPU bounding) and
   **arm them before `Instance::new`** — guest code runs at instantiation (`start` + active segments), not
   only on the first export call.
4. **Set `Config::max_wasm_stack`** to bound recursion depth.
5. **Set `Config::max_module_bytes`** to the untrusted-tier ceiling for guest modules. Compile only
   curated/vetted packages with the higher limit via `Module::new_with_limits`.
6. **Capability-scope imports.** Expose only the host functions a tenant is authorized to call. Host-fn
   panics are contained, but a panic still aborts that tenant's call — keep host functions robust.
7. **Never pass handles or GC references across tenant stores** (it faults, but don't rely on the fault as a
   feature).

## 5. Security-relevant deviations from wasmtime

submilli aims for drop-in wasmtime API compatibility; these *behavioral* deviations are security-motivated
and documented:

- **Finite no-limiter ceilings.** With no `ResourceLimiter` installed, memory/table growth and initial size
  are bounded by finite default ceilings (notably `memory64`/`table64`, which wasmtime leaves effectively
  unbounded). The no-limiter default is never "unbounded". (#31)
- **`Config::max_module_bytes`.** An additive validation-time cap with no wasmtime analog, bounding compiler
  memory against a hostile module. (#32)
- **Collector selection.** `Collector::Auto`/`MarkSweep` run the non-moving mark-sweep collector;
  `Collector::Null` is allocate-only; `DeferredReferenceCounting`/`Copying` are **rejected** at
  `Engine::new`. (#27g)
- **Funcref-value store binding.** Named handles are store-checked; a funcref obtained as a *value* (e.g. a
  `Func::call` result) carries no store check, since funcref values are same-store by construction. (#34)

## 6. How these guarantees are verified

- **Spec conformance:** the WebAssembly testsuite (36,789 assertions, 0 failures; +SIMD under `--features
  simd`).
- **Targeted tests:** `tests/{stack_limit,validation_limits,instantiation,panic_safety,zero_alloc,isolation,
  gc_collect}.rs` and `src/store/limits_tests.rs`.
- **Fuzzing** (`fuzz/`, #35): a `validate` target (arbitrary bytes never panic, only `Err`), an `interpret`
  target (`wasm-smith` modules run fuel-bounded without panic/hang), and a `differential` target comparing
  results and trap categories against wasmtime.
- **CI gates:** `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
  `scripts/check-no-unsafe.sh` (zero-`unsafe`), and `cargo fuzz build`.
