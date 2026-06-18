//! Runtime values and types: `Val`/`Ref`, the type descriptors, and GC ref stubs.

mod gc_ref;
mod gc_type;
mod types;
mod val;

pub use gc_ref::{
    AnyRef, ArrayRef, ArrayRefPre, ExnRef, ExternRef, RootScope, Rooted, StructRef, StructRefPre,
};
pub use gc_type::{ArrayType, FieldType, Finality, RecGroupType, StorageType, StructType};
pub use types::{
    ExportType, ExternType, FuncType, GlobalType, HeapType, ImportType, MemoryType, Mutability,
    RefType, TableType, ValType,
};
pub use val::{Ref, Val, V128};
