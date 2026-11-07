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
/// call — pure host↔wasm ping-pong with no wasm `DoCall` to trip the in-loop check. The native
/// stack each crossing uses is charged against `max_wasm_stack`, so it traps `StackOverflow` rather
/// than let the native Rust stack recurse to abort.
#[test]
fn host_pingpong_traps_not_aborts() {
    let (trap, _) = pingpong(&engine_with_stack(64 * 1024), PINGPONG);
    assert_eq!(trap, Trap::StackOverflow);
}

/// At the default budget the ping-pong traps having used no more native stack than
/// `max_wasm_stack` allows (give or take one crossing), so it fits a 2 MiB thread (the
/// spawned-thread default) in every build, however large its frames.
#[test]
fn host_pingpong_at_default_budget_fits_a_2mib_thread() {
    assert_native_bounded(on_2mib_thread(|| pingpong(&Engine::default(), PINGPONG)));
}

/// A guest that re-exports the host import turns a host fn calling that export into pure
/// host→host recursion, with no wasm frame in between — still charged, still a trap.
#[test]
fn host_recursion_through_a_reexported_import_traps() {
    assert_native_bounded(on_2mib_thread(|| pingpong(&Engine::default(), REEXPORT)));
}

const DEFAULT_MAX_WASM_STACK: usize = 512 << 10;

/// One crossing's frames are charged only once the next entry measures them.
const ONE_CROSSING: usize = 64 << 10;

fn assert_native_bounded((trap, native): (Trap, usize)) {
    assert_eq!(trap, Trap::StackOverflow);
    assert!(
        native <= DEFAULT_MAX_WASM_STACK + ONE_CROSSING,
        "the recursion used {native} bytes of native stack"
    );
}

fn on_2mib_thread<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

const PINGPONG: &str = "(module
    (import \"h\" \"ping\" (func $ping))
    (func (export \"pong\") (call $ping)))";

const REEXPORT: &str = "(module
    (import \"h\" \"ping\" (func $ping))
    (export \"pong\" (func $ping)))";

/// The host fn's state: the `pong` export it calls (held, as an embedder would hold a callback —
/// a directly-called host fn's `Caller` has no instance to look exports up in), and the lowest and
/// highest native stack positions it ran at, whose distance is the native stack the recursion used.
#[derive(Default)]
struct PingPong {
    pong: Option<Func>,
    spread: Option<(usize, usize)>,
}

impl PingPong {
    /// Records the current native stack position; returns the `pong` to call next.
    fn see(&mut self) -> Result<Func, Error> {
        let marker = 0u8;
        let at = std::hint::black_box(std::ptr::addr_of!(marker)).addr();
        let (lo, hi) = self.spread.unwrap_or((at, at));
        self.spread = Some((lo.min(at), hi.max(at)));
        self.pong.ok_or_else(|| Error::msg("pong not set"))
    }

    fn native_bytes(&self) -> usize {
        self.spread.map_or(0, |(lo, hi)| hi - lo)
    }
}

/// Calls `pong`, whose host import `ping` calls `pong` again, until it traps; returns the trap and
/// the native stack used.
fn pingpong(engine: &Engine, wat: &str) -> (Trap, usize) {
    let mut store = Store::new(engine, PingPong::default());
    let ping = Func::new(
        &mut store,
        FuncType::new(engine, [], []),
        |mut caller: Caller<'_, PingPong>, _params, _results| {
            let pong = caller.data_mut().see()?;
            pong.call(&mut caller, &[], &mut [])
        },
    );
    let m = module(engine, wat);
    let inst = Instance::new(&mut store, &m, &[Extern::Func(ping)]).unwrap();
    let pong = inst.get_func(&mut store, "pong").unwrap();
    store.data_mut().pong = Some(pong);
    let trap = trap_of(&pong.call(&mut store, &[], &mut []).unwrap_err());
    (trap, store.data().native_bytes())
}

/// Async sibling of the ping-pong guard: exercises the `invoke_host_async` / `drive_async` path.
#[cfg(feature = "async")]
#[test]
fn host_pingpong_async_traps() {
    let (trap, _) = pingpong_async(Some(64 * 1024), PINGPONG);
    assert_eq!(trap, Trap::StackOverflow);
}

/// The async ping-pong at the default budget, on a 2 MiB thread (see the sync sibling).
#[cfg(feature = "async")]
#[test]
fn host_pingpong_async_at_default_budget_fits_a_2mib_thread() {
    assert_native_bounded(on_2mib_thread(|| pingpong_async(None, PINGPONG)));
}

/// Async host→host recursion through a re-exported import (see the sync sibling).
#[cfg(feature = "async")]
#[test]
fn async_host_recursion_through_a_reexported_import_traps() {
    assert_native_bounded(on_2mib_thread(|| pingpong_async(None, REEXPORT)));
}

#[cfg(feature = "async")]
fn pingpong_async(max_wasm_stack: Option<usize>, wat: &str) -> (Trap, usize) {
    use pollster::block_on;

    let mut config = Config::new();
    config.async_support(true);
    if let Some(bytes) = max_wasm_stack {
        config.max_wasm_stack(bytes);
    }
    let engine = Engine::new(&config).unwrap();
    let mut store = Store::new(&engine, PingPong::default());
    let ping = Func::new_async(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller: Caller<'_, PingPong>, _params, _results| {
            Box::new(async move {
                let pong = caller.data_mut().see()?;
                pong.call_async(&mut caller, &[], &mut []).await
            })
        },
    );
    let m = module(&engine, wat);
    let inst = block_on(Instance::new_async(&mut store, &m, &[Extern::Func(ping)])).unwrap();
    let pong = inst.get_func(&mut store, "pong").unwrap();
    store.data_mut().pong = Some(pong);
    let trap = trap_of(&block_on(pong.call_async(&mut store, &[], &mut [])).unwrap_err());
    (trap, store.data().native_bytes())
}

/// A frame's statically-sized operand area is charged against `max_wasm_stack` *before* it is
/// allocated: one call into a function with a 10k-slot operand stack traps under a 64 KiB budget.
#[test]
fn oversized_callee_frame_traps_before_allocation() {
    let engine = engine_with_stack(64 << 10);
    let pushes = "(i32.const 0)".repeat(10_000);
    let drops = "(drop)".repeat(10_000);
    let wat = format!("(module (func $big {pushes} {drops}) (func (export \"run\") (call $big)))");
    let m = module(&engine, &wat);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    assert_eq!(
        trap_of(&call_export(&mut store, inst, "run").unwrap_err()),
        Trap::StackOverflow
    );
}

/// The flip side (wasmtime parity): slots are charged at a value's 8 bytes, not the interpreter's
/// cell width, so a function near wasmparser's 50k-locals limit runs under the default budget.
#[test]
fn many_locals_fit_the_default_budget() {
    let engine = Engine::default();
    let locals = " i32".repeat(49_000);
    let wat = format!("(module (func $f (local{locals})) (func (export \"run\") (call $f)))");
    let m = module(&engine, &wat);
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    call_export(&mut store, inst, "run").unwrap();
}

/// Returns leave callees' slots in the operand buffer for the next call to reuse; a host re-entry
/// is charged for the live stack, not that high-water mark. `deep(170)` uses ~60% of the budget;
/// run once directly and once re-entered from a host call, it must fit both times.
#[test]
fn reentry_is_charged_for_the_live_stack_not_the_high_water() {
    let engine = engine_with_stack(256 << 10);
    let locals = " i64".repeat(100);
    let wat = format!(
        r#"(module
            (import "h" "reenter" (func $reenter))
            (func $deep (export "deep") (param i32) (local{locals})
                (if (local.get 0) (then (call $deep (i32.sub (local.get 0) (i32.const 1))))))
            (func (export "run") (call $deep (i32.const 170)) (call $reenter)))"#
    );
    let m = module(&engine, &wat);
    let mut store = Store::new(&engine, ());
    let reenter = Func::new(
        &mut store,
        FuncType::new(&engine, [], []),
        |mut caller, _, _| {
            let Some(Extern::Func(deep)) = caller.get_export("deep") else {
                panic!("deep is exported");
            };
            deep.call(&mut caller, &[Val::I32(170)], &mut [])
        },
    );
    let inst = Instance::new(&mut store, &m, &[reenter.into()]).unwrap();
    call_export(&mut store, inst, "run").unwrap();
}
