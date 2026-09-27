use submilli_wasm::{
    Config, Engine, Instance, Linker, Module, ResourceLimiter, Result, Store, Trap,
};

#[cfg(feature = "async")]
mod async_host;

fn main() {
    host_calls_and_memory();
    fuel_interrupts();
    wide_sizes_and_indices();
    wide_segments();
    #[cfg(feature = "async")]
    async_host::run();
    #[cfg(feature = "simd")]
    simd();
}

#[cfg(feature = "simd")]
fn simd() {
    let engine = Engine::default();
    let guest = module(
        &engine,
        r#"(module
        (func (export "run") (result i32)
            v128.const i32x4 1 2 3 4
            v128.const i32x4 10 20 30 40
            i32x4.add i32x4.extract_lane 2))"#,
    );
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &guest, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 33);
}

fn module(engine: &Engine, source: &str) -> Module {
    Module::new(engine, wat::parse_str(source).unwrap()).unwrap()
}

fn host_calls_and_memory() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap("host", "add", |a: i32, b: i32| a + b)
        .unwrap();
    let guest = module(
        &engine,
        r#"(module
        (import "host" "add" (func $add (param i32 i32) (result i32)))
        (memory (export "memory") 1 2)
        (func (export "run") (result i32)
            i32.const 0 i32.const 20 i32.const 22 call $add i32.store
            i32.const 0 i32.load)
        (func (export "grow") (result i32) i32.const 1 memory.grow))"#,
    );
    let instance = linker.instantiate(&mut store, &guest).unwrap();
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    assert_eq!(run.call(&mut store, ()).unwrap(), 42);
    let grow = instance
        .get_typed_func::<(), i32>(&mut store, "grow")
        .unwrap();
    assert_eq!(grow.call(&mut store, ()).unwrap(), 1);
    assert_eq!(grow.call(&mut store, ()).unwrap(), -1);
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert!(memory.data(&store)[65536..].iter().all(|byte| *byte == 0));
}

fn fuel_interrupts() {
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let guest = module(
        &engine,
        r#"(module
        (func (export "run") (loop br 0)))"#,
    );
    let mut store = Store::new(&engine, ());
    store.set_fuel(100).unwrap();
    let instance = Instance::new(&mut store, &guest, &[]).unwrap();
    let run = instance
        .get_typed_func::<(), ()>(&mut store, "run")
        .unwrap();
    let error = run.call(&mut store, ()).unwrap_err();
    assert_eq!(error.downcast_ref::<Trap>(), Some(&Trap::OutOfFuel));
}

#[derive(Default)]
struct Requests {
    memory: usize,
    table: usize,
}

impl ResourceLimiter for Requests {
    fn memory_growing(&mut self, _: usize, desired: usize, _: Option<usize>) -> Result<bool> {
        self.memory = desired;
        Ok(true)
    }

    fn table_growing(&mut self, _: usize, desired: usize, _: Option<usize>) -> Result<bool> {
        self.table = desired;
        Ok(true)
    }
}

fn wide_sizes_and_indices() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, Requests::default());
    store.limiter(|requests| requests);
    let guest = module(&engine, include_str!("../wide.wat"));
    let instance = Instance::new(&mut store, &guest, &[]).unwrap();
    for name in ["grow_memory", "grow_table"] {
        let grow = instance
            .get_typed_func::<(), i64>(&mut store, name)
            .unwrap();
        assert_eq!(grow.call(&mut store, ()).unwrap(), -1);
    }
    assert_eq!(store.data().memory, usize::MAX);
    assert_eq!(store.data().table, usize::MAX);
    for name in ["get", "set"] {
        let access = instance.get_typed_func::<(), ()>(&mut store, name).unwrap();
        let error = access.call(&mut store, ()).unwrap_err();
        assert_eq!(error.downcast_ref::<Trap>(), Some(&Trap::TableOutOfBounds));
    }
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    assert_eq!(memory.size(&store), 1);
    let table = instance.get_table(&mut store, "table").unwrap();
    assert_eq!(table.size(&store), 1);
    for source in [
        "(module (memory i64 4294967296))",
        "(module (table i64 4294967296 funcref))",
    ] {
        let guest = module(&engine, source);
        assert!(Instance::new(&mut store, &guest, &[]).is_err());
    }
}

fn wide_segments() {
    let engine = Engine::default();
    let mut store = Store::new(&engine, ());
    for (source, trap) in [
        (
            r#"(module (memory i64 1) (data (i64.const 4294967296) "x"))"#,
            Trap::MemoryOutOfBounds,
        ),
        (
            "(module (table i64 1 funcref) (func $f) (elem (i64.const 4294967296) func $f))",
            Trap::TableOutOfBounds,
        ),
    ] {
        let guest = module(&engine, source);
        let error = Instance::new(&mut store, &guest, &[]).unwrap_err();
        assert_eq!(error.downcast_ref::<Trap>(), Some(&trap));
    }
}
