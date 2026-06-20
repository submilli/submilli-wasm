// Shared, compile-only embedder surface. Included by both `compat_alias.rs`
// (our crate, aliased as `wasmtime`) and `compat_real.rs` (real `wasmtime`).
//
// Functions here are never called, so `todo!()` stubs never execute — compiling
// this against BOTH crates is the proof that our public API matches wasmtime's.
// No inner attributes here: the including files set crate-level allows.

use wasmtime::{
    AnyRef, ArrayRef, ArrayRefPre, ArrayType, Caller, Config, Engine, Extern, ExternRef, FieldType,
    Finality, Func, FuncType, Global, GlobalType, HeapType, Instance, Linker, Memory, MemoryType,
    Module, Mutability, Ref, RefType, ResourceLimiter, Result, RootScope, StorageType, Store,
    StoreLimits, StoreLimitsBuilder, StructRef, StructRefPre, StructType, Table, TableType,
    TypedFunc, UpdateDeadline, Val, ValType,
};

struct HostState {
    counter: i32,
    limits: StoreLimits,
}

fn build_engine() -> Result<Engine> {
    let mut config = Config::new();
    config
        .consume_fuel(true)
        .epoch_interruption(true)
        .wasm_multi_value(true)
        .wasm_bulk_memory(true)
        .max_wasm_stack(1 << 20);
    Engine::new(&config)
}

fn make_store(engine: &Engine) -> Store<HostState> {
    let limits = StoreLimitsBuilder::new()
        .memory_size(1 << 20)
        .table_elements(10_000)
        .build();
    Store::new(engine, HostState { counter: 0, limits })
}

fn typed_host_fns(store: &mut Store<HostState>) {
    // Typed host fn, no caller.
    let _add = Func::wrap(&mut *store, |a: i32, b: i32| a + b);

    // Typed host fn with a caller, mutating host state, returning `Result`.
    let _log = Func::wrap(
        &mut *store,
        |mut caller: Caller<'_, HostState>, x: i32| -> Result<()> {
            caller.data_mut().counter += x;
            Ok(())
        },
    );

    // Host fn reading guest memory through the caller (the canonical pattern).
    let _peek = Func::wrap(
        &mut *store,
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<i32> {
            let mem = match caller.get_export("memory") {
                Some(Extern::Memory(m)) => m,
                _ => wasmtime::bail!("missing memory export"),
            };
            let data = mem.data(&caller);
            Ok(data.len() as i32 + ptr + len)
        },
    );
}

fn untyped_host_fn(store: &mut Store<HostState>) -> Func {
    let engine = store.engine().clone();
    let ty = FuncType::new(&engine, [ValType::I32, ValType::I32], [ValType::I32]);
    Func::new(
        &mut *store,
        ty,
        |_caller: Caller<'_, HostState>, params: &[Val], results: &mut [Val]| {
            results[0] = params[0];
            Ok(())
        },
    )
}

fn linking(store: &mut Store<HostState>, module: &Module) -> Result<Instance> {
    let engine = store.engine().clone();
    let mut linker: Linker<HostState> = Linker::new(&engine);
    linker.allow_shadowing(true);
    linker.func_wrap("env", "add", |a: i32, b: i32| a + b)?;
    linker.func_wrap("env", "log", |mut caller: Caller<'_, HostState>, x: i32| {
        caller.data_mut().counter += x;
    })?;
    let mem = Memory::new(&mut *store, MemoryType::new(1, Some(10)))?;
    linker.define(&mut *store, "env", "memory", mem)?;
    linker.instantiate(&mut *store, module)
}

fn typed_call(store: &mut Store<HostState>, instance: &Instance) -> Result<i32> {
    let add: TypedFunc<(i32, i32), i32> = instance.get_typed_func(&mut *store, "add")?;
    add.call(&mut *store, (2, 3))
}

fn untyped_call(store: &mut Store<HostState>, func: &Func) -> Result<()> {
    let mut results = [Val::I32(0)];
    func.call(&mut *store, &[Val::I32(1), Val::I32(2)], &mut results)?;
    let _ = results[0].unwrap_i32();
    Ok(())
}

fn entities(store: &mut Store<HostState>) -> Result<()> {
    let mem = Memory::new(&mut *store, MemoryType::new(1, None))?;
    let _old_pages = mem.grow(&mut *store, 1)?;
    let _pages = mem.size(&*store);
    let mut buf = [0u8; 4];
    let _ = mem.read(&*store, 0, &mut buf);
    mem.write(&mut *store, 0, &buf).ok();

    let g = Global::new(
        &mut *store,
        GlobalType::new(ValType::I32, Mutability::Var),
        Val::I32(7),
    )?;
    g.set(&mut *store, Val::I32(8))?;
    let _v = g.get(&mut *store);

    let tt = TableType::new(RefType::new(true, HeapType::Func), 1, None);
    let t = Table::new(&mut *store, tt, Ref::Func(None))?;
    let _old = t.grow(&mut *store, 1, Ref::Func(None))?;
    Ok(())
}

fn resource_control(store: &mut Store<HostState>, engine: &Engine) -> Result<()> {
    store.set_fuel(10_000)?;
    let _fuel = store.get_fuel()?;
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|_ctx| Ok(UpdateDeadline::Continue(1)));
    store.limiter(|state| &mut state.limits);
    engine.increment_epoch();
    let weak = engine.weak();
    let _upgraded = weak.upgrade();
    Ok(())
}

// externref host-payload API (#26c): wrap host state in an externref and read it back,
// including under a `RootScope`.
fn externref_host_state(store: &mut Store<HostState>) -> Result<()> {
    let r = ExternRef::new(&mut *store, 42u32)?;
    let _data = r.data(&*store)?;
    let mut scope = RootScope::new(&mut *store);
    let _scoped = ExternRef::new(&mut scope, "hi".to_string())?;
    Ok(())
}

// GC host-API surface (#24d/#27b): a host constructs GC type descriptors, allocates objects,
// reads them back, and upcasts to `anyref` — all proving signature parity with wasmtime.
fn gc_host_api(engine: &Engine, store: &mut Store<HostState>) -> Result<()> {
    let field = FieldType::new(Mutability::Var, StorageType::ValType(ValType::I32));
    let _ = field.element_type().is_val_type();
    let _st0 = StructType::new(engine, [field.clone()])?;
    let st = StructType::with_finality_and_supertype(engine, Finality::Final, None, [field.clone()])?;
    let _ = st.fields().count();
    let _ = st.field(0);
    let at = ArrayType::new(engine, field);
    let _ = at.field_type();
    let _ = at.element_type();

    // Concrete GC heap types flow through `RefType`/`ValType`.
    let _struct_ty = ValType::Ref(RefType::new(true, HeapType::ConcreteStruct(st.clone())));
    let _array_ty = ValType::Ref(RefType::new(true, HeapType::ConcreteArray(at.clone())));

    // Allocate structs/arrays from host code.
    let spre = StructRefPre::new(&mut *store, st);
    let s: wasmtime::Rooted<StructRef> = StructRef::new(&mut *store, &spre, &[Val::I32(1)])?;
    let apre = ArrayRefPre::new(&mut *store, at);
    let a: wasmtime::Rooted<ArrayRef> = ArrayRef::new(&mut *store, &apre, &Val::I32(0), 4)?;
    let _a2 = ArrayRef::new_fixed(&mut *store, &apre, &[Val::I32(1), Val::I32(2)])?;

    // Read fields/elements back; upcast struct/array → anyref and reinterpret.
    let _f = s.field(&mut *store, 0)?;
    let _len = a.len(&*store)?;
    let _e = a.get(&mut *store, 0)?;
    let any: wasmtime::Rooted<AnyRef> = s.into();
    let _back = any.unwrap_struct(&*store)?;
    Ok(())
}

struct MyLimiter {
    max_bytes: usize,
}

impl ResourceLimiter for MyLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        Ok(desired <= self.max_bytes)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        Ok(true)
    }
}
