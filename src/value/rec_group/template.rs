//! The public type descriptors for [`RecGroupBuilder`](super::RecGroupBuilder): forward-reference
//! labels (`Pending*Id`) and the `*Template` mirrors of `HeapType`/`ValType`/`StorageType`/
//! `FieldType` that can additionally hold a sibling label. Lowered to module IR by [`super::lower`].

use crate::value::{
    ArrayType, FieldType, FuncType, HeapType, Mutability, StorageType, StructType, ValType,
};

/// A forward-reference label for a struct being defined in a `RecGroupBuilder`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PendingStructId {
    pub(super) builder_id: usize,
    pub(super) index: u32,
}

/// A forward-reference label for an array being defined in a `RecGroupBuilder`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PendingArrayId {
    pub(super) builder_id: usize,
    pub(super) index: u32,
}

/// A forward-reference label for a function type being defined in a `RecGroupBuilder`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PendingFuncId {
    pub(super) builder_id: usize,
    pub(super) index: u32,
}

impl PendingStructId {
    pub(super) fn new(builder_id: usize, index: u32) -> Self {
        PendingStructId { builder_id, index }
    }
}
impl PendingArrayId {
    pub(super) fn new(builder_id: usize, index: u32) -> Self {
        PendingArrayId { builder_id, index }
    }
}
impl PendingFuncId {
    pub(super) fn new(builder_id: usize, index: u32) -> Self {
        PendingFuncId { builder_id, index }
    }
}

/// A heap type that may reference a sibling label being defined in the same builder.
#[derive(Clone, Debug)]
pub enum HeapTypeTemplate {
    Type(HeapType),
    LocalStruct(PendingStructId),
    LocalArray(PendingArrayId),
    LocalFunc(PendingFuncId),
}

impl From<HeapType> for HeapTypeTemplate {
    fn from(t: HeapType) -> Self {
        HeapTypeTemplate::Type(t)
    }
}
impl From<StructType> for HeapTypeTemplate {
    fn from(t: StructType) -> Self {
        HeapTypeTemplate::Type(HeapType::ConcreteStruct(t))
    }
}
impl From<ArrayType> for HeapTypeTemplate {
    fn from(t: ArrayType) -> Self {
        HeapTypeTemplate::Type(HeapType::ConcreteArray(t))
    }
}
impl From<FuncType> for HeapTypeTemplate {
    fn from(t: FuncType) -> Self {
        HeapTypeTemplate::Type(HeapType::ConcreteFunc(t))
    }
}
impl From<PendingStructId> for HeapTypeTemplate {
    fn from(id: PendingStructId) -> Self {
        HeapTypeTemplate::LocalStruct(id)
    }
}
impl From<PendingArrayId> for HeapTypeTemplate {
    fn from(id: PendingArrayId) -> Self {
        HeapTypeTemplate::LocalArray(id)
    }
}
impl From<PendingFuncId> for HeapTypeTemplate {
    fn from(id: PendingFuncId) -> Self {
        HeapTypeTemplate::LocalFunc(id)
    }
}

/// A value type that may reference a sibling label (via a reference template).
#[derive(Clone, Debug)]
pub enum ValTypeTemplate {
    Type(ValType),
    Ref {
        nullable: bool,
        heap: HeapTypeTemplate,
    },
}

impl ValTypeTemplate {
    /// A reference value type to `heap` (which may be a pending label).
    pub fn ref_(nullable: bool, heap: impl Into<HeapTypeTemplate>) -> Self {
        ValTypeTemplate::Ref {
            nullable,
            heap: heap.into(),
        }
    }
}

impl From<ValType> for ValTypeTemplate {
    fn from(t: ValType) -> Self {
        ValTypeTemplate::Type(t)
    }
}

/// A field/element storage type that may reference a sibling label.
#[derive(Clone, Debug)]
pub enum StorageTypeTemplate {
    Type(StorageType),
    Ref {
        nullable: bool,
        heap: HeapTypeTemplate,
    },
}

impl From<StorageType> for StorageTypeTemplate {
    fn from(t: StorageType) -> Self {
        StorageTypeTemplate::Type(t)
    }
}
impl From<ValType> for StorageTypeTemplate {
    fn from(t: ValType) -> Self {
        StorageTypeTemplate::Type(StorageType::ValType(t))
    }
}

/// A struct field / array element template (mutability + storage), forward-ref capable.
#[derive(Clone, Debug)]
pub struct FieldTemplate {
    pub(super) mutability: Mutability,
    pub(super) element: StorageTypeTemplate,
}

impl FieldTemplate {
    pub fn new(mutability: Mutability, element: impl Into<StorageTypeTemplate>) -> Self {
        FieldTemplate {
            mutability,
            element: element.into(),
        }
    }

    /// A reference field/element to `heap` (which may be a pending label).
    pub fn ref_(mutability: Mutability, nullable: bool, heap: impl Into<HeapTypeTemplate>) -> Self {
        FieldTemplate {
            mutability,
            element: StorageTypeTemplate::Ref {
                nullable,
                heap: heap.into(),
            },
        }
    }
}

impl From<FieldType> for FieldTemplate {
    fn from(t: FieldType) -> Self {
        FieldTemplate {
            mutability: t.mutability(),
            element: StorageTypeTemplate::Type(t.element_type().clone()),
        }
    }
}

/// The supertype of a struct being defined: a sibling label or an already-registered type.
#[derive(Clone, Debug)]
pub enum StructSuperType {
    Local(PendingStructId),
    Type(StructType),
}
impl From<PendingStructId> for StructSuperType {
    fn from(id: PendingStructId) -> Self {
        StructSuperType::Local(id)
    }
}
impl From<StructType> for StructSuperType {
    fn from(t: StructType) -> Self {
        StructSuperType::Type(t)
    }
}

/// The supertype of an array being defined.
#[derive(Clone, Debug)]
pub enum ArraySuperType {
    Local(PendingArrayId),
    Type(ArrayType),
}
impl From<PendingArrayId> for ArraySuperType {
    fn from(id: PendingArrayId) -> Self {
        ArraySuperType::Local(id)
    }
}
impl From<ArrayType> for ArraySuperType {
    fn from(t: ArrayType) -> Self {
        ArraySuperType::Type(t)
    }
}

/// The supertype of a function type being defined.
#[derive(Clone, Debug)]
pub enum FuncSuperType {
    Local(PendingFuncId),
    Type(FuncType),
}
impl From<PendingFuncId> for FuncSuperType {
    fn from(id: PendingFuncId) -> Self {
        FuncSuperType::Local(id)
    }
}
impl From<FuncType> for FuncSuperType {
    fn from(t: FuncType) -> Self {
        FuncSuperType::Type(t)
    }
}
