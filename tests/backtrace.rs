//! #29d: backtraces captured at trap/throw time and attached to the surfaced error, plus
//! `WasmBacktrace::capture` from inside a host function. Frame names come from the `name` section
//! (kept by default when `wasm_backtrace` is on); file/line resolution is covered by the #29a/#29b
//! unit tests against the DWARF fixture.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    Caller, Config, Engine, Extern, Func, Instance, Module, Store, ThrownException, Trap,
    WasmBacktrace, WasmBacktraceDetails,
};

/// A multi-function DWARF module: `trap_chain → level_b → level_c`, where `level_c` does an
/// out-of-bounds load (`MemoryOutOfBounds`). Source lines: level_c=12, level_b=13, trap_chain=14.
const TRAP_CHAIN: &[u8] = include_bytes!("../src/module/debug/testdata/trap_chain.wasm");

/// Instantiates `wat`, calls a no-arg export, and returns the resulting error.
fn call_err(engine: &Engine, wat: &str, export: &str) -> submilli_wasm::Error {
    let mut store = Store::new(engine, ());
    let module = Module::new(engine, wat::parse_str(wat).unwrap()).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let f = inst.get_func(&mut store, export).unwrap();
    f.call(&mut store, &[], &mut []).unwrap_err()
}

/// Instantiates a binary module, calls a no-arg `() -> i32` export, returns the resulting error.
fn call_err_bin(engine: &Engine, bytes: &[u8], export: &str) -> submilli_wasm::Error {
    let mut store = Store::new(engine, ());
    let module = Module::new(engine, bytes).unwrap();
    let inst = Instance::new(&mut store, &module, &[]).unwrap();
    let f = inst.get_func(&mut store, export).unwrap();
    f.call(&mut store, &[], &mut [submilli_wasm::Val::I32(0)])
        .unwrap_err()
}

fn frame_names(bt: &WasmBacktrace) -> Vec<String> {
    bt.frames()
        .iter()
        .map(|f| f.func_name().unwrap_or("?").to_string())
        .collect()
}

/// A trap carries both its `Trap` (source) and a `WasmBacktrace` (context) — the full call chain,
/// most-recent first.
#[test]
fn trap_attaches_backtrace_with_full_chain() {
    let engine = Engine::default();
    let err = call_err(
        &engine,
        r#"(module
            (func $c unreachable)
            (func $b call $c)
            (func $a (export "a") call $b))"#,
        "a",
    );
    assert!(matches!(
        err.downcast_ref::<Trap>(),
        Some(Trap::UnreachableCodeReached)
    ));
    let bt = err
        .downcast_ref::<WasmBacktrace>()
        .expect("backtrace attached");
    assert_eq!(frame_names(bt), ["c", "b", "a"]);
}

/// An exception thrown N frames deep and left uncaught reports all N frames.
#[test]
fn uncaught_exception_reports_all_frames() {
    let engine = Engine::default();
    let err = call_err(
        &engine,
        r#"(module
            (tag $t)
            (func $c throw $t)
            (func $b call $c)
            (func $a (export "a") call $b))"#,
        "a",
    );
    assert!(err.downcast_ref::<ThrownException>().is_some());
    let bt = err
        .downcast_ref::<WasmBacktrace>()
        .expect("exception backtrace");
    assert_eq!(frame_names(bt), ["c", "b", "a"]);
}

/// `throw` (in `$c`) → `catch` (in `$a`) → `throw_ref` (also in `$a`) keeps the *original*
/// throw-site backtrace: the innermost frame is `$c`, not the rethrow site `$a`.
#[test]
fn rethrow_preserves_original_throw_site() {
    let engine = Engine::default();
    let err = call_err(
        &engine,
        r#"(module
            (tag $t)
            (func $c throw $t)
            (func $a (export "a")
              (block $h (result exnref)
                (try_table (catch_ref $t $h) (call $c))
                (return))
              (throw_ref)))"#,
        "a",
    );
    assert!(err.downcast_ref::<ThrownException>().is_some());
    let bt = err.downcast_ref::<WasmBacktrace>().expect("backtrace");
    assert_eq!(
        bt.frames()[0].func_name(),
        Some("c"),
        "innermost frame must be the original throw site, not the rethrow"
    );
}

/// `wasm_backtrace(false)` traps normally but attaches no backtrace.
#[test]
fn disabled_backtrace_traps_without_attaching() {
    let engine = Engine::new(Config::new().wasm_backtrace(false)).unwrap();
    let err = call_err(
        &engine,
        r#"(module (func $a (export "a") unreachable))"#,
        "a",
    );
    assert!(matches!(
        err.downcast_ref::<Trap>(),
        Some(Trap::UnreachableCodeReached)
    ));
    assert!(err.downcast_ref::<WasmBacktrace>().is_none());
}

/// `WasmBacktrace::capture(caller)` from inside a host function walks the wasm frames that called
/// it (the host frame itself is not a wasm frame).
#[test]
fn capture_from_host_walks_wasm_callers() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, 0usize);
    let probe = Func::wrap(&mut store, |mut caller: Caller<'_, usize>| {
        let n = WasmBacktrace::capture(&caller).frames().len();
        *caller.data_mut() = n;
    });
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "probe" (func $probe))
                (func $b call $probe)
                (func $a (export "a") call $b))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(probe)]).unwrap();
    let f = inst.get_func(&mut store, "a").unwrap();
    f.call(&mut store, &[], &mut []).unwrap();
    assert_eq!(*store.data(), 2, "wasm callers $a and $b");
}

/// `capture` honors `wasm_backtrace(false)` (empty), but `force_capture` ignores it.
#[test]
fn force_capture_ignores_disabled_backtrace() {
    let engine = Engine::new(Config::new().wasm_backtrace(false)).unwrap();
    let mut store = Store::new(&engine, (0usize, 0usize));
    let probe = Func::wrap(&mut store, |mut caller: Caller<'_, (usize, usize)>| {
        let cap = WasmBacktrace::capture(&caller).frames().len();
        let forced = WasmBacktrace::force_capture(&caller).frames().len();
        *caller.data_mut() = (cap, forced);
    });
    let module = Module::new(
        &engine,
        wat::parse_str(
            r#"(module
                (import "h" "probe" (func $probe))
                (func $a (export "a") call $probe))"#,
        )
        .unwrap(),
    )
    .unwrap();
    let inst = Instance::new(&mut store, &module, &[Extern::Func(probe)]).unwrap();
    let f = inst.get_func(&mut store, "a").unwrap();
    f.call(&mut store, &[], &mut []).unwrap();
    assert_eq!(
        *store.data(),
        (0, 1),
        "capture empty when disabled; force not"
    );
}

/// Phase-7 gate: a trap in a DWARF module yields a backtrace with correct `file:line` per frame.
/// `trap_chain → level_b → level_c`, `level_c` traps; under `debug_info(true)` each frame resolves
/// to its own source line.
#[test]
fn dwarf_trap_reports_file_line_per_frame() {
    let engine = Engine::new(Config::new().debug_info(true)).unwrap();
    let err = call_err_bin(&engine, TRAP_CHAIN, "trap_chain");
    assert!(matches!(
        err.downcast_ref::<Trap>(),
        Some(Trap::MemoryOutOfBounds)
    ));
    let bt = err.downcast_ref::<WasmBacktrace>().expect("backtrace");

    // Most-recent first: the trapping callee, then its callers (source lines 12/13/14).
    let expected = [("level_c", 12u32), ("level_b", 13), ("trap_chain", 14)];
    assert_eq!(frame_names(bt), ["level_c", "level_b", "trap_chain"]);
    for (frame, (name, line)) in bt.frames().iter().zip(expected) {
        let sym = frame.symbols().first().expect("symbol");
        assert_eq!(sym.name(), Some(name));
        assert!(
            sym.file().unwrap().ends_with("trap_chain.rs"),
            "file: {:?}",
            sym.file()
        );
        assert_eq!(sym.line(), Some(line), "line for {name}");
    }
}

/// Phase-7 gate: `wasm_backtrace_details(Disable)` (with `debug_info` off) keeps the frames — func
/// names + the chain — but drops file/line. (`debug_info(true)` is the override that forces source
/// resolution; with it off, `Disable` means no DWARF retained.)
#[test]
fn details_disabled_keeps_frames_drops_file_line() {
    let engine =
        Engine::new(Config::new().wasm_backtrace_details(WasmBacktraceDetails::Disable)).unwrap();
    let err = call_err_bin(&engine, TRAP_CHAIN, "trap_chain");
    let bt = err.downcast_ref::<WasmBacktrace>().expect("backtrace");
    assert_eq!(frame_names(bt), ["level_c", "level_b", "trap_chain"]);
    for frame in bt.frames() {
        let sym = frame.symbols().first().expect("symbol (name only)");
        assert!(sym.name().is_some(), "func name kept");
        assert_eq!(sym.file(), None, "no DWARF file");
        assert_eq!(sym.line(), None, "no DWARF line");
    }
}
