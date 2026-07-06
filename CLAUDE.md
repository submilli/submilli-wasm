# submilli-wasm

A WebAssembly interpreter in Rust with a **`wasmtime`-compatible (drop-in) API**.
Priority: **fast compilation/startup ≫ runtime speed**. Stack-based interpreter, no JIT.

Feature target: MVP + mutable-globals + sign-extension-ops + multi-value + reference-types
+ function-references + GC + exception-handling.

## Docs (read these first)
- `docs/ARCHITECTURE.md` — the design (interpreter core, value model, runtime, API, GC, EH).
- `docs/TESTING.md` — spec `.wast` conformance harness + where the test vectors live.
- `docs/STYLE.md` — coding style (enforced by clippy/rustfmt/CI).

## Commands
- Build: `cargo build`
- Lint: `cargo clippy --all-targets -- -D warnings` (warnings are hard failures)
- Format: `cargo fmt` (CI runs `cargo fmt --check`)
- Test: `cargo test`
- Spec suite: `cargo test --test spec` (needs `git submodule update --init`)

## Conventions (follow `docs/STYLE.md`)
- Small files (one module = one concern), short functions (≤50 lines), ≤6 args.
- **Don't over-document or over-test.** Self-documenting code; comment the *why*, not the *what*.
  Lean on the spec `.wast` suite for conformance; unit-test only error-prone logic.
- Public API names match `wasmtime` exactly — `#[allow]` clippy rather than break compatibility.
- `Result<T> = anyhow::Result<T>`; `Trap`/`WasmBacktrace` recovered via `downcast_ref`.
- The interpreter dispatch `match` is the only sanctioned long function
  (`#[allow(clippy::too_many_lines)]`, thin arms delegating to small per-op handlers).
- Minimal `unsafe`; any block needs a `// SAFETY:` comment.

## Security (untrusted multi-tenant guests — non-negotiable)
The interpreter runs untrusted, mutually-distrusting guest code. Treat every wasm-reachable
path as adversarial. See `SECURITY.md` (threat model).
- **Zero-on-allocation.** Every guest-visible allocation MUST be fully initialized before the
  guest can read it — linear memory, tables, locals, GC objects. A guest must never observe a
  prior tenant's freed memory or the allocator's stale bytes. Use `vec![0; n]` / `resize(.., v)`
  / explicit defaults. **Never** `set_len`/`MaybeUninit`/`with_capacity`-then-expose to skip
  zeroing, even for startup speed. Any pooling/recycling allocator MUST zero on reuse-or-return.
- **No `unsafe`.** Spatial isolation rests on safe-Rust bounds checks; keep the tree at zero
  `unsafe`. If one is ever truly unavoidable, it needs a `// SAFETY:` proof *and* explicit signoff.
- **No panic on validated input.** A panic = whole-process DoS. In guest-reachable paths, trap
  (return `Err(Trap::…)`) instead of `unwrap`/`expect`/slice-indexing/`as`-truncation/overflowing
  arithmetic. Use `checked_*`/`get`/explicit bounds checks; reserve `expect` for genuine
  post-validation invariants (and say why).
- **Bounds-check every guest access.** Effective addresses via `checked_add`, compared against
  length, before touching memory/tables; out-of-range → trap, never UB.
- **Bound every resource.** Route all growth/allocation/counts through the `ResourceLimiter`;
  enforce fuel/epoch and the stack-size limit (`max_wasm_stack`, in **bytes** like wasmtime —
  account our heap-allocated frame/operand stacks against it; there is no native stack to overflow).
  Guest code runs at instantiation (the `start`
  function), so limits must hold there too — never assume work only happens on export calls.
- **Type-index discipline (GC/EH).** Never mix relative (decoder-local) and canonical (runtime)
  type indices — that confusion is a known RCE class (CVE-2024-12053).
- **Contain host-fn panics** at the call boundary; one tenant must not poison the shared engine.

## Workflow
Pinned reference: `wasmtime` 45.x (dev-dependency for compatibility/differential tests).
