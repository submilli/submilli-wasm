# Coding Style

Clarity over cleverness. This is an interpreter whose stated priority is fast compilation and *maintainability*, not peak runtime cleverness — the code should read that way. The rules below are enforced by `clippy.toml`, `rustfmt.toml` and the `[lints]` block (below).

## Guiding principles

1. **Small files.** One module = one concern. When a file grows unwieldy, split it (e.g. `exec/numeric.rs`, `exec/memory.rs`, `exec/control.rs` rather than one giant `exec.rs`).
2. **Short functions.** Target **≤ 50 lines** per function (clippy enforces). One function does one thing. Prefer early returns over nesting; extract helpers liberally — extracting is free, reading a 200-line function is not.
3. **Few arguments.** **≤ 6 parameters** (clippy enforces). If you need more, bundle them into a `struct` (e.g. an `ExecCtx`/`CompileCtx`) and pass that.
4. **Low complexity.** Keep cyclomatic/cognitive complexity low (clippy `cognitive_complexity`). Deeply nested `if`/`match` is a smell — flatten with early returns, helper fns, or `?`.
5. **No `unsafe` without justification.** `unsafe_code` is warned. The runtime is allocation-backed and portable by design (§ARCHITECTURE 2/6) — we should have essentially none. Any `unsafe` block needs a `// SAFETY:` comment explaining the invariant.
6. **Errors, not panics, in library code.** Return `Result` (`anyhow` at the API boundary, specific errors internally). `unwrap()` is warned outside tests; use `?`, `.context(...)`, or an explicit error. `todo!()`/`unimplemented!()` are fine only as scaffolding and must be gone before a phase's gate.
7. **Document and test in proportion.** Self-documenting code first; comment the non-obvious, not the obvious. Lean on the spec suite for conformance and unit-test only error-prone logic. Neither over-document nor over-test (see the two sections below).

## The dispatch-loop exception

The central interpreter `match op { … }` is the one place a long function is acceptable — but keep it a **thin dispatch table**: each arm is ideally a single line that delegates to a small, individually-testable handler (`Op::I32Add => self.exec_i32_add(),`). The numeric/memory/control handlers stay short and live in their own files. The dispatch fn itself carries an explicit, commented `#[allow(clippy::too_many_lines)]`. Do **not** use that allow anywhere else without discussion.

## Naming & API

- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) for casing, conversions (`as_`/`to_`/`into_`), and getters.
- **Public API names match `wasmtime` exactly** (see ARCHITECTURE §9) — this overrides any local naming preference. If clippy suggests renaming a public item to satisfy a lint, `#[allow]` it rather than break compatibility (`avoid-breaking-exported-api = true` already steers clippy away from this).
- Internal names: descriptive, not abbreviated — `operand_stack`, not `os`. The pinned-register-style terse names from wasm3/Wizard are *not* our style.

## Modules & organization

- Keep `mod.rs` files small — they wire submodules and re-export, they don't hold logic.
- Group by concern matching ARCHITECTURE §3. Split a phase's work across files from the start rather than growing one and splitting later.
- `pub(crate)` by default for internal items; `pub` only for the wasmtime-compatible surface. `unreachable_pub` is warned to catch accidental leaks.

## Documentation & comments — in proportion, not exhaustive

Prefer self-documenting code over prose. **Do not over-document.**

- Comments earn their place by explaining **why** — a non-obvious invariant, a spec corner, a deliberate tradeoff. Don't narrate **what** the code already says; a comment that restates the signature or the next line is noise, delete it.
- Document the **non-obvious** algorithms (folded-sidetable branch resolution, precise GC root enumeration, zero-copy call frames) with a short note pointing at the relevant ARCHITECTURE section. Obvious code gets nothing.
- Public items: a concise `///` summary where it adds information (and a "≈ `wasmtime::X`" note for compat items). A one-liner that just echoes the name is not required — skip it. We do **not** turn on `missing_docs`.
- No commented-out code; git is the history. No doc boilerplate (`# Arguments`/`# Returns` sections) unless a parameter genuinely needs explaining.

## Tests — cover what can break, not everything

Test behavior and the parts that are easy to get wrong. **Do not over-test.**

- **The spec `.wast` suite (TESTING.md) is the primary correctness gate** — it covers the language exhaustively. Don't re-create that coverage with hand-written unit tests; lean on it.
- Add a focused unit test only for **non-trivial, error-prone logic** that the spec suite doesn't isolate well: branch `keep`/`pop` computation, bounds-check edge cases, precise GC root enumeration / cycle reclamation, the multi-value loop-arity rule.
- **Don't** unit-test getters, trivial conversions, or anything the type system/spec suite already guarantees. One good test beats five redundant ones.
- Colocate unit tests in `#[cfg(test)] mod tests` at the bottom of the file; `unwrap()`/`expect()` are fine in tests.

## Formatting

`cargo fmt` is mandatory (CI checks `--check`). Config in `rustfmt.toml`. Run before every commit.

## The lint config (add to `Cargo.toml` when scaffolding — Task #1)

```toml
[lints.rust]
unsafe_code = "warn"
unreachable_pub = "warn"
missing_debug_implementations = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }

[lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
# clean-code enforcers (explicit for visibility; thresholds in clippy.toml)
too_many_lines = "warn"
too_many_arguments = "warn"
type_complexity = "warn"
cognitive_complexity = "warn"        # nursery lint, enabled individually
unwrap_used = "warn"                  # restriction; allowed in tests
dbg_macro = "warn"
# curated allows: pedantic noise that fights interpreter code / wasmtime compat
module_name_repetitions = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
must_use_candidate = "allow"
similar_names = "allow"
doc_markdown = "allow"
cast_possible_truncation = "allow"   # pervasive & intentional in a wasm interpreter
cast_sign_loss = "allow"
cast_possible_wrap = "allow"
cast_lossless = "allow"
```

CI runs `cargo clippy --all-targets -- -D warnings` so every warning is a hard failure.
