//! wasmtime-compatible accessors on `ExternType` and `HeapType`: the `func()`/`global()`/… (and
//! panicking `unwrap_*`) projections, and `as_concrete_struct()`/`…_array`/`…_func` for recovering
//! a concrete GC/func type from a `HeapType`. Split from `types` to keep that file under the cap.

use super::gc_type::{ArrayType, StructType};
use super::tag_type::TagType;
use super::types::{
    ExternType, FuncType, GlobalType, HeapType, MemoryType, RefType, TableType, ValType,
};

impl ValType {
    /// Whether this is a reference type.
    pub fn is_ref(&self) -> bool {
        matches!(self, ValType::Ref(_))
    }

    /// The underlying [`RefType`] if this is a reference type, else `None`.
    // Inherent `as_ref` matches `wasmtime::ValType::as_ref` (not the `AsRef` trait).
    #[allow(clippy::should_implement_trait)]
    pub fn as_ref(&self) -> Option<&RefType> {
        match self {
            ValType::Ref(r) => Some(r),
            _ => None,
        }
    }

    /// The underlying [`RefType`], panicking if this is not a reference type.
    pub fn unwrap_ref(&self) -> &RefType {
        self.as_ref().expect("expected a reference type")
    }
}

macro_rules! extern_type_accessors {
    ($(($variant:ident($ty:ty) $get:ident $unwrap:ident))*) => ($(
        /// The underlying type if this `ExternType` is that variant, else `None`.
        pub fn $get(&self) -> Option<&$ty> {
            if let ExternType::$variant(e) = self {
                Some(e)
            } else {
                None
            }
        }

        /// The underlying type, panicking if this `ExternType` is a different variant.
        pub fn $unwrap(&self) -> &$ty {
            self.$get().expect(concat!("expected ", stringify!($ty)))
        }
    )*)
}

impl ExternType {
    extern_type_accessors! {
        (Func(FuncType) func unwrap_func)
        (Global(GlobalType) global unwrap_global)
        (Table(TableType) table unwrap_table)
        (Memory(MemoryType) memory unwrap_memory)
        (Tag(TagType) tag unwrap_tag)
    }
}

impl HeapType {
    /// The concrete struct type if this is a `ConcreteStruct` heap type, else `None`.
    pub fn as_concrete_struct(&self) -> Option<&StructType> {
        match self {
            HeapType::ConcreteStruct(t) => Some(t),
            _ => None,
        }
    }

    /// The concrete struct type, panicking if this is not a concrete struct heap type.
    pub fn unwrap_concrete_struct(&self) -> &StructType {
        self.as_concrete_struct()
            .expect("expected a concrete struct heap type")
    }

    /// The concrete array type if this is a `ConcreteArray` heap type, else `None`.
    pub fn as_concrete_array(&self) -> Option<&ArrayType> {
        match self {
            HeapType::ConcreteArray(t) => Some(t),
            _ => None,
        }
    }

    /// The concrete array type, panicking if this is not a concrete array heap type.
    pub fn unwrap_concrete_array(&self) -> &ArrayType {
        self.as_concrete_array()
            .expect("expected a concrete array heap type")
    }

    /// The concrete function type if this is a `ConcreteFunc` heap type, else `None`.
    pub fn as_concrete_func(&self) -> Option<&FuncType> {
        match self {
            HeapType::ConcreteFunc(t) => Some(t),
            _ => None,
        }
    }

    /// The concrete function type, panicking if this is not a concrete func heap type.
    pub fn unwrap_concrete_func(&self) -> &FuncType {
        self.as_concrete_func()
            .expect("expected a concrete func heap type")
    }
}
