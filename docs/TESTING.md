# Testing & Conformance Vectors

How we prove correctness, where the authoritative test vectors live, and how to run them. The spec `.wast` suite is the gate for every feature.

## 1. Where the test vectors live

**Key fact:** WebAssembly **3.0** is finalized (2025). Every proposal we implement *except legacy exception-handling* is already **merged into the flat root** of `WebAssembly/testsuite` (mirror of `WebAssembly/spec/test/core`). There is **no separate "3.0" branch** — `main` *is* 3.0. So one vendored repo covers almost everything.

- **Authoritative source:** `WebAssembly/spec` `main` `/test/core/*.wast` (file bugs/PRs here).
- **Convenient mirror:** `WebAssembly/testsuite` `main` — auto-generated weekly, flat root, **what we vendor**.
- Each proposal repo is a *fork* of the spec repo; its `test/core` is the full superset (only needed for pre-merge edge files).

### Per-feature map

| # | Feature | Repo / branch | Path | Notes |
|---|---------|---------------|------|-------|
| 1 | **Core MVP** | `WebAssembly/testsuite` `main` | `*.wast` (root) | `i32/i64/f32/f64.wast`, `memory.wast`, `address.wast`, `call.wast`, `call_indirect.wast`, `global.wast`, `const.wast`, control-flow, `fac.wast`, … |
| 2 | **mutable-globals** | merged → testsuite `main` | `global.wast`, `globals.wast` | proposal repo `WebAssembly/mutable-global` is historical |
| 3 | **sign-extension-ops** | merged → testsuite `main` | folded into `i32.wast`, `i64.wast` | historical proposal repo |
| 4 | **multi-value** | merged → testsuite `main` | spread across `block.wast`, `loop.wast`, `if.wast`, `call.wast`, `func.wast`, … | historical proposal repo |
| 5 | **reference-types** | merged → testsuite `main` | `ref.wast`, `ref_null.wast`, `ref_is_null.wast`, `ref_func.wast`, `table*.wast`, `bulk.wast`, `select.wast` | |
| 6 | **function-references** | merged → testsuite `main` | `call_ref.wast`, `return_call_ref.wast`, `br_on_null.wast`, `br_on_non_null.wast`, `ref_as_non_null.wast`, updated `table*.wast`/`func.wast` | proposal repo `WebAssembly/function-references` `main` `test/core/` for extras |
| 7 | **GC** | merged → testsuite `main` | `struct.wast`, `array*.wast`, `i31.wast`, `br_on_cast.wast`, `br_on_cast_fail.wast`, `type-rec.wast`, `type-canon.wast`, `type-equivalence.wast`, `type-subtyping.wast`, `ref_cast.wast`, `ref_test.wast`, `ref_eq.wast`, `extern.wast` | proposal repo `WebAssembly/gc` `main` keeps a `test/core/gc/` subdir the mirror flattens |
| 8a | **exception-handling** (current: `exnref`/`try_table`) | merged → testsuite `main` | `tag.wast`, `throw.wast`, `throw_ref.wast`, `try_table.wast` | proposal repo `WebAssembly/exception-handling` `main` |
| 8b | **exception-handling** (legacy: `try/catch/delegate/rethrow`) | testsuite `main` | `legacy/{rethrow,throw,try_catch,try_delegate}.wast` | only if we decode-accept legacy; otherwise skip |

URLs: <https://github.com/WebAssembly/testsuite> · <https://github.com/WebAssembly/spec/tree/main/test/core> · <https://github.com/WebAssembly/gc/tree/main/test/core> · <https://github.com/WebAssembly/exception-handling/tree/main/test/core> · <https://github.com/WebAssembly/function-references/tree/main/test/core>

## 2. Vendoring

```sh
git submodule add https://github.com/WebAssembly/testsuite tests/testsuite
# pin a specific commit (the mirror updates weekly):
cd tests/testsuite && git checkout <commit> && cd -
```

**Status:** vendored at `tests/testsuite`, pinned to `0dc0343c9876267d99a7577ed4fc2289406a7869` (`main`, Wasm 3.0). The runner `tests/spec.rs` is currently a **scaffold**: it parses the whole suite and reports directive counts (256/257 root files parse; `names.wast` hits `wast`'s confusing-unicode lexer guard), but execution is gated behind an empty `should_execute` allowlist until **Task #15** turns it on per suite. CI must run `git submodule update --init`.

Dev-dependencies (all from `bytecodealliance/wasm-tools`):
- **`wast`** — parses `.wast` into directives (the harness). <https://docs.rs/wast>
- **`wat`** — `wat::parse_str(s) -> Vec<u8>` for inline unit-test modules. <https://docs.rs/wat>
- (optional) **`wasmparser`** for cross-checking; **`wasmtime`** (pinned) for differential + drop-in compatibility tests.

## 3. The `.wast` directive surface

`wast::WastDirective` variants the runner must handle:

| Directive | Action |
|---|---|
| `Module` / `ModuleDefinition` / `ModuleInstance` | decode + validate (+ instantiate); `.encode()` → bytes |
| `Register { name, module }` | expose an instance under `name` for later imports |
| `Invoke` | call an export (no assertion) |
| `AssertReturn { exec, results }` | run; assert results equal (handle `nan:canonical`/`nan:arithmetic`, v128 lanes — **not** `==`) |
| `AssertTrap { exec, message }` | assert trap; **substring**-match the message |
| `AssertExhaustion` | assert stack-overflow/resource-exhaustion trap |
| `AssertInvalid { module, message }` | module must fail **validation** |
| `AssertMalformed { module, message }` | module must fail **decoding** |
| `AssertUnlinkable { module, message }` | instantiation/linking must fail |
| `AssertException { exec }` | invocation must throw (EH) |
| `Thread` / `Wait` | skip (threads proposal, out of scope) |

Notes:
- **`spectest` host module is mandatory**: spec tests import `spectest` (funcs `print_i32`/`print_f64`/…, a `global_i32`, a `table`, a `memory`). Register it before instantiating or many tests fail at link time.
- **Message matching is substring-based** across engines — mirror that.

## 4. Runner sketch (`tests/spec.rs`)

Same pattern as wasmtime (`tests/wast.rs` + `wasmtime-wast`) and wasmi (`wasmi_wast`): both vendor the suite as a submodule and keep a `should_fail`/skip allowlist that shrinks per phase.

```rust
fn run_wast(path: &Path, allow: &SkipList) -> Result<()> {
    let text = fs::read_to_string(path)?;
    let buf = wast::parser::ParseBuffer::new(&text)?;
    let wast: wast::Wast = wast::parser::parse(&buf)?;

    let mut env = TestEnv::new_with_spectest();  // Engine + Store<T> + Linker + "spectest"
    let mut current = None;                       // last instantiated instance
    let mut named = HashMap::new();               // (register "name") -> instance

    for d in wast.directives {
        match d {
            WastDirective::Module(mut m) => {
                let bytes = m.encode()?;
                current = Some(env.instantiate(&bytes)?);
            }
            WastDirective::Register { name, module } => {
                env.register(name, lookup(module, &named, &current));
            }
            WastDirective::AssertReturn { exec, results } => {
                let got = env.run(exec, &current, &named)?;
                assert_results_match(&got, &results);   // NaN/v128-aware
            }
            WastDirective::AssertTrap { exec, message } => {
                let err = env.run(exec, &current, &named).unwrap_err();
                assert!(err.to_string().contains(message));
            }
            WastDirective::AssertInvalid { mut module, message } =>
                assert_validation_fails(module.encode()?, message),
            WastDirective::AssertMalformed { mut module, .. } =>
                assert_decode_fails(module.encode()),    // encode() may itself error
            WastDirective::AssertUnlinkable { mut module, message } =>
                assert_link_fails(module.encode()?, message),
            WastDirective::Invoke(i) => { env.invoke(i, &current, &named)?; }
            WastDirective::AssertException { exec } =>
                assert!(env.run(exec, &current, &named).is_err()),
            _ => {} // Thread/Wait etc.
        }
    }
    Ok(())
}
```

Drive it by globbing `tests/testsuite/*.wast` (+ `legacy/*.wast` if doing legacy EH), filtered by the per-phase skip allowlist.

## 5. Per-phase conformance targets

| Phase | `.wast` files that must pass |
|---|---|
| 1 — Core | MVP set + multi-value-affected (`block/loop/if/call/func`) + `i32.wast`/`i64.wast` (sign-ext) + `global*.wast` (mutable-globals). Skip SIMD/threads/ref/gc/eh. |
| 2 — API depth | no new `.wast`; the linking tests (`linking.wast`, `imports.wast`, `(register …)`) plus our own fuel/epoch/limit API tests |
| 3 — Async | our own async API tests (no spec `.wast` for async) |
| 4 — Refs | `ref*.wast`, `table*.wast`, `select.wast`, `bulk.wast`, `call_ref.wast`, `return_call_ref.wast`, `br_on_null.wast`, `br_on_non_null.wast`, `ref_as_non_null.wast` |
| 5 — GC | `struct.wast`, `array*.wast`, `i31.wast`, `ref_cast/test/eq.wast`, `br_on_cast*.wast`, `type-rec/canon/equivalence/subtyping.wast`, `extern.wast` |
| 6 — EH | `tag.wast`, `throw.wast`, `throw_ref.wast`, `try_table.wast` (+ `legacy/*` if supported) |

## 6. Beyond `.wast`

- WABT's `wast2json` emits a `.json` manifest + per-module `.wasm`/`.wat` fixtures if we ever want prebuilt binary vectors instead of linking `wast`.
- `wat` crate for hand-written regression modules in unit tests (e.g. the multi-value branch-arity and GC cycle-reclamation cases).
