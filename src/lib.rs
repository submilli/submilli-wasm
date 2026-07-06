//! A WebAssembly interpreter with a [`wasmtime`]-compatible API, built for fast
//! compilation and startup rather than peak execution speed.
//!
//! Most engines JIT-compile a module because they expect to run it many times.
//! This one is for the opposite workload — code that is compiled, run once, and
//! thrown away (its home is a product running LLM-generated modules). Compilation
//! is a single fused validate+lower pass over the bytes, `Store` creation is
//! free, and the guest↔host call boundary is tens of nanoseconds — while pure
//! compute runs at interpreter speed, slower than any JIT.
//!
//! The full **Wasm 3.0** feature set is supported — including GC, exception
//! handling, tail calls, memory64, and (behind the `simd` feature) fixed-width
//! SIMD — and every guest is treated as hostile: the crate is `unsafe`-free,
//! guest-reachable paths trap instead of panicking, and memory, stack, fuel,
//! epochs, and allocations are all bounded per [`Store`].
//!
//! # Example
//!
//! ```
//! use submilli_wasm::{Engine, Linker, Module, Store};
//!
//! # fn main() -> anyhow::Result<()> {
//! let engine = Engine::default();
//! let mut store = Store::new(&engine, ());
//! let mut linker = Linker::new(&engine);
//! linker.func_wrap("host", "add", |a: i32, b: i32| a + b)?;
//!
//! let wasm = wat::parse_str(
//!     r#"(module
//!         (import "host" "add" (func $add (param i32 i32) (result i32)))
//!         (func (export "run") (param i32) (result i32)
//!             (call $add (local.get 0) (i32.const 35))))"#,
//! )?;
//! let module = Module::new(&engine, &wasm)?;
//! let instance = linker.instantiate(&mut store, &module)?;
//! let run = instance.get_typed_func::<i32, i32>(&mut store, "run")?;
//! assert_eq!(run.call(&mut store, 7)?, 42);
//! # Ok(())
//! # }
//! ```
//!
//! # Drop-in `wasmtime` replacement
//!
//! The public surface mirrors `wasmtime` 45.x — `Engine`, `Store<T>`, `Module`,
//! `Linker`, typed and untyped calls, host functions sync and async, fuel,
//! epochs, [`ResourceLimiter`], `Trap`/`WasmBacktrace` via `downcast_ref` — so
//! an existing embedder can switch with a Cargo package rename and no code
//! changes:
//!
//! ```toml
//! [dependencies]
//! wasmtime = { package = "submilli-wasm", version = "0.1", features = ["async"] }
//! ```
//!
//! The surface grows by need rather than by completeness; if a method you use
//! is missing, an issue or PR is welcome.
//!
//! # Cargo features
//!
//! - **`async`** — `call_async`, `func_wrap_async`/`func_new_async`, async
//!   resource limiters, and fuel/epoch yielding to the executor.
//! - **`simd`** — the fixed-width SIMD (`v128`) and relaxed-SIMD proposals;
//!   off by default to keep compile time and binary size lean.
//!
//! # More
//!
//! The story, benchmarks, and design docs live in the
//! [repository](https://github.com/submilli/submilli-wasm); the threat model in
//! [`SECURITY.md`](https://github.com/submilli/submilli-wasm/blob/main/SECURITY.md).
//!
//! [`wasmtime`]: https://docs.rs/wasmtime

// TODO: remove these as `todo!()` stubs are replaced by real bodies.
// They fire only because placeholder bodies don't yet read their fields/params.
#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(clippy::unused_self)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::return_self_not_must_use)]

/// Expands `$mac` once per arity 0..=16, passing the count then that many type
/// idents (e.g. `$mac!(2 A1 A2)`). Mirrors wasmtime's `for_each_function_signature!`.
macro_rules! for_each_arity {
    ($mac:ident) => {
        $mac!(0);
        $mac!(1 A1);
        $mac!(2 A1 A2);
        $mac!(3 A1 A2 A3);
        $mac!(4 A1 A2 A3 A4);
        $mac!(5 A1 A2 A3 A4 A5);
        $mac!(6 A1 A2 A3 A4 A5 A6);
        $mac!(7 A1 A2 A3 A4 A5 A6 A7);
        $mac!(8 A1 A2 A3 A4 A5 A6 A7 A8);
        $mac!(9 A1 A2 A3 A4 A5 A6 A7 A8 A9);
        $mac!(10 A1 A2 A3 A4 A5 A6 A7 A8 A9 A10);
        $mac!(11 A1 A2 A3 A4 A5 A6 A7 A8 A9 A10 A11);
        $mac!(12 A1 A2 A3 A4 A5 A6 A7 A8 A9 A10 A11 A12);
        $mac!(13 A1 A2 A3 A4 A5 A6 A7 A8 A9 A10 A11 A12 A13);
        $mac!(14 A1 A2 A3 A4 A5 A6 A7 A8 A9 A10 A11 A12 A13 A14);
        $mac!(15 A1 A2 A3 A4 A5 A6 A7 A8 A9 A10 A11 A12 A13 A14 A15);
        $mac!(16 A1 A2 A3 A4 A5 A6 A7 A8 A9 A10 A11 A12 A13 A14 A15 A16);
    };
}
pub(crate) use for_each_arity;

mod backtrace;
mod canon;
mod config;
mod engine;
mod error;
mod exception;
mod exec;
mod extern_;
mod func;
mod gc;
mod instance;
mod linker;
mod module;
mod store;
mod trap;
mod value;

// `bail!`/`ensure!`/`format_err!` are exported at the crate root via `#[macro_export]`.
pub use crate::error::{Error, Result};

pub use crate::backtrace::{FrameInfo, FrameSymbol, WasmBacktrace};
pub use crate::config::{Collector, Config, OptLevel, WasmBacktraceDetails};
pub use crate::engine::{Engine, EngineWeak};
pub use crate::exception::ThrownException;
pub use crate::extern_::{Extern, Global, Memory, MemoryAccessError, Table, Tag};
pub use crate::func::{
    Caller, Func, IntoFunc, TypedFunc, WasmParams, WasmResults, WasmRet, WasmTy,
};
pub use crate::gc::GcHeapOutOfMemory;
pub use crate::instance::Instance;
pub use crate::linker::Linker;
pub use crate::module::{Module, ModuleLimits};
#[cfg(feature = "async")]
pub use crate::store::ResourceLimiterAsync;
pub use crate::store::{
    AsContext, AsContextMut, CallHook, ResourceLimiter, Store, StoreContext, StoreContextMut,
    StoreLimits, StoreLimitsBuilder, UpdateDeadline,
};
pub use crate::trap::Trap;
pub use crate::value::{
    AnyRef, ArrayRef, ArrayRefPre, ArrayType, ArrayTypeBuilder, ExnRef, ExnRefPre, ExnType,
    ExportType, ExternRef, ExternType, FieldType, Finality, ForwardRefElementBuilder,
    ForwardRefFieldBuilder, ForwardRefFuncValBuilder, FuncType, FuncTypeBuilder, GlobalType,
    HeapType, ImportType, MemoryType, Mutability, PendingType, RecGroup, RecGroupBuilder,
    RecGroupType, Ref, RefType, RootScope, Rooted, StorageType, StructRef, StructRefPre,
    StructType, StructTypeBuilder, TableType, TagType, Val, ValType, V128,
};
