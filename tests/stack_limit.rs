//! #30: stack-size limit (`max_wasm_stack`) hardening gate. A hostile guest that recurses
//! unboundedly must trap `Trap::StackOverflow` cleanly — never a panic or native-stack abort. These
//! tests cover the guest-reachable surface: direct/mutual/indirect recursion, the knob actually
//! changing the threshold, enforcement during instantiation (`start`), clean recovery, the
//! tail-call exemption, and — the cross-boundary case — pure host↔wasm ping-pong trapping rather
//! than aborting the native stack. Re-entry *isolation* (operand/exception/backtrace) lives in
//! `tests/reentry.rs`; its Case 5 covers wasm-recursion-after-re-entry.

#![allow(clippy::unwrap_used)]

use submilli_wasm::{
    Caller, Config, Engine, Error, Extern, Func, FuncType, Instance, Module, Store, Trap, Val,
};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

fn engine_with_stack(bytes: usize) -> Engine {
    Engine::new(Config::new().max_wasm_stack(bytes)).unwrap()
}

fn trap_of(err: &Error) -> Trap {
    *err.downcast_ref::<Trap>()
        .unwrap_or_else(|| panic!("expected a Trap, got: {err}"))
}

fn call_export(store: &mut Store<()>, inst: Instance, name: &str) -> Result<(), Error> {
    let f = inst.get_func(&mut *store, name).unwrap();
    f.call(store, &[], &mut [])
}

/// Direct self-recursion exhausts the stack and traps (the default-config baseline).
#[test]
fn unbounded_recursion_traps_stack_overflow() {
    let engine = Engine::default();
    let m = module(&engine, "(module (func (export \"run\") (call 0)))");
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        trap_of(&call_export(&mut store, inst, "run").unwrap_err()),
        Trap::StackOverflow
    );
}

/// Mutual recursion (two frames per cycle) also traps — mirrors `call.wast`'s `mutual-runaway`.
#[test]
fn mutual_recursion_traps_stack_overflow() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (func $a (export \"run\") (call $b))
            (func $b (call $a)))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        trap_of(&call_export(&mut store, inst, "run").unwrap_err()),
        Trap::StackOverflow
    );
}

/// Recursion through `call_indirect` enforces the limit too (it shares the `DoCall` check path).
#[test]
fn call_indirect_recursion_traps_stack_overflow() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (type $t (func))
            (table 1 funcref)
            (func $f (type $t) (call_indirect (type $t) (i32.const 0)))
            (elem (i32.const 0) $f)
            (func (export \"run\") (call_indirect (type $t) (i32.const 0))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        trap_of(&call_export(&mut store, inst, "run").unwrap_err()),
        Trap::StackOverflow
    );
}

/// A `(global $d)` counts each frame entered; read it back after the trap to learn the recursion
/// depth reached, so we can assert it *scales* with the configured budget (the knob is honored,
/// not merely that overflow happens).
const DEPTH_MOD: &str = "(module
    (global $d (mut i32) (i32.const 0))
    (func $f (global.set $d (i32.add (global.get $d) (i32.const 1))) (call $f))
    (func (export \"run\") (call $f))
    (func (export \"depth\") (result i32) (global.get $d)))";

fn depth_reached(stack_bytes: usize) -> i32 {
    let engine = engine_with_stack(stack_bytes);
    let m = module(&engine, DEPTH_MOD);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        trap_of(&call_export(&mut store, inst, "run").unwrap_err()),
        Trap::StackOverflow
    );
    let depth = inst.get_func(&mut store, "depth").unwrap();
    let mut out = [Val::I32(0)];
    depth.call(&mut store, &[], &mut out).unwrap();
    out[0].unwrap_i32()
}

#[test]
fn custom_max_wasm_stack_is_honored() {
    let small = depth_reached(16 * 1024);
    let large = depth_reached(256 * 1024);
    assert!(
        small > 0 && large > small,
        "depth should scale with the budget: small={small} large={large}"
    );
}

/// Guest code runs during `Instance::new` (the `start` fn), so the stack limit must already be
/// armed there — a recursing `start` aborts instantiation with `StackOverflow`, and the store
/// remains usable for a subsequent (benign) instantiation.
#[test]
fn stack_overflow_in_start_function_aborts_instantiation() {
    let engine = Engine::default();
    let m = module(&engine, "(module (func $f (call $f)) (start $f))");
    let mut store = Store::new(&engine, ());
    assert_eq!(
        trap_of(&Instance::new(&mut store, &m, &[]).unwrap_err()),
        Trap::StackOverflow
    );

    let benign = module(
        &engine,
        "(module (func (export \"id\") (result i32) (i32.const 7)))",
    );
    let inst = Instance::new(&mut store, &benign, &[]).unwrap();
    let id = inst.get_func(&mut store, "id").unwrap();
    let mut out = [Val::I32(0)];
    id.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
}

/// A `StackOverflow` trap is clean: the store is not poisoned and another export still runs.
#[test]
fn store_usable_after_stack_overflow() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (func $f (call $f))
            (func (export \"boom\") (call $f))
            (func (export \"ok\") (result i32) (i32.const 7)))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        trap_of(&call_export(&mut store, inst, "boom").unwrap_err()),
        Trap::StackOverflow
    );
    let ok = inst.get_func(&mut store, "ok").unwrap();
    let mut out = [Val::I32(0)];
    ok.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
}

/// Tail calls reuse the frame (stack-neutral), so they are exempt: a 1,000,000-deep `return_call`
/// completes even under a tiny `max_wasm_stack` that traps after a few hundred normal calls.
#[test]
fn tail_calls_not_counted_against_limit() {
    let engine = engine_with_stack(16 * 1024);
    let m = module(
        &engine,
        "(module (func $count (export \"count\") (param i32) (result i32)
            (if (result i32) (i32.eqz (local.get 0))
                (then (i32.const 0))
                (else (return_call $count (i32.sub (local.get 0) (i32.const 1)))))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let count = inst.get_func(&mut store, "count").unwrap();
    let mut out = [Val::I32(-1)];
    count
        .call(&mut store, &[Val::I32(1_000_000)], &mut out)
        .unwrap();
    assert_eq!(out[0].unwrap_i32(), 0);
}

/// The cross-boundary gap closure: a host fn that re-enters wasm whose *only* op is another host
/// call — pure host↔wasm ping-pong with no wasm `DoCall` to trip the in-loop check. The
/// per-crossing reserve folded into `max_wasm_stack` must trap it `StackOverflow` rather than let
/// the native Rust stack recurse to abort.
#[test]
fn host_pingpong_traps_not_aborts() {
    let engine = engine_with_stack(64 * 1024);
    let mut store = Store::new(&engine, ());
    let ping = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller: Caller<'_, ()>, _params, _results| {
            let Some(Extern::Func(pong)) = caller.get_export("pong") else {
                return Err(Error::msg("missing export pong"));
            };
            pong.call(&mut caller, &[], &mut [])?;
            Ok(())
        },
    );
    let m = module(
        &engine,
        "(module
            (import \"h\" \"ping\" (func $ping))
            (func (export \"pong\") (call $ping)))",
    );
    let inst = Instance::new(&mut store, &m, &[Extern::Func(ping)]).unwrap();
    assert_eq!(
        trap_of(&call_export(&mut store, inst, "pong").unwrap_err()),
        Trap::StackOverflow
    );
}

/// Async sibling of the ping-pong guard: exercises the `invoke_host_async` / `drive_async` path.
#[cfg(feature = "async")]
#[test]
fn host_pingpong_async_traps() {
    use pollster::block_on;

    let mut config = Config::new();
    config.async_support(true);
    config.max_wasm_stack(64 * 1024);
    let engine = Engine::new(&config).unwrap();
    let mut store = Store::new(&engine, ());
    let ping = Func::new_async(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller: Caller<'_, ()>, _params, _results| {
            Box::new(async move {
                let Some(Extern::Func(pong)) = caller.get_export("pong") else {
                    return Err(Error::msg("missing export pong"));
                };
                pong.call_async(&mut caller, &[], &mut []).await?;
                Ok(())
            })
        },
    );
    let m = module(
        &engine,
        "(module
            (import \"h\" \"ping\" (func $ping))
            (func (export \"pong\") (call $ping)))",
    );
    let inst = block_on(Instance::new_async(&mut store, &m, &[Extern::Func(ping)])).unwrap();
    let pong = inst.get_func(&mut store, "pong").unwrap();
    let err = block_on(pong.call_async(&mut store, &[], &mut [])).unwrap_err();
    assert_eq!(trap_of(&err), Trap::StackOverflow);
}
