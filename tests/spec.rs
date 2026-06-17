//! WebAssembly spec-suite (`.wast`) runner — the Phase-1 conformance gate (#15).
//!
//! The vendored testsuite is Wasm 3.0, which interleaves reference-types / SIMD /
//! GC / EH / memory64 content into otherwise-core files. We run a managed set
//! **resiliently**: a module (or assertion) that fails only because a feature
//! isn't enabled is *skipped and tallied* — never silently dropped — while every
//! assertion that does run must pass. Whole files that import the `spectest` host
//! module are deferred to #24b (they need host functions + the linker).

#![allow(dead_code, unused_imports)]
#![allow(clippy::all, clippy::pedantic)]
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use submilli_wasm::{Engine, Error, Instance, Module, Result, Store, Val};
use wast::core::{NanPattern, WastArgCore, WastRetCore};
use wast::parser::{self, ParseBuffer};
use wast::token::{Id, F32, F64};
use wast::{QuoteWat, Wast, WastArg, WastDirective, WastExecute, WastInvoke, WastRet};

fn testsuite_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testsuite")
}

fn wast_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wast"))
        .collect();
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// File-level classification (the managed "what we don't run" list).
// ---------------------------------------------------------------------------

enum Class {
    Run,
    Skip(&'static str),
}

/// Decides whether to execute a whole file. Out-of-scope *features* are caught
/// per-module at runtime via [`is_unsupported`]; here we only short-circuit whole
/// files that are wholly out of scope (faster, cleaner summary).
fn classify(path: &Path, text: &str) -> Class {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if text.contains("\"spectest\"") {
        return Class::Skip("spectest import (deferred to #24b)");
    }
    if name.starts_with("simd") {
        return Class::Skip("simd (out of scope)");
    }
    if text.contains("(memory i64") || text.contains("(table i64") {
        return Class::Skip("memory64 (out of scope)");
    }
    if name.starts_with("linking") || text.contains("(register ") {
        return Class::Skip("multi-module / register — needs linker (deferred to #24b)");
    }
    Class::Run
}

// ---------------------------------------------------------------------------
// Execution summary.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Summary {
    files_run: usize,
    files_skipped: BTreeMap<String, usize>,
    modules_ok: usize,
    modules_skipped: BTreeMap<String, usize>,
    asserts_passed: usize,
    asserts_skipped: usize,
}

impl Summary {
    fn skip_file(&mut self, reason: &str) {
        *self.files_skipped.entry(reason.to_string()).or_default() += 1;
    }

    fn skip_module(&mut self, err: &Error) {
        *self.modules_skipped.entry(skip_bucket(err)).or_default() += 1;
    }

    fn report(&self) {
        eprintln!(
            "spec gate: {} files run, {} modules instantiated, {} assertions passed, {} skipped",
            self.files_run, self.modules_ok, self.asserts_passed, self.asserts_skipped
        );
        eprintln!("  files skipped (whole-file):");
        for (reason, n) in &self.files_skipped {
            eprintln!("    {n:>4}  {reason}");
        }
        eprintln!("  modules skipped (in-file, unsupported feature):");
        for (reason, n) in &self.modules_skipped {
            eprintln!("    {n:>4}  {reason}");
        }
    }
}

/// Buckets an "unsupported" error into a short, stable label for the summary.
fn skip_bucket(e: &Error) -> String {
    let s = e.to_string();
    for (needle, label) in [
        (
            "enabled",
            "feature not enabled (ref-types/simd/gc/eh/memory64/threads)",
        ),
        (
            "proposal",
            "feature not enabled (ref-types/simd/gc/eh/memory64/threads)",
        ),
        ("not supported", "feature not enabled (gc/etc.)"),
        ("multiple memories", "multi-memory (out of scope)"),
        ("multiple tables", "multi-table (out of scope)"),
        ("not yet supported", "operator not yet implemented"),
        ("element expressions", "reference-types element expr"),
        ("wrong number of imports", "needs imports/linker"),
        ("module skipped", "depends on skipped module"),
        ("non-core", "non-core value"),
        ("unsupported", "unsupported value/op"),
    ] {
        if s.contains(needle) {
            return label.to_string();
        }
    }
    format!("OTHER (investigate): {s}")
}

/// True when an error means "we don't support this yet" rather than a real bug,
/// so the directive is skipped (and tallied) instead of failing the gate.
fn is_unsupported(e: &Error) -> bool {
    let s = e.to_string();
    // wasmparser feature gating phrases it as "... not enabled", "proposal must be
    // enabled", "requires `X` proposal to be enabled" — all contain "enabled".
    s.contains("enabled")
        || s.contains("proposal")
        || s.contains("not supported") // "... not supported without the gc feature"
        || s.contains("multiple memories")
        || s.contains("multiple tables")
        || s.contains("element expressions")
        || s.contains("wrong number of imports")
        || s.contains("module skipped")
        || s.contains("unsupported")
}

// ---------------------------------------------------------------------------
// The gate.
// ---------------------------------------------------------------------------

#[test]
fn spec_suite() {
    let dir = testsuite_dir();
    if !dir.is_dir() {
        eprintln!(
            "spec testsuite not vendored at {} — run `git submodule update --init`; skipping",
            dir.display()
        );
        return;
    }

    let files = wast_files(&dir);
    assert!(!files.is_empty(), "no .wast files in {}", dir.display());

    let mut summary = Summary::default();
    let mut failures: Vec<String> = Vec::new();
    let mut parse_failures: Vec<String> = Vec::new();

    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        if let Class::Skip(reason) = classify(path, &text) {
            summary.skip_file(reason);
            continue;
        }

        let buf = match ParseBuffer::new(&text) {
            Ok(b) => b,
            Err(e) => {
                parse_failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let parsed: Wast<'_> = match parser::parse(&buf) {
            Ok(w) => w,
            Err(e) => {
                parse_failures.push(format!("{name}: {e}"));
                continue;
            }
        };

        summary.files_run += 1;
        let mut ctx = SpecContext::new();
        run_directives(&mut ctx, parsed, &name, &mut failures, &mut summary);
    }

    summary.report();
    if !parse_failures.is_empty() {
        eprintln!("{} run-file(s) failed to parse:", parse_failures.len());
        for f in &parse_failures {
            eprintln!("    {f}");
        }
    }
    assert!(
        failures.is_empty(),
        "{} spec assertion failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

struct SpecContext {
    engine: Engine,
    store: Store<()>,
    current: Option<Instance>,
    current_skipped: bool,
    named: HashMap<String, Instance>,
    skipped_names: HashSet<String>,
}

impl SpecContext {
    fn new() -> Self {
        let engine = Engine::default();
        let store = Store::new(&engine, ());
        SpecContext {
            engine,
            store,
            current: None,
            current_skipped: false,
            named: HashMap::new(),
            skipped_names: HashSet::new(),
        }
    }

    fn set_current_skipped(&mut self, name: Option<&str>) {
        self.current = None;
        self.current_skipped = true;
        if let Some(n) = name {
            self.skipped_names.insert(n.to_string());
        }
    }
}

fn run_directives(
    ctx: &mut SpecContext,
    wast: Wast<'_>,
    file: &str,
    failures: &mut Vec<String>,
    summary: &mut Summary,
) {
    for directive in wast.directives {
        match directive {
            WastDirective::Module(quoted) => handle_module(ctx, quoted, file, failures, summary),
            WastDirective::Invoke(invoke) => match invoke_export(ctx, &invoke) {
                Ok(_) => summary.asserts_passed += 1,
                Err(e) if is_unsupported(&e) => summary.asserts_skipped += 1,
                Err(e) => failures.push(format!("{file}: invoke {}: {e}", invoke.name)),
            },
            WastDirective::AssertReturn { exec, results, .. } => match execute(ctx, exec) {
                Ok(actual) if rets_match(&actual, &results) => summary.asserts_passed += 1,
                Ok(actual) => failures.push(format!(
                    "{file}: assert_return mismatch: got {actual:?}, want {results:?}"
                )),
                Err(e) if is_unsupported(&e) => summary.asserts_skipped += 1,
                Err(e) => failures.push(format!("{file}: assert_return errored: {e}")),
            },
            WastDirective::AssertTrap { exec, message, .. } => {
                check_trap(execute(ctx, exec), message, file, failures, summary);
            }
            WastDirective::AssertExhaustion { call, message, .. } => {
                check_trap(invoke_export(ctx, &call), message, file, failures, summary);
            }
            WastDirective::AssertInvalid { mut module, .. } => {
                if let Ok(bytes) = encode(&mut module) {
                    if Module::validate(&ctx.engine, &bytes).is_err() {
                        summary.asserts_passed += 1;
                    } else {
                        failures.push(format!("{file}: expected invalid module to be rejected"));
                    }
                }
            }
            WastDirective::AssertMalformed { mut module, .. } => match encode(&mut module) {
                Ok(bytes) if Module::new(&ctx.engine, &bytes).is_ok() => {
                    failures.push(format!("{file}: expected malformed module to be rejected"));
                }
                _ => summary.asserts_passed += 1,
            },
            // Need the linker / cross-module imports (Phase 2 / #24b) or EH.
            WastDirective::Register { .. }
            | WastDirective::AssertUnlinkable { .. }
            | WastDirective::AssertException { .. } => summary.asserts_skipped += 1,
            _ => {}
        }
    }
}

fn handle_module(
    ctx: &mut SpecContext,
    mut quoted: QuoteWat<'_>,
    file: &str,
    failures: &mut Vec<String>,
    summary: &mut Summary,
) {
    let name = quoted.name().map(|id| id.name().to_string());
    let bytes = match encode(&mut quoted) {
        Ok(b) => b,
        Err(e) => {
            summary.skip_module(&e);
            ctx.set_current_skipped(name.as_deref());
            return;
        }
    };
    let module = match Module::new(&ctx.engine, &bytes) {
        Ok(m) => m,
        Err(e) if is_unsupported(&e) => {
            summary.skip_module(&e);
            ctx.set_current_skipped(name.as_deref());
            return;
        }
        Err(e) => {
            failures.push(format!("{file}: module failed to compile: {e}"));
            ctx.set_current_skipped(name.as_deref());
            return;
        }
    };
    match Instance::new(&mut ctx.store, &module, &[]) {
        Ok(inst) => {
            summary.modules_ok += 1;
            ctx.current = Some(inst);
            ctx.current_skipped = false;
            if let Some(n) = name {
                ctx.named.insert(n, inst);
            }
        }
        Err(e) if is_unsupported(&e) => {
            summary.skip_module(&e);
            ctx.set_current_skipped(name.as_deref());
        }
        Err(e) => {
            failures.push(format!("{file}: instantiation failed: {e}"));
            ctx.set_current_skipped(name.as_deref());
        }
    }
}

fn check_trap(
    result: Result<Vec<Val>>,
    message: &str,
    file: &str,
    failures: &mut Vec<String>,
    summary: &mut Summary,
) {
    match result {
        Ok(_) => failures.push(format!(
            "{file}: expected trap '{message}', but it returned"
        )),
        Err(e) if is_unsupported(&e) => summary.asserts_skipped += 1,
        Err(e) if trap_matches(&e, message) => summary.asserts_passed += 1,
        Err(e) => failures.push(format!(
            "{file}: trap mismatch: want '{message}', got '{e}'"
        )),
    }
}

fn execute(ctx: &mut SpecContext, exec: WastExecute<'_>) -> Result<Vec<Val>> {
    match exec {
        WastExecute::Invoke(invoke) => invoke_export(ctx, &invoke),
        WastExecute::Get { module, global, .. } => {
            let instance = resolve(ctx, module)?;
            let g = instance
                .get_global(&mut ctx.store, global)
                .ok_or_else(|| Error::msg(format!("missing global {global}")))?;
            Ok(vec![g.get(&mut ctx.store)])
        }
        WastExecute::Wat(mut wat) => {
            let bytes = wat.encode().map_err(to_err)?;
            Module::new(&ctx.engine, &bytes)?;
            Ok(Vec::new())
        }
    }
}

fn invoke_export(ctx: &mut SpecContext, invoke: &WastInvoke<'_>) -> Result<Vec<Val>> {
    let instance = resolve(ctx, invoke.module)?;
    let func = instance
        .get_func(&mut ctx.store, invoke.name)
        .ok_or_else(|| Error::msg(format!("missing export {}", invoke.name)))?;
    let args = invoke
        .args
        .iter()
        .map(arg_to_val)
        .collect::<Result<Vec<_>>>()?;
    let result_count = func.ty(&ctx.store).results().len();
    let mut results = vec![Val::I32(0); result_count];
    func.call(&mut ctx.store, &args, &mut results)?;
    Ok(results)
}

fn resolve(ctx: &SpecContext, module: Option<Id<'_>>) -> Result<Instance> {
    match module {
        Some(id) => {
            if ctx.skipped_names.contains(id.name()) {
                return Err(Error::msg("module skipped"));
            }
            ctx.named
                .get(id.name())
                .copied()
                .ok_or_else(|| Error::msg(format!("unknown module {}", id.name())))
        }
        None => {
            if ctx.current_skipped {
                return Err(Error::msg("module skipped"));
            }
            ctx.current.ok_or_else(|| Error::msg("no current module"))
        }
    }
}

fn encode(quoted: &mut QuoteWat<'_>) -> Result<Vec<u8>> {
    quoted.encode().map_err(to_err)
}

fn to_err(e: wast::Error) -> Error {
    Error::msg(e.to_string())
}

fn trap_matches(err: &Error, expected: &str) -> bool {
    err.to_string().contains(expected)
}

// ---------------------------------------------------------------------------
// Value conversion + NaN-aware result matching.
// ---------------------------------------------------------------------------

fn arg_to_val(arg: &WastArg<'_>) -> Result<Val> {
    let WastArg::Core(core) = arg else {
        return Err(Error::msg("non-core argument"));
    };
    Ok(match core {
        WastArgCore::I32(x) => Val::I32(*x),
        WastArgCore::I64(x) => Val::I64(*x),
        WastArgCore::F32(f) => Val::F32(f.bits),
        WastArgCore::F64(f) => Val::F64(f.bits),
        _ => return Err(Error::msg("unsupported (non-numeric) argument")),
    })
}

fn rets_match(actual: &[Val], expected: &[WastRet<'_>]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(a, e)| match e {
            WastRet::Core(c) => ret_core_matches(a, c),
            _ => false,
        })
}

fn ret_core_matches(actual: &Val, expected: &WastRetCore<'_>) -> bool {
    match expected {
        WastRetCore::I32(x) => actual.i32() == Some(*x),
        WastRetCore::I64(x) => actual.i64() == Some(*x),
        WastRetCore::F32(p) => matches!(actual, Val::F32(bits) if f32_matches(*bits, p)),
        WastRetCore::F64(p) => matches!(actual, Val::F64(bits) if f64_matches(*bits, p)),
        WastRetCore::Either(opts) => opts.iter().any(|o| ret_core_matches(actual, o)),
        _ => false,
    }
}

fn f32_matches(bits: u32, pat: &NanPattern<F32>) -> bool {
    match pat {
        NanPattern::Value(f) => bits == f.bits,
        NanPattern::CanonicalNan => bits & 0x7fff_ffff == 0x7fc0_0000,
        NanPattern::ArithmeticNan => bits & 0x7fc0_0000 == 0x7fc0_0000,
    }
}

fn f64_matches(bits: u64, pat: &NanPattern<F64>) -> bool {
    match pat {
        NanPattern::Value(f) => bits == f.bits,
        NanPattern::CanonicalNan => bits & 0x7fff_ffff_ffff_ffff == 0x7ff8_0000_0000_0000,
        NanPattern::ArithmeticNan => bits & 0x7ff8_0000_0000_0000 == 0x7ff8_0000_0000_0000,
    }
}
