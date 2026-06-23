//! Phase-2 gate: public-API integration tests. Drives the embedder surface
//! (host functions, `Linker`, typed calls, fuel, epoch, limits) through the
//! crate exactly as a wasmtime user would, asserting behavior end-to-end.

#![allow(clippy::unwrap_used, clippy::float_cmp)]

use submilli_wasm::{
    Caller, Config, Engine, Extern, ExternRef, Func, FuncType, Global, GlobalType, Instance,
    Linker, Memory, MemoryType, Module, Mutability, Store, StoreLimits, StoreLimitsBuilder, Trap,
    TypedFunc, UpdateDeadline, Val, ValType, WasmBacktrace,
};

fn module(engine: &Engine, wat: &str) -> Module {
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

fn ft(engine: &Engine, params: &[ValType], results: &[ValType]) -> FuncType {
    FuncType::new(engine, params.iter().cloned(), results.iter().cloned())
}

// --- host functions -------------------------------------------------------

#[test]
fn typed_host_fn_called_from_wasm() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let add = Func::wrap(&mut store, |a: i32, b: i32| a + b);
    let m = module(
        &engine,
        "(module
            (import \"h\" \"add\" (func (param i32 i32) (result i32)))
            (func (export \"run\") (param i32 i32) (result i32)
                local.get 0 local.get 1 call 0))",
    );
    let inst = Instance::new(&mut store, &m, &[Extern::Func(add)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    run.call(&mut store, &[Val::I32(40), Val::I32(2)], &mut out)
        .unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

#[test]
fn caller_aware_host_fn_mutates_store_data() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, 0i32);
    let bump = Func::wrap(&mut store, |mut caller: Caller<'_, i32>, x: i32| {
        *caller.data_mut() += x;
    });
    let m = module(
        &engine,
        "(module
            (import \"h\" \"bump\" (func (param i32)))
            (func (export \"run\") (param i32) local.get 0 call 0))",
    );
    let inst = Instance::new(&mut store, &m, &[Extern::Func(bump)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    run.call(&mut store, &[Val::I32(5)], &mut []).unwrap();
    run.call(&mut store, &[Val::I32(37)], &mut []).unwrap();
    assert_eq!(*store.data(), 42);
}

#[test]
fn untyped_host_fn_reads_guest_memory_via_caller() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    // sum_two(addr): read two bytes from the caller's exported memory and add them.
    let sum_two = Func::new(
        &mut store,
        ft(&engine, &[ValType::I32], &[ValType::I32]),
        |mut caller: Caller<'_, ()>, params, results| {
            let addr = params[0].unwrap_i32() as usize;
            let Some(Extern::Memory(mem)) = caller.get_export("mem") else {
                return Err(submilli_wasm::Error::msg("no memory"));
            };
            let data = mem.data(&caller);
            results[0] = Val::I32(i32::from(data[addr]) + i32::from(data[addr + 1]));
            Ok(())
        },
    );
    let m = module(
        &engine,
        "(module
            (import \"h\" \"sum_two\" (func (param i32) (result i32)))
            (memory (export \"mem\") 1)
            (data (i32.const 8) \"\\05\\25\")
            (func (export \"run\") (param i32) (result i32) local.get 0 call 0))",
    );
    let inst = Instance::new(&mut store, &m, &[Extern::Func(sum_two)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    run.call(&mut store, &[Val::I32(8)], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 0x05 + 0x25);
}

#[test]
fn host_err_propagates_as_trap() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let boom = Func::wrap(&mut store, || -> submilli_wasm::Result<()> {
        Err(Trap::UnreachableCodeReached.into())
    });
    let m = module(
        &engine,
        "(module (import \"h\" \"boom\" (func)) (func (export \"run\") call 0))",
    );
    let inst = Instance::new(&mut store, &m, &[Extern::Func(boom)]).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let err = run.call(&mut store, &[], &mut []).unwrap_err();
    assert_eq!(
        *err.downcast_ref::<Trap>().unwrap(),
        Trap::UnreachableCodeReached
    );
}

// --- linker ---------------------------------------------------------------

#[test]
fn linker_func_wrap_and_define() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let g = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(100),
    )
    .unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker
        .func_wrap("env", "add", |a: i32, b: i32| a + b)
        .unwrap();
    linker.define(&store, "env", "base", g).unwrap();
    let m = module(
        &engine,
        "(module
            (import \"env\" \"add\" (func $add (param i32 i32) (result i32)))
            (import \"env\" \"base\" (global $base i32))
            (func (export \"run\") (param i32) (result i32)
                global.get $base local.get 0 call $add))",
    );
    let inst = linker.instantiate(&mut store, &m).unwrap();
    let run = inst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    run.call(&mut store, &[Val::I32(23)], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 123);
}

#[test]
fn linker_multi_module() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let provider = module(
        &engine,
        "(module (func (export \"forty_two\") (result i32) i32.const 42))",
    );
    let pinst = Instance::new(&mut store, &provider, &[]).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);
    linker.instance(&mut store, "lib", pinst).unwrap();
    let consumer = module(
        &engine,
        "(module
            (import \"lib\" \"forty_two\" (func $f (result i32)))
            (func (export \"run\") (result i32) call $f))",
    );
    let cinst = linker.instantiate(&mut store, &consumer).unwrap();
    let run = cinst.get_func(&mut store, "run").unwrap();
    let mut out = [Val::I32(0)];
    run.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 42);
}

// --- typed calls ----------------------------------------------------------

#[test]
fn typed_call_agrees_with_untyped() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let m = module(
        &engine,
        "(module (func (export \"mul\") (param i32 i32) (result i32)
            local.get 0 local.get 1 i32.mul))",
    );
    let inst = Instance::new(&mut store, &m, &[]).unwrap();

    let typed: TypedFunc<(i32, i32), i32> = inst.get_typed_func(&mut store, "mul").unwrap();
    let typed_res = typed.call(&mut store, (6, 7)).unwrap();

    let f = inst.get_func(&mut store, "mul").unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut store, &[Val::I32(6), Val::I32(7)], &mut out)
        .unwrap();

    assert_eq!(typed_res, 42);
    assert_eq!(typed_res, out[0].unwrap_i32());
}

// --- fuel -----------------------------------------------------------------

const COUNTER: &str = "(module (func (export \"count\") (param i32) (result i32)
    (local i32)
    (block $b (loop $l
        local.get 0 i32.eqz br_if $b
        local.get 0 i32.const 1 i32.sub local.set 0
        local.get 1 i32.const 1 i32.add local.set 1
        br $l))
    local.get 1))";

fn fuel_store() -> (Engine, Store<()>) {
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let store = Store::new(&engine, ());
    (engine, store)
}

#[test]
fn fuel_is_deterministic_and_traps() {
    let (engine, _) = fuel_store();
    let m = module(&engine, COUNTER);

    let run = |fuel: u64| -> submilli_wasm::Result<(i32, u64)> {
        let mut store = Store::new(&engine, ());
        store.set_fuel(fuel).unwrap();
        let inst = Instance::new(&mut store, &m, &[]).unwrap();
        let f = inst.get_func(&mut store, "count").unwrap();
        let mut out = [Val::I32(0)];
        f.call(&mut store, &[Val::I32(20)], &mut out)?;
        Ok((out[0].unwrap_i32(), store.get_fuel().unwrap()))
    };

    let (a_val, a_fuel) = run(1_000_000).unwrap();
    let (b_val, b_fuel) = run(1_000_000).unwrap();
    assert_eq!(a_val, 20);
    assert_eq!((a_val, a_fuel), (b_val, b_fuel)); // deterministic
    assert!(a_fuel < 1_000_000); // fuel was consumed

    // Too little fuel for 1e6 iterations → trap.
    let mut store = Store::new(&engine, ());
    store.set_fuel(50).unwrap();
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "count").unwrap();
    let err = f
        .call(&mut store, &[Val::I32(1_000_000)], &mut [Val::I32(0)])
        .unwrap_err();
    assert_eq!(*err.downcast_ref::<Trap>().unwrap(), Trap::OutOfFuel);
}

// --- epoch ----------------------------------------------------------------

fn epoch_engine() -> Engine {
    let mut config = Config::new();
    config.epoch_interruption(true);
    Engine::new(&config).unwrap()
}

const SEVEN: &str = "(module (func (export \"f\") (result i32) i32.const 7))";

#[test]
fn epoch_deadline_traps() {
    let engine = epoch_engine();
    let m = module(&engine, SEVEN);
    let mut store = Store::new(&engine, ());
    store.set_epoch_deadline(0); // already at the deadline
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "f").unwrap();
    let err = f.call(&mut store, &[], &mut [Val::I32(0)]).unwrap_err();
    assert_eq!(*err.downcast_ref::<Trap>().unwrap(), Trap::Interrupt);
    // Like every other trap, an epoch interrupt carries a captured backtrace (wasmtime parity):
    // embedders downcast it to render the trap site.
    assert!(err.downcast_ref::<WasmBacktrace>().is_some());
}

#[test]
fn epoch_callback_continue_resumes() {
    let engine = epoch_engine();
    let m = module(&engine, SEVEN);
    let mut store = Store::new(&engine, ());
    store.epoch_deadline_callback(|_| Ok(UpdateDeadline::Continue(1_000_000)));
    store.set_epoch_deadline(0);
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "f").unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 7);
}

// --- limits ---------------------------------------------------------------

struct HostState {
    limits: StoreLimits,
}

fn limited_store(limits: StoreLimits) -> Store<HostState> {
    let mut store = Store::new(&Engine::default(), HostState { limits });
    store.limiter(|s| &mut s.limits);
    store
}

const PAGE: usize = 64 * 1024;

#[test]
fn limiter_denies_api_memory_grow() {
    let mut store = limited_store(StoreLimitsBuilder::new().memory_size(2 * PAGE).build());
    let mem = Memory::new(&mut store, MemoryType::new(1, None)).unwrap();
    assert_eq!(mem.grow(&mut store, 1).unwrap(), 1); // 1 -> 2 pages
    assert!(mem.grow(&mut store, 1).is_err()); // 2 -> 3 pages, denied
}

#[test]
fn limiter_controls_guest_memory_grow() {
    let grower = "(module (memory 1)
        (func (export \"grow\") (param i32) (result i32) local.get 0 memory.grow))";

    // Default: denied grow returns -1.
    let engine = Engine::default();
    let m = module(&engine, grower);
    let mut store = limited_store(StoreLimitsBuilder::new().memory_size(2 * PAGE).build());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "grow").unwrap();
    let mut out = [Val::I32(0)];
    f.call(&mut store, &[Val::I32(1)], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 1); // 1 -> 2 ok
    f.call(&mut store, &[Val::I32(1)], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), -1); // denied -> -1

    // trap_on_grow_failure: denied grow traps.
    let mut store = limited_store(
        StoreLimitsBuilder::new()
            .memory_size(PAGE)
            .trap_on_grow_failure(true)
            .build(),
    );
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "grow").unwrap();
    assert!(f
        .call(&mut store, &[Val::I32(1)], &mut [Val::I32(0)])
        .is_err());
}

#[test]
fn limiter_caps_instance_count() {
    let mut store = limited_store(StoreLimitsBuilder::new().instances(1).build());
    let engine = store.engine().clone();
    let m = module(&engine, "(module)");
    assert!(Instance::new(&mut store, &m, &[]).is_ok());
    assert!(Instance::new(&mut store, &m, &[]).is_err());
}

// --- reference-types: ref ops + table ref-ops (#26a) -----------------------

#[test]
fn ref_func_and_is_null() {
    let engine = Engine::default();
    // returns (is_null(ref.func 0), is_null(ref.null func))
    let m = module(
        &engine,
        "(module
            (func $f)
            (elem declare func $f)
            (func (export \"check\") (result i32 i32)
                (ref.is_null (ref.func $f))
                (ref.is_null (ref.null func))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "check").unwrap();
    let mut out = [Val::I32(0), Val::I32(0)];
    f.call(&mut store, &[], &mut out).unwrap();
    assert_eq!(out[0].unwrap_i32(), 0); // ref.func is not null
    assert_eq!(out[1].unwrap_i32(), 1); // ref.null is null
}

#[test]
fn table_get_set_grow_size() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module
            (table (export \"t\") 1 funcref)
            (func $f)
            (elem declare func $f)
            (func (export \"set0\") (table.set 0 (i32.const 0) (ref.func $f)))
            (func (export \"is_null\") (param i32) (result i32)
                (ref.is_null (table.get 0 (local.get 0))))
            (func (export \"size\") (result i32) (table.size 0))
            (func (export \"grow\") (param i32) (result i32)
                (table.grow 0 (ref.null func) (local.get 0))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let call = |store: &mut Store<()>, name: &str, arg: Option<i32>| -> i32 {
        let f = inst.get_func(&mut *store, name).unwrap();
        let args: Vec<Val> = arg.map(Val::I32).into_iter().collect();
        let mut out = [Val::I32(0)];
        f.call(store, &args, &mut out).unwrap();
        out[0].unwrap_i32()
    };
    assert_eq!(call(&mut store, "is_null", Some(0)), 1); // slot 0 starts null
    inst.get_func(&mut store, "set0")
        .unwrap()
        .call(&mut store, &[], &mut [])
        .unwrap();
    assert_eq!(call(&mut store, "is_null", Some(0)), 0); // now a funcref
    assert_eq!(call(&mut store, "size", None), 1);
    assert_eq!(call(&mut store, "grow", Some(3)), 1); // old size 1
    assert_eq!(call(&mut store, "size", None), 4);
}

#[test]
fn externref_host_payload_round_trips_through_wasm() {
    let engine = Engine::default();
    // Stores the given externref in a table and hands it back.
    let m = module(
        &engine,
        "(module
            (table 1 externref)
            (func (export \"roundtrip\") (param externref) (result externref)
                (table.set 0 (i32.const 0) (local.get 0))
                (table.get 0 (i32.const 0))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let r = ExternRef::new(&mut store, "host-state".to_string()).unwrap();

    let func = inst.get_func(&mut store, "roundtrip").unwrap();
    let mut out = [Val::FuncRef(None)];
    func.call(&mut store, &[Val::ExternRef(Some(r))], &mut out)
        .unwrap();

    let Val::ExternRef(Some(back)) = out[0] else {
        panic!("expected a non-null externref");
    };
    let data = back.data(&store).unwrap().unwrap();
    assert_eq!(data.downcast_ref::<String>().unwrap(), "host-state");
}

#[test]
fn table_get_out_of_bounds_traps() {
    let engine = Engine::default();
    let m = module(
        &engine,
        "(module (table 1 funcref)
            (func (export \"get\") (param i32) (result funcref) (table.get 0 (local.get 0))))",
    );
    let mut store = Store::new(&engine, ());
    let inst = Instance::new(&mut store, &m, &[]).unwrap();
    let f = inst.get_func(&mut store, "get").unwrap();
    let err = f
        .call(&mut store, &[Val::I32(5)], &mut [Val::FuncRef(None)])
        .unwrap_err();
    assert_eq!(*err.downcast_ref::<Trap>().unwrap(), Trap::TableOutOfBounds);
}
