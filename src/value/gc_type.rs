//! GC composite-type descriptors (`StructType`/`ArrayType`/…), the host-facing surface
//! for declaring and reflecting on GC types. Signature/shape parity with wasmtime so an
//! embedder that builds GC objects from host code compiles now; real allocation/interning
//! lands in Phase 5 (#27). These are plain data descriptors (no engine interning yet).

use crate::engine::Engine;
use crate::value::{Mutability, ValType};
use crate::Result;

/// Whether a GC type may be subtyped further (`final` vs `non-final`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Finality {
    Final,
    NonFinal,
}

/// The storage type of a struct field / array element: a packed `i8`/`i16`, or a value type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum StorageType {
    I8,
    I16,
    ValType(ValType),
}

/// A struct field / array element type: mutability plus storage type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FieldType {
    mutability: Mutability,
    element_type: StorageType,
}

impl FieldType {
    pub fn new(mutability: Mutability, element_type: StorageType) -> Self {
        FieldType {
            mutability,
            element_type,
        }
    }

    pub fn mutability(&self) -> Mutability {
        self.mutability
    }

    pub fn element_type(&self) -> &StorageType {
        &self.element_type
    }
}

/// A GC struct type descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StructType {
    finality: Finality,
    fields: Vec<FieldType>,
}

impl StructType {
    /// Creates a final struct type with the given fields. (`engine` is accepted for
    /// wasmtime parity; engine-level type interning arrives in #27.)
    pub fn new(engine: &Engine, fields: impl IntoIterator<Item = FieldType>) -> Result<Self> {
        Self::with_finality_and_supertype(engine, Finality::Final, None, fields)
    }

    pub fn with_finality_and_supertype(
        engine: &Engine,
        finality: Finality,
        supertype: Option<&Self>,
        fields: impl IntoIterator<Item = FieldType>,
    ) -> Result<Self> {
        let _ = (engine, supertype);
        Ok(StructType {
            finality,
            fields: fields.into_iter().collect(),
        })
    }

    pub fn field(&self, i: usize) -> Option<FieldType> {
        self.fields.get(i).cloned()
    }

    pub fn fields(&self) -> impl ExactSizeIterator<Item = FieldType> + '_ {
        self.fields.iter().cloned()
    }
}

/// A GC array type descriptor. The element is boxed so the `HeapType` ↔ `ArrayType`
/// recursion (via `StorageType::ValType`) stays finite-sized.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ArrayType {
    finality: Finality,
    element: Box<FieldType>,
}

impl ArrayType {
    /// Creates a final array type with the given element type.
    pub fn new(engine: &Engine, field_type: FieldType) -> Self {
        Self::with_finality_and_supertype(engine, Finality::Final, None, field_type)
            .expect("array type without a supertype never fails")
    }

    pub fn with_finality_and_supertype(
        engine: &Engine,
        finality: Finality,
        supertype: Option<&Self>,
        field_type: FieldType,
    ) -> Result<Self> {
        let _ = (engine, supertype);
        Ok(ArrayType {
            finality,
            element: Box::new(field_type),
        })
    }

    pub fn field_type(&self) -> FieldType {
        (*self.element).clone()
    }

    pub fn element_type(&self) -> StorageType {
        self.element.element_type().clone()
    }
}

/// An engine-level recursion-group handle (`StructType`/`ArrayType` belong to one).
/// Opaque stub — real canonicalized rec-groups arrive in #27c.
#[derive(Clone, Debug)]
pub struct RecGroupType {
    _private: (),
}
