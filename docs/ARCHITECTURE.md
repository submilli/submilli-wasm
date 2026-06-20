# Architecture

> Working crate name: `submilli-wasm` (lib `submilli_wasm`).
> A WebAssembly interpreter in Rust with a **`wasmtime`-compatible** (drop-in) API.

## 1. Goals & non-goals

### Goals
- **Fast compilation / startup is the primary objective.** Runtime throughput is explicitly secondary.
- Stack-based interpreter (no JIT, no native codegen).
- Feature set: **the full finished Wasm 2.0 + 3.0 proposal set** (excluding threads/atomics and custom-page-sizes). Core path (Phases 1–6): MVP + mutable-globals + **sign-extension-ops** + **non-trapping (saturating) float→int** + multi-value + bulk-memory + reference-types + function-references + GC + exception-handling. Remaining standardized proposals (Phase 9): fixed-width SIMD + relaxed-SIMD + tail-calls + extended-const + multiple-memories + memory64. The acceptance bar is the vendored Wasm-3.0 spec suite passing with **zero skips**.
- **Fully `wasmtime`-compatible embedder API** — drop-in for the implemented feature subset. Goal: existing code written against `wasmtime` compiles and runs against us by changing only the import path (`use submilli_wasm as wasmtime;`). Type names, method signatures, trait bounds, and error/trap semantics match `wasmtime` exactly for everything we support.
- Capabilities: sync + async host functions; async execution with a shared `Engine` across many concurrent executions; multi-module + `Linker`; fuel/gas metering; epoch interruption; memory controls (initial/maximum) for both linear memory and the GC heap.
- Portable, minimal `unsafe`.

### Non-goals (initially)
- Peak runtime performance / JIT tiering.
- **Threads/atomics** (a separate proposal, not part of Wasm 3.0; needs a shared-memory model change), **custom-page-sizes**, the component model (`wasmtime::component::*`), WASI, and module serialization/AOT (`Module::serialize`/`deserialize`). These are the intentional gaps in `wasmtime` API coverage; everything we *do* implement matches `wasmtime`'s signatures. (SIMD/memory64/multi-memory and the other standardized 2.0/3.0 proposals are **in scope** at Phase 9 — the value model and flat instruction table were built to absorb them.)
- A *moving* or generational/incremental garbage collector. We ship a non-moving, stop-the-world mark-sweep collector; it reclaims cycles but pauses to collect. (The public `Config::collector`/`Collector` API still matches `wasmtime`; only the internal strategy differs.)
- Native-debugger (GDB/LLDB JIT) integration: there is no native code to attach to. `Config::debug_info`'s debugger aspect is therefore a no-op — but we *do* consume the module's DWARF to produce source-level **backtraces** (Phase 7), so `wasm_backtrace_details` is fully supported.

### Compatibility approach
We target **API-level drop-in compatibility**: the same public types (`Engine`, `Config`, `Store<T>`, `Module`, `Instance`, `Linker<T>`, `Func`, `TypedFunc`, `Caller`, `Val`, `Ref`, `Extern`, `Memory`, `Global`, `Table`, `ExternRef`/`AnyRef`/`ExnRef`, `Rooted`, `RootScope`, `Trap`, `WasmBacktrace`, `ResourceLimiter`, `StoreLimits[Builder]`, `UpdateDeadline`, `CallHook`), the same method signatures and trait bounds (`AsContext`/`AsContextMut`, `IntoFunc`, `WasmParams`/`WasmResults`/`WasmTy`/`WasmRet`), and the same error model — wasmtime 45.x's own `Error`/`Result` (NOT `anyhow::Error`; wasmtime moved off it), with `Trap`/`WasmBacktrace` recovered via `downcast_ref`, plus the `bail!`/`ensure!`/`format_err!` macros. Internals (the pre-decode interpreter, the resumable loop instead of fibers, the mark-sweep collector) are free to differ as long as observable behavior matches. Where `wasmtime` has feature-gated or version-drifted APIs, we track a pinned `wasmtime` version (initially **45.x**) and document it.

## 2. Design principles

These follow directly from "compile speed ≫ runtime speed":

1. **One linear compile pass, no register allocation.** We pre-decode wasm into a flat internal stack-bytecode. No SSA, no interference graph, no optimizing backend. This is the cheapest non-trivial compilation model that still yields a fast hot loop.
2. **Reuse `wasmparser` for decoding + validation.** It correctly handles every encoding detail (LEB128, the s33 blocktype, all targeted proposals' opcodes) and validates per the spec. Hand-rolling this is large and a correctness liability for no benefit on our priorities.
3. **Fold control-flow metadata into branch instructions at compile time.** The Wizard paper computes a *sidetable* (`⟨Δip, Δstp, keep, pop⟩` per branch) during validation because its IP walks the *immutable* original bytes. We own a *mutable* decoded instruction array, so we instead patch the resolved target index and the `keep`/`pop` value counts directly into each branch op. Same O(1) branch resolution, no separate sidetable/STP, simpler and faster loop.
4. **Explicit operand and frame stacks; no native recursion for wasm calls.** This bounds host-stack usage and — crucially — makes the entire interpreter a **pausable state machine**. Suspend/resume (for async, fuel, epoch) is just saving and restoring a struct.
5. **`loop { match op }` dispatch.** Rust has no guaranteed tail calls (`become` is unstable), so wasm3-style threaded dispatch is not portable. A match over a clean internal op enum is the pragmatic choice and is fine given our priorities.
6. **Portable, allocation-backed runtime.** Linear memory is a `Vec<u8>`; the GC heap is a handle table. No mmap, guard pages, or signal handlers.

## 3. Component map

```
        embedder (host application)
                 │  wasmtime-compatible API (drop-in)
   ┌─────────────┴───────────────────────────────────┐
   │ Engine (Arc, Send+Sync)   Config   epoch counter │
   └─────────────┬───────────────────────────────────┘
                 │ shared by many
        ┌────────┴─────────┐
        │  Store<T>        │  owns all runtime entities + host state T
        │  ├ funcs/mems/…  │  (arenas keyed by typed handles)
        │  ├ gc heap       │
        │  └ Execution     │  resumable interpreter state (operands, frames, fuel)
        └────────┬─────────┘
   Module (compiled, shareable) ── Instance ── Linker<T>
   CompiledFunc{ops, sidetable-folded branches, layout}
```

Modules (Rust source layout):

```
src/
  lib.rs
  engine.rs     Engine, Config, epoch (AtomicU64)
  store.rs      Store<T>, AsContext/AsContextMut, entity arenas, handles
  value.rs      Value, Ref, ValType, HeapType, FuncType
  module/
    mod.rs      Module (parse + validate + compile orchestration)
    compile.rs  single-pass decoder: wasm ops -> Vec<Op>, branch resolution
    op.rs       the internal Op enum (instruction set)
  instance.rs   Instance, instantiation (imports, elem/data init, start)
  linker.rs     Linker<T>, multi-module import resolution
  func.rs       Func, TypedFunc, IntoFunc, Caller<'_,T>, WasmParams/WasmResults
  extern_.rs    Memory, Global, Table, Extern
  exec/
    mod.rs      run loop, Step/SuspendReason, resume
    frame.rs    Frame, operand-stack discipline
  gc.rs         handle-table heap + mark-sweep (phase 5)
  trap.rs       Trap, error model
  config.rs     Config knobs (fuel, epoch, limits, async)
```

## 4. Compilation pipeline

`Module::new(&engine, bytes)`:

1. **Parse + validate** with `wasmparser` (`Validator` + per-function `FuncValidator`). This yields type info, resolves all indices, and rejects invalid modules. Validation and decoding run in the same scan.
2. **Pre-decode each function** (`compile.rs`) into a `CompiledFunc`:
   - Walk operators in order. For each, emit one (or zero, for dead code) internal `Op`.
   - Track an **abstract operand height** and a **control stack** of block records to resolve branches (see §5).
   - Fold constants into op immediates.
   - Resolve backward branch targets immediately (loop headers), record forward branches for patching at the matching `end`.
3. **Lazy option (later):** per-function compilation can be deferred to first call behind a state machine (`Uninit → Compiling → Compiled`), as wasmi does, for large modules. Phase 1 compiles eagerly; laziness is an additive optimization.

`CompiledFunc`:

```rust
struct CompiledFunc {
    ops: Box<[Op]>,
    type_idx: TypeIdx,
    n_params: u32,
    n_results: u32,              // cached from the type; used on return
    local_types: Box<[ValType]>, // declared locals (params excluded); default-init at call
    max_operands: u32,           // peak operand-stack depth above locals (from validation)
}
```

Compilation does **no** register allocation and **no** second pass beyond back-patching forward branch targets, so cost is ~linear in code size with a tiny constant.

## 5. Internal bytecode & control flow

### Instruction set (`Op`)
A flat enum with inline immediates, one variant per logical operation (locals, globals, memory load/store with offset, numeric ops, calls, branches, ref/gc/eh ops added in later phases). Example shape:

```rust
enum Op {
    // values / locals / globals
    I32Const(i32), I64Const(i64), F32Const(u32), F64Const(u64),
    LocalGet(u32), LocalSet(u32), LocalTee(u32),
    GlobalGet(GlobalIdx), GlobalSet(GlobalIdx),
    // memory
    I32Load { offset: u32, align: u8 }, /* … all load/store variants … */
    MemorySize, MemoryGrow,
    // numeric (one variant per opcode)
    I32Add, I32Sub, /* … */
    // control — branch metadata folded in:
    Br(BranchTarget),
    BrIf(BranchTarget),
    BrTable { targets: Box<[BranchTarget]>, default: BranchTarget },
    Return,
    Call(FuncIdx),
    CallIndirect { type_idx: TypeIdx, table: TableIdx },
    Unreachable, Nop,
    // later phases: RefNull/RefFunc/CallRef/StructNew/.../Throw/TryTableEnter/...
}

struct BranchTarget {
    ip: u32,     // resolved index into CompiledFunc.ops
    keep: u16,   // # values transferred to the target label
    pop: u16,    // # operands discarded beneath them
}
```

`block`/`loop`/`if`/`end` produce **no runtime op** (or a `Nop`/marker only where needed): structured control flow is fully lowered to `Br*` with resolved targets. This is the "blocks vanish" property from wasmi/Wizard.

### Branch metadata (the folded sidetable)
For a branch to label `L`:
- `keep` = arity of `L` — **result count** for `block`/`if`, **parameter count** for `loop` (the multi-value rule: branching to a loop restarts it and must supply its inputs).
- `pop` = `current_operand_height − (L.base_height + keep)` — the operands between the surviving results and the label's stack floor, discarded on transfer.
- `ip` = target op index — the matching `end` for `block`/`if`, the loop header for `loop`.

Runtime branch execution (no STP needed):
```
fn take_branch(t: &BranchTarget, ops: &mut OperandStack) {
    ops.move_top_down(t.keep, t.pop); // copy top `keep` over `pop` dead slots
    frame.ip = t.ip;
}
```

### Compile-time control stack
```rust
struct CtrlFrame {
    kind: Block | Loop | If | Else,
    base_height: u32,         // operand height at entry (after params popped)
    label_arity: u16,         // results (block/if) or params (loop)
    start_ip: u32,            // loop header, for backward targets
    pending_forward: Vec<u32>,// op indices whose BranchTarget.ip needs patching at `end`
    unreachable: bool,        // dead-code tracking after br/return/unreachable
}
```

Dead code after an unconditional control transfer is dropped (no `Op` emitted) until the matching `else`/`end`, mirroring validation's stack-polymorphic state.

## 6. Value model

The value enum is **`Val`** (`src/value/val.rs`) — the public, `wasmtime`-compatible value type *and* the value the interpreter operates on (no separate internal type). It is enriched progressively across phases so later proposals never force a rework:

```rust
pub enum Val {
    I32(i32), I64(i64), F32(u32), F64(u64), V128(V128),
    FuncRef(Option<Func>),               // null = None
    ExternRef(Option<Rooted<ExternRef>>),
    AnyRef(Option<Rooted<AnyRef>>),      // struct/array/i31 land under here in Phase 5
    ExnRef(Option<Rooted<ExnRef>>),
}
```

- f32/f64 stored as raw bits (matching `wasmtime::Val`); accessors `f32()`/`unwrap_f32()` decode via `from_bits`.
- Null is **per-hierarchy** (`Option::None` per reference variant), not a single sentinel — a null funcref and a null externref are distinct types.
- `Val::default_for(&ValType)` yields the correctly-typed zero (numbers → 0, references → null) for local initialization.
- `funcref`/`externref` are abbreviations of `(ref null func/extern)` once function-references lands; types are erased at runtime (only the null-ness bit is inspected by `call_ref`/`ref.as_non_null`/`br_on_*`). GC `struct`/`array`/`i31` are reached through the `AnyRef` hierarchy (Phase 5); a compact internal representation (NaN-boxing) is a possible later optimization.

A later optimization (NaN-boxing / tagged `u64` cells) is possible but deferred; correctness first.

## 7. Runtime: execution engine

### Stacks
- **Operand stack** — one flat `Vec<Value>` per `Execution`. Per Wizard, **locals and operands are contiguous**: a call reserves `n_locals` slots (params copied/aliased in, the rest default-initialized or marked unset for non-defaultable locals), then operands grow above. `local.get i` ⇒ `operands[frame.locals_base + i]`.
- **Frame stack** — `Vec<Frame>`; wasm calls push a frame instead of recursing natively.

```rust
struct Frame {
    func: FuncAddr,
    ip: u32,
    locals_base: u32,         // index into the operand stack
    instance: InstanceHandle,
    handlers: Vec<HandlerRec>,// exception-handling (phase 6)
}
```

### Calls (zero-copy args)
On `Call`, callee args are already the top `n_params` operands; the callee's `locals_base = caller_top − n_params`, so arguments become the callee's first locals with no copy (the long-promised "JVM trick", per Wizard). On `Return`, the top `n_results` operands are moved down to `locals_base`, the frame is popped, and execution continues in the caller. Tail calls (`return_call`) reuse the caller's frame slot.

### The loop
```rust
enum Step { Done, Trap(Trap), Suspend(SuspendReason) }
enum SuspendReason { OutOfFuel, Epoch, HostAsync(HostFutureHandle) }

fn run(store) -> Step {
    loop {
        // periodic checks batched at block/loop back-edges & calls:
        //   fuel -= cost; if fuel < 0 { return Step::Suspend(OutOfFuel)|Trap(OutOfFuel) }
        //   if epoch_deadline_passed() { return Step::Suspend(Epoch)|Trap(Interrupt) }
        match frame.func.ops[frame.ip] {
            Op::I32Add => { /* pop2 push1 */ frame.ip += 1; }
            Op::Br(t)  => take_branch(&t, ops),
            Op::Call(f)=> { push_frame(f); }
            // …
            Op::Return => { if pop_frame_or_finish() { return Step::Done } }
        }
    }
}
```

Because all state lives in `Execution` inside the `Store`, returning `Step::Suspend` mid-execution is safe and cheap; re-entering `run` resumes exactly where it left off.

## 8. Store, handles, entity model

- **`Store<T>`** owns every runtime entity (funcs, memories, tables, globals, instances, GC objects, extern objects) in typed arenas, plus the embedder's host state `T` and the in-flight `Execution`.
- Public handles (`Func`, `Memory`, `Table`, `Global`, `Instance`) are lightweight indices into the store's arenas, bound to that store.
- **`AsContext` / `AsContextMut`** are implemented by both `Store<T>`/`&mut Store<T>` and `Caller<'_, T>`, so every entity method takes `impl AsContextMut` and works identically whether called from the embedder or from inside a host function. This is the API spine, copied directly from wasmtime.

### Instantiation & the start function

`Instance::new` (impl in `src/instance/init.rs`) populates the store in a fixed order, matching the spec's instantiation algorithm:

1. **Check imports** — arity must match; each import is type-checked against its declaration.
2. **Link imports** into the func/memory/global/table index spaces.
3. **Allocate defined entities** — memories (zeroed), tables (filled with the element type's null), globals (each evaluated from its constant init expression, which may read earlier imported/defined globals).
4. **Allocate the function entities** and the `Instance` record.
5. **Apply active element segments**, then **active data segments** — each bounds-checked against the table/memory; an out-of-range segment **traps** (`TableOutOfBounds`/`MemoryOutOfBounds`) and aborts instantiation.
6. **Run the `start` function** (`run_start`): if the module declares a start index, resolve that func and execute it with no arguments. A module has at most one start function; it takes no params and returns nothing.

**The start function is the first point at which guest code executes** — before the embedder ever calls an export. Its execution is an ordinary call through the interpreter loop, so it is subject to **the same fuel / epoch / stack-size limits** as any other call (once those land). A trap in `start` (or in any active-segment initializer) makes `Instance::new` return `Err`; the partially-built `Instance` handle is never returned to the embedder.

> **Rollback caveat (current behavior):** entities allocated before the failing step (memories, tables, globals, the instance record) remain in the store's arenas until the `Store` is dropped — instantiation is *not* transactionally rolled back. The failed instance is unreachable via the API, but its memory is not reclaimed early. This is a known limitation flagged for the security phase (a tenant repeatedly instantiating failing modules into one long-lived store can accumulate dead entities); see PLAN Phase 8.

## 9. Public API surface (`wasmtime`-compatible)

Signatures match `wasmtime` (pinned to **45.x**) exactly so embedder code is drop-in. `Result<T>` and `Error` are our own (wasmtime-native, a thin wrapper over `anyhow`; see §16) — NOT `anyhow::Result`. The public value type is **`Val`** (internally backed by the `Value` enum of §6); `Val` is the exported name.

```rust
// engine / config
Engine::new(config: &Config) -> Result<Engine>     // Arc-cheap clone, Send+Sync
Engine::increment_epoch(&self)
Engine::weak(&self) -> EngineWeak
Config::{new, consume_fuel, epoch_interruption, async_support, async_stack_size,
         max_wasm_stack, wasm_function_references, wasm_gc, wasm_exceptions,
         wasm_reference_types, wasm_multi_value, wasm_*_, collector,
         gc_heap_*, memory_reservation, gc_memory_threshold /* ⚠ ours, not wasmtime */,
         wasm_backtrace, wasm_backtrace_details, debug_info, ...} // wasmtime-named knobs

// modules / instances
Module::{new, from_binary, from_file, validate, imports, exports, get_export}
Instance::new(store: impl AsContextMut, &Module, imports: &[Extern]) -> Result<Instance>
Instance::{get_func, get_typed_func::<P,R>, get_memory, get_global, get_table, get_export}

// linker
Linker::new(&Engine)
Linker::{define, define_name, func_wrap, func_new, func_wrap_async, func_new_async,
         instance, module, instantiate, instantiate_async, get, get_default,
         alias, alias_module, allow_shadowing}

// functions
Func::wrap<T,P,R>(store, impl IntoFunc<T,P,R>) -> Func
Func::new<T>(store, FuncType, impl Fn(Caller<'_,T>, &[Val], &mut [Val]) -> Result<()> + Send+Sync+'static) -> Func
Func::wrap_async / Func::new_async                            // boxed-future closures
Func::call(store, &[Val], &mut [Val]) -> Result<()>
Func::call_async(store, &[Val], &mut [Val]) -> Result<()>
Func::typed::<P,R>(store) -> Result<TypedFunc<P,R>>
TypedFunc::call(store, P) -> Result<R>
TypedFunc::call_async(store, P) -> Result<R>

// values / externs / refs
Val { I32, I64, F32(u32 bits), F64(u64 bits), V128, FuncRef(Option<Func>),
      ExternRef(Option<Rooted<ExternRef>>), AnyRef(Option<Rooted<AnyRef>>),
      ExnRef(Option<Rooted<ExnRef>>) }
Ref, Extern{Func,Memory,Global,Table,Tag}
ExternRef::{new, data}; AnyRef, ExnRef; Rooted<T>; RootScope::new
Memory::{new, ty, data, data_mut, data_ptr, data_size, size, grow, read, write}
Global::{new, ty, get, set}; Table::{new, get, set, grow, size}

// store / resource control
Store::{new, data, data_mut, into_data, engine,
        set_fuel, get_fuel, fuel_async_yield_interval,
        set_epoch_deadline, epoch_deadline_trap, epoch_deadline_callback,
        epoch_deadline_async_yield_and_update, limiter, limiter_async, call_hook}
Caller<'_,T>::{data, data_mut, get_export, engine}
AsContext / AsContextMut                                       // impl by Store & Caller
ResourceLimiter / ResourceLimiterAsync traits
StoreLimits, StoreLimitsBuilder{memory_size, table_elements, instances, tables,
                                memories, trap_on_grow_failure}
UpdateDeadline{Interrupt, Continue, Yield, YieldCustom}; CallHook
Trap (enum, downcast from Error); WasmBacktrace; Error, Result, bail!, ensure!, format_err!
```

**Intentional gaps** (the only places embedder code won't port unchanged): `wasmtime::component::*`, WASI, SIMD/threads/memory64 instructions, `Module::serialize`/`deserialize`. The `Config` knobs for those features exist as no-ops/errors where needed to keep call sites compiling. Internal divergences (resumable loop vs fibers, mark-sweep vs DRC/copying GC) are invisible at the API boundary.

## 10. Host functions

- **Typed** (`Func::wrap` / `Linker::func_wrap`): an `IntoFunc` trait implemented for closures `Fn(A1..An) -> R` and the caller-aware `Fn(Caller<'_,T>, A1..An) -> R`, with `Ai: WasmTy` and `R: WasmRet` (covers `()`, a single value, a tuple of results, and `Result<_, Error>` where `Err` → trap). Arity supported up to ~16 params (macro-generated impls).
- **Untyped** (`Func::new` / `Linker::func_new`): dynamic `FuncType` + `&[Val]`/`&mut [Val]`.
- **`Caller<'_, T>`** is passed as the optional first parameter; gives `data()/data_mut()` and `get_export("memory")` to read/write guest memory. Implements `AsContext[Mut]`.
- Host fns return our `Result<_>` (= `Result<_, Error>`); `Err` unwinds wasm as a trap and surfaces from the outer `call` (recover a specific `Trap` via `err.downcast_ref::<Trap>()`).

When a host function is invoked, the interpreter pushes a host-call boundary, runs the Rust closure, and pushes results back onto the operand stack. (Sync host calls do not suspend; async ones can — §11.)

## 11. Async & resumability

Async support reuses the resumable loop; **no fibers** internally — but the **public async API matches `wasmtime` exactly** (`Config::async_support`, `*_async` call/instantiate methods, `func_wrap_async`/`func_new_async` taking boxed-future closures, `fuel_async_yield_interval`, `epoch_deadline_async_yield_and_update`, `UpdateDeadline::{Yield,YieldCustom}`). Embedders cannot observe that we resume a saved state struct rather than switching native stacks; this is purely an implementation simplification (an interpreter can pause by returning, which `wasmtime`'s compiled code cannot, which is *why* it needs fibers).

- `TypedFunc::call_async` / `Func::call_async` drive `run` in a loop, awaiting on each `Step::Suspend`.
- **Async host function** returns a future. If it is not ready, the interpreter records the pending future and returns `Step::Suspend(HostAsync(..))`; `call_async` `.await`s it, stores the result back, and resumes.
- **Fuel-yield** and **epoch-yield** return `Step::Suspend(OutOfFuel|Epoch)`; the async driver yields to the executor (and, for fuel, optionally tops up per a yield interval) then resumes.
- A shared `Engine` backs many `Store`s on many tasks concurrently; concurrency is entirely the embedder executor's (Wasmtime never spawns threads either).
- As in `wasmtime`, with `async_support` enabled the **sync entry points (`Func::call`, etc.) panic**; without it, `*_async` are unavailable. The sync loop treats `OutOfFuel`/`Epoch` as **traps** rather than yields.

## 12. Fuel & epoch

- **Fuel**: a per-`Store` counter. The run loop charges **1 unit per executed internal `Op`** (structural ops like `block`/`loop`/`if` are already compiled away, so they cost nothing); the charge happens only on the op-execution path (a function return is not an op). This is a deterministic cost model — `set_fuel(N)` runs exactly `N` ops then traps — and bounds every loop since back-edges re-execute ops. We deliberately do *not* do compiler-injected block batching: runtime speed is secondary here, so a per-op decrement+branch (only when `Config::consume_fuel` is on; zero overhead otherwise) is the simpler, more precise choice. Depletion → `Trap::OutOfFuel` (sync) or yield (async). `set_fuel` (absolute) / `get_fuel` (remaining); both require `Config::consume_fuel`.
- **Epoch**: `Engine` holds an `AtomicU64` epoch. `Engine::increment_epoch()` is a plain atomic add, safe to call from a background thread/timer/signal. Each `Store` has an absolute deadline (epoch value); when `Config::epoch_interruption` is on, the run loop compares `current_epoch() >= deadline` per executed op (the same uniform checkpoint as fuel — simpler than back-edge-only, runtime speed secondary; a relaxed atomic load, only when enabled). On deadline the loop suspends; the generic driver applies the policy: trap (`Trap::Interrupt`, the default) or invoke a `Store::epoch_deadline_callback` returning `UpdateDeadline` (`Interrupt` → trap, `Continue(n)` → extend deadline by `n` and resume, `Yield` → async/§11). `set_epoch_deadline` is inert unless `epoch_interruption` is enabled. The embedder runs the ticker (we never spawn it); `Engine::weak` gives it a non-owning handle.

Fuel is deterministic but charges per-instruction; epoch is cheap but non-deterministic — both offered, matching wasmtime's tradeoff.

## 13. Memory & tables

- **Linear memory**: `MemoryEntity { bytes: Vec<u8>, min, max }`; 64 KiB pages. The loop caches `(*base, len)` for the active memory to avoid per-access indirection. **Software bounds check** on every access: effective address = `offset + dynamic`, both checked against `len`; out-of-range → `Trap::MemoryOutOfBounds`. `memory.grow` resizes the `Vec` (subject to `max` and the limiter), returns the old page count, `-1` on failure, and refreshes the cached base/len.
- **Tables**: `TableEntity { elems: Vec<Ref>, elem_type, max }`. Bulk ops (`table.init/copy/fill`) bounds-check the **whole range before** any write so a trap leaves the table unmodified; `table.copy` handles overlap by direction. `table.grow` returns old size / `-1`.
- **Element / data segments**: active (applied at instantiation), passive (kept for `table.init`/`memory.init`), declarative (only declares funcs for `ref.func`).
- **Limits**: module-level (`MemoryType`/`TableType` min/max) plus a store-level `ResourceLimiter` (`memory_growing`, `table_growing`, count caps) installed via `Store::limiter`, with a `StoreLimitsBuilder` convenience (`memory_size`, `table_elements`, `instances/tables/memories`, `gc_heap_*`, `trap_on_grow_failure`). The limiter is consulted on: memory/table **grow** — both the public `Memory::grow`/`Table::grow` *and* the in-wasm `memory.grow` op (routed through the generic driver since the limiter is `T`-generic; soft-deny ⇒ `-1`, or trap when `trap_on_grow_failure`); **initial size** at `Memory::new`/`Table::new`; and **instance/memory/table counts** at instantiation. Exhaustive coverage of the remaining growth paths (`table.grow`/`fill`/`init`/`copy`, GC-heap allocation) is the Phase-8 hardening gate (#31).

## 14. Garbage collection (phase 5)

- **Heap**: a handle table in the `Store`. A `GcHandle` is an index (+ generation) — not a raw pointer — so references stay stable across collection and a future moving collector remains possible. Heap holds only **structs and arrays** (plus a tiny `extern` wrapper for `any.convert_extern` of a host externref); `i31` and nulls are unboxed and never allocated; `externref` payloads live in their own arena.
- **Compact, packed object bodies (one byte buffer).** An object's body is a single tightly-packed `Box<[u8]>` — **one allocation per object**, scalars at their natural width and references as a 4-byte handle (null = a reserved `NULL_REF` sentinel). The field/element **types are encoded once per type** in a per-canonical-type `Layout` (`canon/layout.rs`: a list of `Slot { offset, ScalarKind | RefKind }` for structs, an element `Slot` + `stride` for arrays), *not* per element — so an `array i8` costs 1 byte/element, not `size_of::<Val>()` (≈32×). Scalars are read/written with safe little-endian byte codecs (`store/gc_codec.rs`, `from_le_bytes`/`to_le_bytes`: no `unsafe`, no alignment requirement — one `mov` on x86-64/aarch64 where unaligned scalar loads are free; deterministic across host endianness). The `Layout` is computed once at intern and baked into `ModuleInner` by type index for **lock-free** interpreter access; it is also the object map a future tracing collector will trace through. `array.new_data`/`copy` become direct byte copies (segment LE layout == body layout). We still do **one Rust allocation per object** (no pre-reserved arena / bump region): *(i)* fast startup is the #1 priority and we've banned mmap/guard pages, so a reserved region is *committed* memory; *(ii)* an arena only pays off with a **moving** collector — a non-moving mark-sweep over a bump region would reimplement `malloc`, whereas Rust's allocator already is one; *(iii)* pointer stability comes from the `GcHandle`, not from owning the bytes; *(iv)* zero `unsafe`, and simpler. This is *not* inconsistent with linear memory being a grown `Vec<u8>` (§13): both are now byte buffers; GC objects are just individually-typed and individually-reclaimed, hence the handle-table + per-object model. (A moving/compacting collector with an arena remains the path *if* priorities ever invert to throughput; the `GcHandle` indirection keeps that door open.)
- **Object header**: canonical type id (for casts + field layout) + a kind tag (struct/array/extern) + `len` for arrays + collector metadata (a mark bit). The body holds no per-element tags — the `Layout` supplies the types. The public `Val` (wasmtime parity) is unchanged; the compact form is internal, materialized to/from `Val` only at field access.
- **Type identity / canonicalization**: rec groups are canonicalized to a position-independent form and interned to a canonical type id stored in object headers; `ref.cast`/`ref.test` become id comparison + a supertype-chain walk (O(depth); no display/RTT tables, matching Wizard). ⚠️ Never mix relative (decoder-local) and canonical (runtime) type indices — that confusion is a known RCE class (CVE-2024-12053).
- **Type-registry reclamation (#27i, matching wasmtime)**: the engine `TypeRegistry` is **per-rec-group reference counted**. A `Module` holds its groups (released on drop); the public type handles (`FuncType`/`StructType`/`ArrayType`) and `RecGroup` are **RAII** — each holds one registration, `Clone` increfs, `Drop` decrefs. Registering a group **pins the groups it references** (cross-group `CanonRef::Canon` edges incref'd on a hash-cons miss, decref'd on reclaim via a drop-cascade worklist), so an inter-group reference keeps its target alive. Reclamation frees the whole group's canonical-id + group slots for reuse. **No generation/epoch** — the refcount invariant *is* the stale-id safety (you cannot hold a usable canonical id without holding a registration). A `GcHeader` stores only a bare id, so the **`Store` pins the types of host-allocated GC objects** (`gc_host_alloc_types`, released on store drop) — guest-object types are pinned by the defining instance's module. The refcount is a lock-guarded `u32` (not a per-entry atomic); incref/decref happen only at the embedder/materialization boundary, never on the run loop, and **two-phase materialization** (clone canonical body under the read lock, build handles after release) avoids nesting a write lock inside it.
- **Collector: non-moving, stop-the-world mark-sweep** over the handle table. Chosen over deferred reference counting because it fits this project's priorities better: *(i)* runtime speed is explicitly secondary, so stop-the-world pauses are acceptable; *(ii)* precise root enumeration — the one hard part of tracing — is **already free** for us: `Val` is a tagged enum and every root lives in an explicit, walkable structure in the `Store` (we never paid Wizard's ~8% value-stack-tagging cost because the tag *is* the runtime representation); *(iii)* no write barriers and no inc-before-dec ordering subtleties on the hot store path; *(iv)* it **collects cycles**, which DRC leaks. The collector strategy is internal and invisible at the API boundary (`Config::collector`/`Collector` still match wasmtime).
  - **Mark**: from the root set, trace reachable structs/arrays through their reference-typed fields/elements, setting the header mark bit. A single unified trace (not per-hierarchy) because `extern.convert_any`/`any.convert_extern` let references span hierarchies.
  - **Sweep**: free unmarked handle-table slots (bumping the slot generation so stale `GcHandle`s fault), clear marks. Non-moving ⇒ no pointer rewriting; the generation field still catches use-after-free.
  - **No write barriers, no refcount field** — stores (`struct.set`, `array.set/fill/copy/init_*`, `global.set`, `table.set/...`) and the hot `local.get/set` path are plain moves; correctness is established at collection time by the precise root walk, so the `x.f = x.f` hazard cannot arise.
  - **When to collect — two axes, both only at safe points** (never mid-instruction; precise rooting is free at *every* allocation point, so each one already qualifies as a safe point — no stack maps, no constrained collection sites):
    - **Per-store budget (the limiter axis).** Each store holds a local byte budget drawn from its `ResourceLimiter` in **batches**, not per object — an individual `struct.new`/`array.new` just decrements the budget (no limiter call, no atomic, plain subtraction). When the budget is exhausted we **collect first**, then: if the live set + the pending object fits the current reservation, continue with no limiter call at all; otherwise request the next batch from the limiter; a refusal → `Trap` (GC-heap OOM). After each collection the next trigger is retuned to `live_bytes * factor`, so a small live set stays small and a large one collects less often instead of thrashing. Consequence: a short-lived store that never exceeds its initial budget **never collects — it just drops**, which is the common case we optimize for. (This mirrors §13: linear memory likewise consults the limiter only on `memory.grow`, not per byte. Calling the `&mut self` limiter per object would be both wasteful and off-model — wasmtime accounts the GC heap in growth chunks too.)
    - **Engine-wide pressure.** The `Engine` keeps an `AtomicUsize` of total committed GC bytes across all its stores, updated **at batch granularity** — a delta when a store reserves/releases a batch, and on `Store::drop` — never per object, so there is no hot-path cost and no per-thread flush-at-threshold trick is needed (the batch already coarsens it). When the counter crosses `Config::gc_memory_threshold` (below), the engine sets a *GC-requested* flag that each store checks at the **same back-edge safe point as fuel/epoch** (§12) and then self-collects at its next safe point. The engine **requests, never forces**: a `Store` is `!Sync` (§17), so the engine cannot reach into another thread's heap — and a store about to finish simply drops without ever honoring the request. To avoid a thundering herd, a store honors the request only when its own footprint exceeds a floor; tiny tenants ignore engine pressure. Idle stores between calls are unreachable by the engine (owner-thread only) — acceptable under the many-short-lived-stores model, where they drop rather than linger.
  - **`Config::gc_memory_threshold(bytes)` — the one additive deviation from wasmtime's `Config`.** Engine-wide high-water mark that drives the pressure axis above; defaults to **~80% of detected physical RAM** (with a fixed fallback if detection fails). Everything else in `Config` matches wasmtime exactly — this knob is ours because the engine-wide axis has no wasmtime analog. True OS-level memory-pressure signals (macOS dispatch source, Linux PSI/cgroup) may feed the *same* GC-requested flag through an **embedder-driven hook**; we never spawn a monitor thread ourselves, consistent with the epoch ticker (§12).
- **`externref` is collected too** — that's the whole point: an `externref`'s payload is **host-owned Rust data** (a `Box<dyn Any + Send + Sync>` from `ExternRef::new`), and dropping that box runs the host type's `Drop` impl, which is the *only* mechanism that releases the memory/handle/file behind it. Mechanics:
  - **Sweep drops the box.** No write barriers, no refcount, no separate finalizer API — Rust `Drop` *is* the release hook. (The arena holds no type-id/fields, which is why it's separate from the struct/array object representation.)
  - **Reachability is unified.** An `externref` can be kept alive solely via a struct/array field (laundered through `any.convert_extern`/`extern.convert_any`), so the single mark pass traces into the GC heap — not just the host roots. This is the same "one unified trace, not per-hierarchy" rule above.
  - **Release is non-deterministic** — the host `Drop` runs *at collection*, not when the last wasm reference disappears. Embedders must not rely on an `externref`'s `Drop` for prompt resource release. This matches `wasmtime`, which also GCs `externref` rather than ref-counting it.
  - **Store teardown is a guaranteed sweep.** `Store::drop` owns the arena and runs `Drop` on every remaining live payload, so nothing leaks past the `Store`'s lifetime even if it was never collected during execution.
  - Contrast: `funcref` needs **no** GC-drop — it points at a `Func` entity that lives in the Store arenas for the Store's whole life; there is no host-owned payload to reclaim early. The early-release concern is `externref`, GC `struct`/`array`, and `exnref` payloads only.
- **Roots** (enumerated precisely at each collection): operand stack, locals, globals, tables, `exnref` payloads, and host-held references (`Rooted`/`RootScope`/`ManuallyRooted` on the embedder side).
- **GC heap limits** mirror the linear-memory controls (reservation/max via the limiter).

## 15. Exception handling (phase 6)

Target the **current** standardized proposal (`exnref` + `try_table`), not legacy `try/catch/delegate` (decode-only for compat, if at all).

- **Tag section**: a tag references a function type whose params are the exception's argument types (results must be empty); tags are matched at runtime by **store address identity**, not signature.
- **`try_table`** is a normal control block with a `blocktype` and a fixed vector of catch clauses (`catch`, `catch_ref`, `catch_all`, `catch_all_ref`). Each clause is precompiled into the **same `BranchTarget` machinery** as `br`: target ip + the values pushed to the label (params; params+exnref; nothing; exnref) + the operand-height restore.
- **`throw`** builds an exception instance and unwinds; **`throw_ref`** re-throws an `exnref` (traps if null). Both are stack-polymorphic (validated like `unreachable`).
- **Unwinding**: each `Frame` keeps a handler stack. On throw, search the current frame's handlers in order (tag-address match for `catch`/`catch_ref`, always for `*_all`); on match, restore the operand stack to the handler's recorded height, push the clause payload, and transfer control like a `br`. No match in the frame ⇒ pop the frame and continue in the caller. Exhausted ⇒ surface to the embedder. Handler records are exactly the `try_table` labels, so this reuses §5 + §7 structures.

## 16. Error & trap model (`wasmtime`-compatible)

- The embedder API uses **our own `Error`/`Result`** (`pub type Result<T, E = Error>`), mirroring wasmtime 45.x — which **no longer uses `anyhow::Error`** (it ships its own `Error`/`Result`/`bail!`/`ensure!`/`format_err!`). Our `Error` is a thin wrapper over `anyhow::Error` exposing the same surface (`new`/`msg`/`context`/`downcast`/`downcast_ref`, `From<E: std::error::Error>`, `From<Error> for anyhow::Error`). Specific error kinds are *attached* and recovered via `.downcast_ref::<T>()`.
- **`Trap`** is a `Copy`, `#[non_exhaustive]` enum of wasm trap codes (`StackOverflow`, `MemoryOutOfBounds`, `TableOutOfBounds`, `IndirectCallToNull`, `BadSignature`, `IntegerOverflow`, `IntegerDivisionByZero`, `BadConversionToInteger`, `UnreachableCodeReached`, `Interrupt`, `OutOfFuel`, `NullReference`, `CastFailure`, `ArrayOutOfBounds`, …) — matching `wasmtime::Trap`'s variant names. It implements `std::error::Error`, so it is carried *inside* an `Error` (via the blanket `From`), not the error type itself.
- **`WasmBacktrace`** (also attached to the error, `capture`/`force_capture`/`frames()`) is straightforward for us because we hold the explicit frame stack; gated by `Config::wasm_backtrace` (default on), like `wasmtime`. Captured by walking the live `Vec<Frame>` at trap/throw time as raw `(func, code_offset)` pairs; source-level symbolication (file/line/column, inlined frames) is resolved lazily from DWARF only when `frames()`/`symbols()` is inspected (next bullet).
- **DWARF / backtrace symbolication (Phase 7).** When the guest module carries DWARF (`.debug_*` custom sections) and/or a `name` section, we build a per-`Module` index (code offset → `(func, file, line, column)` + inlined chains) via `gimli`, off the compile path, and use it to symbolicate `FrameInfo`/`FrameSymbol`. Gated by `Config::wasm_backtrace_details` (DWARF file/line) and `Config::debug_info` (DWARF retention; its native-debugger aspect is a no-op for an interpreter). **Exception backtraces** are captured at `throw`/`throw_ref` — snapshotting the full throw-site→boundary chain *before* the §15 unwinder pops any frame, then carried on the exception/error as it propagates. This is where `wasmtime`'s exception backtraces fall short; our explicit frame stack makes the correct snapshot cheap. A rethrow (`throw_ref`) keeps the original throw-site backtrace.
- Host-function `Err(Error)` propagates as a trap and resurfaces from the outer `call`; `Err(Trap::X.into())` registers a specific code; `bail!("…")` constructs a message error.

## 17. Concurrency & thread-safety

- `Engine` is `Arc`-backed, `Send + Sync`; shared across threads and stores. Its mutable shared state is atomic only: the epoch counter (§12) plus the engine-wide GC-byte counter and the GC-requested flag that drive the GC pressure axis (§14).
- `Store<T>` is single-threaded-owner (`Send where T: Send`), `!Sync`; one execution at a time per store. Many stores run concurrently on the shared engine.
- `Module` is immutable post-compilation and shareable across stores of the same engine.

## 18. Dependencies

- **`wasmparser`** — parsing + validation (required, central).
- **`anyhow`** — the error currency, required for `wasmtime`-compatible `Result`/error attachment semantics.
- **`gimli`** — DWARF parsing for symbolicated backtraces (Phase 7); used lazily, off the compile/startup path. (`wasmparser` exposes the `.debug_*` custom sections.)
- Numeric helpers as needed (e.g. float rounding/trunc semantics) — prefer std; minimal external crates.
- For async: no runtime dependency; we expose `async fn`/futures and let the embedder's executor drive (compatible with tokio/async-std/smol).
- We pin a reference **`wasmtime` version (45.x)** to track signatures against (a dev-dependency for compatibility/differential tests; not a runtime dependency).
- Dev: the **WebAssembly spec test suite** vendored as a git submodule; **`wast`** to parse `.wast`, **`wat`** for inline test modules. See [TESTING.md](./TESTING.md).

## 19. Testing strategy

The conformance gate per phase is the official WebAssembly spec `.wast` suite. Full test-vector sourcing, the per-proposal file map, and the harness design live in **[TESTING.md](./TESTING.md)**. In brief:

- **Spec conformance** (primary gate): vendor `WebAssembly/testsuite` (its `main` *is* Wasm 3.0 — MVP + all our proposals except legacy EH are merged into the flat root; legacy EH under `legacy/`). A `tests/spec.rs` runner parses each `.wast` with the `wast` crate, dispatches directives (`Module`, `AssertReturn`, `AssertTrap`, `AssertInvalid`, `AssertMalformed`, `AssertUnlinkable`, `Register`, `AssertException`, …), registers the `spectest` host shim, and uses a `should_fail`/skip allowlist that shrinks as phases land.
- **Compatibility tests**: compile a set of `wasmtime` example programs against `submilli_wasm` (via `use submilli_wasm as wasmtime;`) to prove drop-in source compatibility.
- **Unit tests** per module (compile-pass branch resolution, operand-stack discipline, memory bounds, precise GC root enumeration).
- **Differential tests** (optional): compare results against `wasmtime`/`wasmi` on generated modules.
- **GC tests**: precise root enumeration, reachability through struct/array fields, cycle reclamation, and stale-`GcHandle` rejection after sweep.
```
