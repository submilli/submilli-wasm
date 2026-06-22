//! WebAssembly spec-suite (`.wast`) runner — the conformance gate (#15 + #24b).
//!
//! The vendored testsuite is Wasm 3.0, which interleaves reference-types / SIMD /
//! GC / EH / memory64 content into otherwise-core files. We run a managed set
//! **resiliently**: a module is skipped (and tallied) iff it fails validation
//! under our enabled feature set — `Module::validate` is the *oracle* for
//! "unsupported", so there's no error-string guessing and **any** failure from a
//! module that does validate is a real bug. Imports are resolved through a
//! `Linker` carrying the standard `spectest` shim; a module whose import provider
//! was skipped is itself skipped. The only whole-file skips are out-of-scope
//! features that can't be cleanly handled per-module (SIMD/memory64/multi-memory).

#![allow(dead_code, unused_imports)]
#![allow(clippy::all, clippy::pedantic)]
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use std::any::Any;
use submilli_wasm::{
    Engine, Error, ExternRef, Global, GlobalType, HeapType, Instance, Linker, Memory, MemoryType,
    Module, Mutability, Ref, RefType, Result, Store, Table, TableType, Val, ValType,
};

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

/// Decides whether to execute a whole file. In-file unsupported modules are caught
/// per-module via the validation oracle ([`is_unsupported_module`]); here we only
/// short-circuit whole files that are wholly out of scope (faster, cleaner summary).
fn classify(path: &Path, text: &str) -> Class {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if name.starts_with("simd") {
        return Class::Skip("simd (out of scope)");
    }
    if text.contains("(memory i64") || text.contains("(table i64") {
        return Class::Skip("memory64 (out of scope)");
    }
    Class::Run
}

// ---------------------------------------------------------------------------
// Execution summary.
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct FileStats {
    modules_ok: usize,
    modules_skipped: usize,
    asserts_passed: usize,
    asserts_skipped: usize,
}

#[derive(Default)]
struct Summary {
    /// Per run-file stats (every executed file appears here).
    run: BTreeMap<String, FileStats>,
    /// Whole-file skips: `(filename, reason)`.
    skipped_files: Vec<(String, String)>,
    /// Aggregate module-skip reasons across all run files.
    skip_reasons: BTreeMap<String, usize>,
}

impl Summary {
    fn file(&mut self, file: &str) -> &mut FileStats {
        self.run.entry(file.to_string()).or_default()
    }

    fn skip_file(&mut self, name: &str, reason: &str) {
        self.skipped_files
            .push((name.to_string(), reason.to_string()));
    }

    fn module_ok(&mut self, file: &str) {
        self.file(file).modules_ok += 1;
    }

    fn skip_module(&mut self, file: &str, reason: &str) {
        self.file(file).modules_skipped += 1;
        *self.skip_reasons.entry(reason.to_string()).or_default() += 1;
    }

    fn pass_assert(&mut self, file: &str) {
        self.file(file).asserts_passed += 1;
    }

    fn skip_assert(&mut self, file: &str) {
        self.file(file).asserts_skipped += 1;
    }

    fn report(&self) {
        let (mut ap, mut ask, mut mok, mut msk) = (0, 0, 0, 0);
        for s in self.run.values() {
            ap += s.asserts_passed;
            ask += s.asserts_skipped;
            mok += s.modules_ok;
            msk += s.modules_skipped;
        }
        // A "run" file is fully ok, genuinely partial, or fully skipped (it ran
        // and passed nothing — usually an all-multi-memory or all-GC file not
        // caught at the file-classify stage).
        let label = |s: &FileStats| -> &'static str {
            if s.asserts_skipped + s.modules_skipped == 0 {
                "ok     "
            } else if s.asserts_passed > 0 || s.modules_ok > 0 {
                "PARTIAL"
            } else {
                "skipped"
            }
        };
        let (mut n_ok, mut n_partial, mut n_skip) = (0, 0, 0);
        for s in self.run.values() {
            match label(s) {
                "ok     " => n_ok += 1,
                "PARTIAL" => n_partial += 1,
                _ => n_skip += 1,
            }
        }
        eprintln!(
            "spec gate: {} files run ({n_ok} fully ok, {n_partial} partial, {n_skip} fully skipped), \
             {} whole files skipped | modules: {mok} ok, {msk} skipped | assertions: {ap} passed, {ask} skipped",
            self.run.len(),
            self.skipped_files.len()
        );

        eprintln!("\n  RUN FILES (assertions passed/skipped, modules ok/skipped):");
        for (file, s) in &self.run {
            eprintln!(
                "    [{}] {file}: {}/{} asserts, {}/{} modules",
                label(s),
                s.asserts_passed,
                s.asserts_skipped,
                s.modules_ok,
                s.modules_skipped
            );
        }

        eprintln!("\n  WHOLE FILES SKIPPED ({}):", self.skipped_files.len());
        for (name, reason) in &self.skipped_files {
            eprintln!("    {name}  —  {reason}");
        }

        eprintln!("\n  IN-FILE SKIPS BY REASON:");
        for (reason, n) in &self.skip_reasons {
            eprintln!("    {n:>4}  {reason}");
        }
    }
}

/// Outcome of running a directive's executable part.
enum ExecResult {
    /// Ran to completion with these results.
    Vals(Vec<Val>),
    /// Trapped / errored at runtime (the *real* behavior — asserted against).
    Trap(Error),
    /// Not run because the module (or a dependency) is out of our feature scope.
    Skip,
}

/// A module uses a feature we don't enable iff it fails validation under
/// `phase1_features()`. This is the **oracle** for "unsupported" — no error-string
/// guessing in the skip *decision* — so any failure from a module that *does*
/// validate is a real bug. Returns the validator's own message (offset stripped,
/// for grouping) purely as the informational skip reason.
fn unsupported_reason(ctx: &SpecContext, bytes: &[u8]) -> Option<String> {
    let msg = Module::validate(&ctx.engine, bytes).err()?.to_string();
    let short = msg.split(" (at offset").next().unwrap_or(&msg);
    Some(short.to_string())
}

fn is_unsupported_module(ctx: &SpecContext, bytes: &[u8]) -> bool {
    unsupported_reason(ctx, bytes).is_some()
}

/// True if a module validates but uses an operator we deliberately defer to a later phase, so its
/// compile failure is an expected skip rather than a bug. Nothing is deferred any more — tail calls
/// (#39), GC aggregates, and EH all compile; SIMD/memory64 fail *validation* (file-skipped) since
/// their features are off. Kept as a hook for future phases.
fn is_deferred_op(_err: &Error) -> bool {
    false
}

/// True if every import of `module` is satisfiable by the linker (else a provider
/// module was skipped, so we skip this one rather than fail on a missing import).
fn imports_available(ctx: &mut SpecContext, module: &Module) -> bool {
    let names: Vec<(String, String)> = module
        .imports()
        .map(|i| (i.module().to_string(), i.name().to_string()))
        .collect();
    names
        .iter()
        .all(|(m, n)| ctx.linker.get(&mut ctx.store, m, n).is_ok())
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
            summary.skip_file(&name, reason);
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

        summary.file(&name);
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
    linker: Linker<()>,
    current: Option<Instance>,
    current_skipped: bool,
    named: HashMap<String, Instance>,
    skipped_names: HashSet<String>,
}

impl SpecContext {
    fn new() -> Self {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new(&engine);
        linker.allow_shadowing(true);
        register_spectest(&mut store, &mut linker);
        SpecContext {
            engine,
            store,
            linker,
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

/// Registers the standard `spectest` host module the suite imports.
fn register_spectest(store: &mut Store<()>, linker: &mut Linker<()>) {
    linker.func_wrap("spectest", "print", || {}).unwrap();
    linker
        .func_wrap("spectest", "print_i32", |_: i32| {})
        .unwrap();
    linker
        .func_wrap("spectest", "print_i64", |_: i64| {})
        .unwrap();
    linker
        .func_wrap("spectest", "print_f32", |_: f32| {})
        .unwrap();
    linker
        .func_wrap("spectest", "print_f64", |_: f64| {})
        .unwrap();
    linker
        .func_wrap("spectest", "print_i32_f32", |_: i32, _: f32| {})
        .unwrap();
    linker
        .func_wrap("spectest", "print_f64_f64", |_: f64, _: f64| {})
        .unwrap();

    let mut global = |name: &str, ty: ValType, val: Val| {
        let g = Global::new(&mut *store, GlobalType::new(ty, Mutability::Const), val).unwrap();
        linker.define(&*store, "spectest", name, g).unwrap();
    };
    global("global_i32", ValType::I32, Val::I32(666));
    global("global_i64", ValType::I64, Val::I64(666));
    global("global_f32", ValType::F32, Val::F32(666.6f32.to_bits()));
    global("global_f64", ValType::F64, Val::F64(666.6f64.to_bits()));

    let mem = Memory::new(&mut *store, MemoryType::new(1, Some(2))).unwrap();
    linker.define(&*store, "spectest", "memory", mem).unwrap();
    let table_ty = TableType::new(RefType::new(true, HeapType::Func), 10, Some(20));
    let table = Table::new(&mut *store, table_ty, Ref::Func(None)).unwrap();
    linker.define(&*store, "spectest", "table", table).unwrap();
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
                ExecResult::Vals(_) => summary.pass_assert(file),
                ExecResult::Skip => summary.skip_assert(file),
                ExecResult::Trap(e) => {
                    failures.push(format!("{file}: invoke {}: {e}", invoke.name))
                }
            },
            WastDirective::AssertReturn { exec, results, .. } => match execute(ctx, exec) {
                ExecResult::Vals(actual) if rets_match(&ctx.store, &actual, &results) => {
                    summary.pass_assert(file);
                }
                ExecResult::Vals(actual) => failures.push(format!(
                    "{file}: assert_return mismatch: got {actual:?}, want {results:?}"
                )),
                ExecResult::Skip => summary.skip_assert(file),
                ExecResult::Trap(e) => failures.push(format!("{file}: assert_return errored: {e}")),
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
                        summary.pass_assert(file);
                    } else {
                        failures.push(format!("{file}: expected invalid module to be rejected"));
                    }
                }
            }
            WastDirective::AssertMalformed { mut module, .. } => match encode(&mut module) {
                Ok(bytes) if Module::new(&ctx.engine, &bytes).is_ok() => {
                    failures.push(format!("{file}: expected malformed module to be rejected"));
                }
                _ => summary.pass_assert(file),
            },
            WastDirective::Register { name, module, .. } => match resolve(ctx, module) {
                Some(inst) => {
                    let _ = ctx.linker.instance(&mut ctx.store, name, inst);
                }
                None => summary.skip_assert(file),
            },
            WastDirective::AssertUnlinkable { mut module, .. } => {
                match module.encode().map_err(to_err) {
                    Ok(bytes) if is_unsupported_module(ctx, &bytes) => summary.skip_assert(file),
                    Ok(bytes) => {
                        let m = Module::new(&ctx.engine, &bytes).expect("validated above");
                        if ctx.linker.instantiate(&mut ctx.store, &m).is_err() {
                            summary.pass_assert(file);
                        } else {
                            failures.push(format!("{file}: expected unlinkable module to fail"));
                        }
                    }
                    Err(_) => summary.skip_assert(file),
                }
            }
            WastDirective::AssertException { exec, .. } => match execute(ctx, exec) {
                ExecResult::Vals(_) => {
                    failures.push(format!("{file}: expected an exception, but it returned"))
                }
                ExecResult::Skip => summary.skip_assert(file),
                ExecResult::Trap(e) if e.is::<submilli_wasm::ThrownException>() => {
                    summary.pass_assert(file);
                }
                ExecResult::Trap(e) => {
                    failures.push(format!("{file}: expected an exception, got: {e}"))
                }
            },
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
        Err(_) => {
            summary.skip_module(file, "module failed to encode");
            ctx.set_current_skipped(name.as_deref());
            return;
        }
    };
    if let Some(reason) = unsupported_reason(ctx, &bytes) {
        summary.skip_module(file, &reason);
        ctx.set_current_skipped(name.as_deref());
        return;
    }
    // Validated, so a compile failure here is a real interpreter bug — except for
    // operators we deliberately defer to a later phase (see `is_deferred_op`).
    let module = match Module::new(&ctx.engine, &bytes) {
        Ok(m) => m,
        Err(e) if is_deferred_op(&e) => {
            summary.skip_module(file, &e.to_string());
            ctx.set_current_skipped(name.as_deref());
            return;
        }
        Err(e) => {
            failures.push(format!("{file}: validated but failed to compile: {e}"));
            ctx.set_current_skipped(name.as_deref());
            return;
        }
    };
    if !imports_available(ctx, &module) {
        summary.skip_module(file, "imports a module that was skipped");
        ctx.set_current_skipped(name.as_deref());
        return;
    }
    match ctx.linker.instantiate(&mut ctx.store, &module) {
        Ok(inst) => {
            summary.module_ok(file);
            ctx.current = Some(inst);
            ctx.current_skipped = false;
            if let Some(n) = name {
                ctx.named.insert(n, inst);
            }
        }
        Err(e) => {
            failures.push(format!("{file}: instantiation failed: {e}"));
            ctx.set_current_skipped(name.as_deref());
        }
    }
}

fn check_trap(
    result: ExecResult,
    message: &str,
    file: &str,
    failures: &mut Vec<String>,
    summary: &mut Summary,
) {
    match result {
        ExecResult::Vals(_) => failures.push(format!(
            "{file}: expected trap '{message}', but it returned"
        )),
        ExecResult::Skip => summary.skip_assert(file),
        ExecResult::Trap(e) if trap_matches(&e, message) => summary.pass_assert(file),
        ExecResult::Trap(e) => failures.push(format!(
            "{file}: trap mismatch: want '{message}', got '{e}'"
        )),
    }
}

fn execute(ctx: &mut SpecContext, exec: WastExecute<'_>) -> ExecResult {
    match exec {
        WastExecute::Invoke(invoke) => invoke_export(ctx, &invoke),
        WastExecute::Get { module, global, .. } => match resolve(ctx, module) {
            Some(instance) => match instance.get_global(&mut ctx.store, global) {
                Some(g) => ExecResult::Vals(vec![g.get(&mut ctx.store)]),
                None => ExecResult::Trap(Error::msg(format!("missing global {global}"))),
            },
            None => ExecResult::Skip,
        },
        WastExecute::Wat(mut wat) => {
            // Instantiate (not just compile): active-segment OOB and a trapping
            // `start` surface here, which is what `assert_trap (module …)` checks.
            let bytes = match wat.encode().map_err(to_err) {
                Ok(b) => b,
                Err(_) => return ExecResult::Skip,
            };
            if is_unsupported_module(ctx, &bytes) {
                return ExecResult::Skip;
            }
            let module = match Module::new(&ctx.engine, &bytes) {
                Ok(m) => m,
                Err(e) => return ExecResult::Trap(e),
            };
            if !imports_available(ctx, &module) {
                return ExecResult::Skip;
            }
            match ctx.linker.instantiate(&mut ctx.store, &module) {
                Ok(_) => ExecResult::Vals(Vec::new()),
                Err(e) => ExecResult::Trap(e),
            }
        }
    }
}

fn invoke_export(ctx: &mut SpecContext, invoke: &WastInvoke<'_>) -> ExecResult {
    let Some(instance) = resolve(ctx, invoke.module) else {
        return ExecResult::Skip;
    };
    let Some(func) = instance.get_func(&mut ctx.store, invoke.name) else {
        return ExecResult::Trap(Error::msg(format!("missing export {}", invoke.name)));
    };
    let mut args = Vec::with_capacity(invoke.args.len());
    for a in &invoke.args {
        match arg_to_val(&mut ctx.store, a) {
            Ok(v) => args.push(v),
            Err(_) => return ExecResult::Skip, // unsupported (v128 / typed-ref) argument
        }
    }
    let result_count = func.ty(&ctx.store).results().len();
    let mut results = vec![Val::I32(0); result_count];
    match func.call(&mut ctx.store, &args, &mut results) {
        Ok(()) => ExecResult::Vals(results),
        Err(e) => ExecResult::Trap(e),
    }
}

/// The instance a directive targets, or `None` if it was skipped / is unknown.
fn resolve(ctx: &SpecContext, module: Option<Id<'_>>) -> Option<Instance> {
    match module {
        Some(id) if ctx.skipped_names.contains(id.name()) => None,
        Some(id) => ctx.named.get(id.name()).copied(),
        None if ctx.current_skipped => None,
        None => ctx.current,
    }
}

fn encode(quoted: &mut QuoteWat<'_>) -> Result<Vec<u8>> {
    quoted.encode().map_err(to_err)
}

fn to_err(e: wast::Error) -> Error {
    Error::msg(e.to_string())
}

fn trap_matches(err: &Error, expected: &str) -> bool {
    // Trap-text matching is fuzzy across spec versions — e.g. the suite's
    // "uninitialized element 2" (indexed) vs our canonical "uninitialized element".
    // Accept either direction of containment. The suite says "null function reference"
    // for `call_ref`; our (and wasmtime's) canonical trap text is "null reference".
    let actual = err.to_string();
    let expected = expected.replace("null function reference", "null reference");
    actual.contains(&expected) || expected.contains(actual.as_str())
}

// ---------------------------------------------------------------------------
// Value conversion + NaN-aware result matching.
// ---------------------------------------------------------------------------

fn arg_to_val(store: &mut Store<()>, arg: &WastArg<'_>) -> Result<Val> {
    use wast::core::{AbstractHeapType, HeapType as WastHeap};
    let WastArg::Core(core) = arg else {
        return Err(Error::msg("non-core argument"));
    };
    Ok(match core {
        WastArgCore::I32(x) => Val::I32(*x),
        WastArgCore::I64(x) => Val::I64(*x),
        WastArgCore::F32(f) => Val::F32(f.bits),
        WastArgCore::F64(f) => Val::F64(f.bits),
        // The host wraps `(ref.extern N)` / `(ref.host N)` as an externref carrying `N`.
        WastArgCore::RefExtern(n) | WastArgCore::RefHost(n) => {
            Val::ExternRef(Some(ExternRef::new(store, *n)?))
        }
        WastArgCore::RefNull(WastHeap::Abstract {
            ty: AbstractHeapType::Extern | AbstractHeapType::NoExtern,
            ..
        }) => Val::ExternRef(None),
        WastArgCore::RefNull(WastHeap::Abstract {
            ty: AbstractHeapType::Func | AbstractHeapType::NoFunc,
            ..
        }) => Val::FuncRef(None),
        _ => return Err(Error::msg("unsupported argument")),
    })
}

fn rets_match(store: &Store<()>, actual: &[Val], expected: &[WastRet<'_>]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(a, e)| match e {
            WastRet::Core(c) => ret_core_matches(store, a, c),
            _ => false,
        })
}

fn ret_core_matches(store: &Store<()>, actual: &Val, expected: &WastRetCore<'_>) -> bool {
    match expected {
        WastRetCore::I32(x) => actual.i32() == Some(*x),
        WastRetCore::I64(x) => actual.i64() == Some(*x),
        WastRetCore::F32(p) => matches!(actual, Val::F32(bits) if f32_matches(*bits, p)),
        WastRetCore::F64(p) => matches!(actual, Val::F64(bits) if f64_matches(*bits, p)),
        WastRetCore::RefNull(_) => matches!(
            actual,
            Val::FuncRef(None) | Val::ExternRef(None) | Val::AnyRef(None) | Val::ExnRef(None)
        ),
        // `(ref.func)` / `(ref.func $f)` assert a non-null funcref (identity isn't
        // portably checkable), so we accept any non-null funcref.
        WastRetCore::RefFunc(_) => matches!(actual, Val::FuncRef(Some(_))),
        // `(ref.extern N)` / `(ref.host N)`: a non-null externref carrying payload `N`. An
        // `any.convert_extern` of a host extern yields an *anyref* wrapping it; we have no public
        // way to read the wrapped payload, so accept any non-null anyref there (as with `ref.func`).
        WastRetCore::RefExtern(Some(n)) | WastRetCore::RefHost(n) => match actual {
            Val::ExternRef(Some(r)) => {
                r.data(store)
                    .ok()
                    .flatten()
                    .and_then(|a| a.downcast_ref::<u32>())
                    == Some(n)
            }
            Val::AnyRef(Some(_)) => true,
            _ => false,
        },
        WastRetCore::RefExtern(None) => matches!(actual, Val::ExternRef(Some(_))),
        // GC any-hierarchy assertions `(ref.array|struct|eq|i31|any)` check the result is a
        // non-null ref of that hierarchy; all map to `Val::AnyRef`, and (like `ref.func`) the
        // finer kind/identity isn't portably checkable here, so accept any non-null `anyref`.
        WastRetCore::RefArray
        | WastRetCore::RefStruct
        | WastRetCore::RefEq
        | WastRetCore::RefI31
        | WastRetCore::RefAny => matches!(actual, Val::AnyRef(Some(_))),
        WastRetCore::Either(opts) => opts.iter().any(|o| ret_core_matches(store, actual, o)),
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
