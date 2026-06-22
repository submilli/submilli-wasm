//! Runtime values and types: `Val`/`Ref`, the type descriptors, and GC ref stubs.

mod exn_ref;
mod gc_ref;
mod gc_type;
mod rec_group;
mod tag_type;
mod types;
mod val;

pub use exn_ref::ExnRefPre;
pub use gc_ref::{
    AnyRef, ArrayRef, ArrayRefPre, ExnRef, ExternRef, RootScope, Rooted, StructRef, StructRefPre,
};
pub use gc_type::{ArrayType, ExnType, FieldType, Finality, RecGroupType, StorageType, StructType};
pub use rec_group::{
    ArraySuperType, CompositeType, FieldTemplate, FuncSuperType, HeapTypeTemplate, PendingArrayId,
    PendingFuncId, PendingStructId, RecGroup, RecGroupBuilder, StorageTypeTemplate,
    StructSuperType, ValTypeTemplate,
};
pub use tag_type::TagType;
pub use types::{
    ExportType, ExternType, FuncType, GlobalType, HeapType, ImportType, MemoryType, Mutability,
    RefType, TableType, ValType,
};
pub use val::{Ref, Val, V128};
