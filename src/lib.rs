//! `submilli-wasm`: a WebAssembly interpreter with a `wasmtime`-compatible API.
//!
//! Fast-compilation-first, stack-based interpreter. See `docs/ARCHITECTURE.md`.
//!
//! The public surface mirrors `wasmtime` 45.x so embedder code is drop-in
//! (`use submilli_wasm as wasmtime;`). The interpreter is filled in incrementally.

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
pub use crate::instance::Instance;
pub use crate::linker::Linker;
pub use crate::module::Module;
#[cfg(feature = "async")]
pub use crate::store::ResourceLimiterAsync;
pub use crate::store::{
    AsContext, AsContextMut, CallHook, ResourceLimiter, Store, StoreContext, StoreContextMut,
    StoreLimits, StoreLimitsBuilder, UpdateDeadline,
};
pub use crate::trap::Trap;
pub use crate::value::{
    AnyRef, ArrayRef, ArrayRefPre, ArraySuperType, ArrayType, CompositeType, ExnRef, ExnRefPre,
    ExnType, ExportType, ExternRef, ExternType, FieldTemplate, FieldType, Finality, FuncSuperType,
    FuncType, GlobalType, HeapType, HeapTypeTemplate, ImportType, MemoryType, Mutability,
    PendingArrayId, PendingFuncId, PendingStructId, RecGroup, RecGroupBuilder, RecGroupType, Ref,
    RefType, RootScope, Rooted, StorageType, StorageTypeTemplate, StructRef, StructRefPre,
    StructSuperType, StructType, TableType, TagType, Val, ValType, ValTypeTemplate, V128,
};
