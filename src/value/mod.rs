//! Runtime values and types: `Val`/`Ref`, the type descriptors, and GC ref stubs.

mod gc_ref;
mod types;
mod val;

pub use gc_ref::{AnyRef, ExnRef, ExternRef, RootScope, Rooted};
pub use types::{
    ExportType, ExternType, FuncType, GlobalType, HeapType, ImportType, MemoryType, Mutability,
    RefType, TableType, ValType,
};
pub use val::{Ref, Val, V128};
