//! GC composite-type descriptors (`StructType`/`ArrayType`/…), the host-facing surface for
//! declaring and reflecting on GC types. `StructType`/`ArrayType` are engine-interned handles
//! (identity by canonical id; structure materialized from the registry), matching wasmtime.

use crate::canon::CanonicalTypeId;
use crate::engine::Engine;
use crate::value::{FuncType, Mutability, TagType, ValType};
use crate::Result;

/// Whether a GC type may be subtyped further (`final` vs `non-final`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Finality {
    Final,
    NonFinal,
}

/// The storage type of a struct field / array element: a packed `i8`/`i16`, or a value type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StorageType {
    I8,
    I16,
    ValType(ValType),
}

impl StorageType {
    /// Whether this storage type is a (non-packed) value type.
    pub fn is_val_type(&self) -> bool {
        matches!(self, StorageType::ValType(_))
    }
}

/// A struct field / array element type: mutability plus storage type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

/// A GC struct type — an engine-interned handle (identity by canonical id). Refcounted (#27i):
/// holds one registration on its rec group; `Clone`/`Drop` incref/decref (see `handle_id_traits!`).
pub struct StructType {
    engine: Engine,
    id: CanonicalTypeId,
}

impl StructType {
    pub fn new(engine: &Engine, fields: impl IntoIterator<Item = FieldType>) -> Result<Self> {
        Self::with_finality_and_supertype(engine, Finality::Final, None, fields)
    }

    pub fn with_finality_and_supertype(
        engine: &Engine,
        finality: Finality,
        supertype: Option<&Self>,
        fields: impl IntoIterator<Item = FieldType>,
    ) -> Result<Self> {
        let fields: Vec<FieldType> = fields.into_iter().collect();
        let id =
            engine.intern_struct_type(finality, supertype.map(StructType::canonical_id), &fields);
        Ok(StructType {
            engine: engine.clone(),
            id,
        })
    }

    pub(crate) fn from_id(engine: &Engine, id: CanonicalTypeId) -> Self {
        engine.incref_type(id);
        StructType {
            engine: engine.clone(),
            id,
        }
    }

    pub(crate) fn canonical_id(&self) -> CanonicalTypeId {
        self.id
    }

    pub fn field(&self, i: usize) -> Option<FieldType> {
        self.engine.struct_fields(self.id).into_iter().nth(i)
    }

    pub fn fields(&self) -> impl ExactSizeIterator<Item = FieldType> {
        self.engine.struct_fields(self.id).into_iter()
    }
}

/// A GC array type — an engine-interned handle (identity by canonical id). Refcounted (#27i),
/// like [`StructType`].
pub struct ArrayType {
    engine: Engine,
    id: CanonicalTypeId,
}

impl ArrayType {
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
        let id = engine.intern_array_type(
            finality,
            supertype.map(ArrayType::canonical_id),
            &field_type,
        );
        Ok(ArrayType {
            engine: engine.clone(),
            id,
        })
    }

    pub(crate) fn from_id(engine: &Engine, id: CanonicalTypeId) -> Self {
        engine.incref_type(id);
        ArrayType {
            engine: engine.clone(),
            id,
        }
    }

    pub(crate) fn canonical_id(&self) -> CanonicalTypeId {
        self.id
    }

    pub fn field_type(&self) -> FieldType {
        self.engine.array_field(self.id)
    }

    pub fn element_type(&self) -> StorageType {
        self.engine.array_field(self.id).element_type().clone()
    }
}

macro_rules! handle_id_traits {
    ($ty:ident) => {
        impl Clone for $ty {
            fn clone(&self) -> Self {
                self.engine.incref_type(self.id);
                $ty {
                    engine: self.engine.clone(),
                    id: self.id,
                }
            }
        }
        impl Drop for $ty {
            fn drop(&mut self) {
                self.engine.decref_type(self.id);
            }
        }
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }
        impl Eq for $ty {}
        impl core::hash::Hash for $ty {
            fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
                self.id.hash(state);
            }
        }
        impl core::fmt::Debug for $ty {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("id", &self.id)
                    .finish_non_exhaustive()
            }
        }
    };
}

handle_id_traits!(StructType);
handle_id_traits!(ArrayType);

/// An exception type — the value types an exception carries. Internally the tag's function type
/// (`[fields] → []`), so it shares `FuncType`'s engine-canonical identity + refcounting (#27i).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExnType {
    func: FuncType,
}

impl ExnType {
    pub fn new(engine: &Engine, fields: impl IntoIterator<Item = ValType>) -> Result<ExnType> {
        Ok(ExnType {
            func: FuncType::new(engine, fields, []),
        })
    }

    pub fn from_tag_type(tag: &TagType) -> Result<ExnType> {
        Ok(ExnType {
            func: tag.ty().clone(),
        })
    }

    /// The tag type an exception of this type is thrown with.
    pub fn tag_type(&self) -> TagType {
        TagType::new(self.func.clone())
    }

    pub fn field(&self, i: usize) -> Option<FieldType> {
        self.func.params().nth(i).map(exn_field)
    }

    pub fn fields(&self) -> impl ExactSizeIterator<Item = FieldType> {
        self.func.params().map(exn_field)
    }

    pub fn engine(&self) -> &Engine {
        self.func.engine()
    }

    pub fn matches(&self, other: &ExnType) -> bool {
        self.func == other.func
    }

    pub(crate) fn func(&self) -> &FuncType {
        &self.func
    }
}

/// An exception field is one of the tag's params: an immutable value-type slot.
fn exn_field(ty: ValType) -> FieldType {
    FieldType::new(Mutability::Const, StorageType::ValType(ty))
}

/// An engine-level recursion-group handle (`StructType`/`ArrayType` belong to one).
/// Opaque stub — real canonicalized rec-groups arrive with the host rec-group builder.
#[derive(Clone, Debug)]
pub struct RecGroupType {
    _private: (),
}
