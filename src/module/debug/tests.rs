// `Module::deserialize` is `unsafe` for wasmtime API parity (no actual unsafe ops here).
#![allow(clippy::unwrap_used, unsafe_code)]

use super::DebugSections;
use crate::{Config, Engine, Module};

/// A real `wasm32-unknown-unknown` module carrying DWARF (`.debug_*`) + a `name` section.
/// Built from `testdata/fixture.rs` (see the header there for the exact command).
const FIXTURE: &[u8] = include_bytes!("testdata/fixture.wasm");

/// An engine that retains DWARF — the default no longer does (#29c gated retention on `debug_info`).
fn debug_engine() -> Engine {
    Engine::new(Config::new().debug_info(true)).unwrap()
}

/// End-to-end through the #29a side-table: a frame's `ip` → `CompiledFunc::offsets[ip]` →
/// `DebugSections::lookup` resolves to the function's source line. `boom` is the only defined
/// function; its body sits on line 13 of the (path-remapped) fixture source.
#[test]
fn dwarf_fixture_resolves_source_line() {
    let engine = debug_engine();
    let module = Module::new(&engine, FIXTURE).unwrap();
    let inner = module.inner();

    let boom = &inner.functions[0];
    let offsets = boom.offsets.as_deref().expect("debug retention is on");
    let entry = inner
        .debug
        .lookup(offsets[0])
        .expect("first op resolves to a source location");
    assert!(
        entry.file.ends_with("fixture.rs"),
        "unexpected file: {}",
        entry.file
    );
    assert_eq!(entry.line, 13, "boom's body is on line 13");
}

/// The `name` custom section symbolicates function indices even without DWARF.
#[test]
fn name_section_resolves_func_name() {
    let engine = Engine::default();
    let module = Module::new(&engine, FIXTURE).unwrap();
    assert_eq!(module.inner().debug.func_name(0), Some("boom"));
}

/// Debug info is part of the compiled artifact: a `serialize`/`deserialize` round-trip preserves
/// both source-line lookup and function names (matching wasmtime's `.cwasm` behavior).
#[test]
fn debug_info_survives_serialize_round_trip() {
    let engine = debug_engine();
    let module = Module::new(&engine, FIXTURE).unwrap();
    let artifact = module.serialize().unwrap();

    let restored = unsafe { Module::deserialize(&engine, &artifact) }.unwrap();
    let inner = restored.inner();
    let offsets = inner.functions[0].offsets.as_deref().expect("offsets kept");
    let entry = inner.debug.lookup(offsets[0]).expect("line still resolves");
    assert!(entry.file.ends_with("fixture.rs"));
    assert_eq!(entry.line, 13);
    assert_eq!(inner.debug.func_name(0), Some("boom"));
}

/// `wat`-built modules with symbolic names also surface through the `name` section (no toolchain).
#[test]
fn name_section_from_wat() {
    let bytes =
        wat::parse_str(r#"(module (func $answer (export "answer") (result i32) i32.const 42))"#)
            .unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes).unwrap();
    assert_eq!(module.inner().debug.func_name(0), Some("answer"));
}

/// Adversarial DWARF must degrade to `None`, never panic (panic = whole-process DoS).
#[test]
fn malformed_dwarf_yields_none() {
    let mut debug = DebugSections::default();
    debug.set_code_base(0);
    debug.add_dwarf_section(".debug_abbrev", &[0x00, 0xff, 0x13]);
    debug.add_dwarf_section(".debug_info", &[0xff; 24]);
    debug.add_dwarf_section(".debug_line", &[0xff; 32]);
    assert!(debug.lookup(0).is_none());
    assert!(debug.lookup(0x1000).is_none());
}

/// A module with no debug info at all resolves nothing but stays well-behaved.
#[test]
fn no_debug_info_is_empty() {
    let bytes = wat::parse_str("(module (func))").unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes).unwrap();
    assert!(module.inner().debug.lookup(0).is_none());
    assert_eq!(module.inner().debug.func_name(0), None);
}

/// #29c default: `wasm_backtrace` on keeps offsets + names, but `debug_info` off drops DWARF —
/// so frames get func names but no file/line (matching wasmtime).
#[test]
fn default_engine_keeps_offsets_and_names_not_dwarf() {
    let engine = Engine::default();
    let module = Module::new(&engine, FIXTURE).unwrap();
    let inner = module.inner();
    let offsets = inner.functions[0]
        .offsets
        .as_deref()
        .expect("offsets kept by default");
    assert_eq!(inner.debug.func_name(0), Some("boom"));
    assert!(
        inner.debug.lookup(offsets[0]).is_none(),
        "no DWARF by default"
    );
}

/// #29c: `wasm_backtrace(false)` drops the offset table and name section entirely.
#[test]
fn wasm_backtrace_off_drops_offsets_and_names() {
    let engine = Engine::new(Config::new().wasm_backtrace(false)).unwrap();
    let module = Module::new(&engine, FIXTURE).unwrap();
    let inner = module.inner();
    assert!(inner.functions[0].offsets.is_none());
    assert_eq!(inner.debug.func_name(0), None);
}
