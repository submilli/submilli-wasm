//! The nested per-type builders returned by `RecGroupBuilder::define_*`: fluent configuration of
//! one member (finality, supertype, fields/element/signature), committed with `finish`. The
//! `ForwardRef*Builder`s configure a reference to a same-group sibling (defaults: immutable,
//! nullable).

use super::{FieldDef, MemberDef, PendingType, RecGroupBuilder, SuperDef, ValDef};
use crate::value::{ArrayType, FieldType, Finality, FuncType, Mutability, StructType, ValType};

/// Maximum number of fields in a struct (mirrors wasmtime).
const MAX_FIELDS: usize = 10_000;

/// Builder for a struct type within a [`RecGroupBuilder`]; returned by
/// [`RecGroupBuilder::define_struct`]. Call [`finish`](Self::finish) to commit.
pub struct StructTypeBuilder<'a> {
    rec: &'a mut RecGroupBuilder,
    index: u32,
    finality: Finality,
    supertype: Option<SuperDef>,
    fields: Vec<FieldDef>,
}

impl<'a> StructTypeBuilder<'a> {
    pub(super) fn new(rec: &'a mut RecGroupBuilder, index: u32) -> Self {
        StructTypeBuilder {
            rec,
            index,
            finality: Finality::Final,
            supertype: None,
            fields: Vec::new(),
        }
    }

    /// Sets this struct type's finality. Defaults to [`Finality::Final`].
    pub fn finality(&mut self, finality: Finality) -> &mut Self {
        self.finality = finality;
        self
    }

    /// Sets the supertype to an already-registered struct type.
    pub fn supertype(&mut self, supertype: StructType) -> &mut Self {
        let same = supertype.engine().same(&self.rec.engine);
        self.rec.check_engine(same, "supertype");
        self.supertype = Some(SuperDef::Struct(supertype));
        self
    }

    /// Sets the supertype to another struct being defined in the same rec group.
    #[track_caller]
    pub fn forward_supertype(&mut self, supertype: PendingType) -> &mut Self {
        self.rec.check_owns(supertype);
        self.supertype = Some(SuperDef::Forward(supertype.index));
        self
    }

    /// Appends a field whose type is already known (a scalar, an abstract ref, or a reference to
    /// an already-registered type).
    pub fn field(&mut self, ty: FieldType) -> &mut Self {
        self.rec.check_field_engine(&ty, "field type");
        self.fields.push(FieldDef::Registered(ty));
        self
    }

    /// Appends a field referencing another type being defined in the same rec group; configure
    /// it on the returned builder and commit with [`ForwardRefFieldBuilder::finish`].
    #[track_caller]
    pub fn forward_ref_field(&mut self, ty: PendingType) -> ForwardRefFieldBuilder<'_, 'a> {
        self.rec.check_owns(ty);
        ForwardRefFieldBuilder {
            parent: self,
            target: ty,
            mutability: Mutability::Const,
            nullable: true,
        }
    }

    /// Commits this struct definition to the rec group.
    pub fn finish(&mut self) {
        let fields = core::mem::take(&mut self.fields);
        if fields.len() > MAX_FIELDS {
            self.rec.record_error(crate::format_err!(
                "attempted to define a struct type with {} fields, but that is more than the \
                 maximum supported number of fields ({MAX_FIELDS})",
                fields.len(),
            ));
            return;
        }
        self.rec.members[self.index as usize] = Some(MemberDef::Struct {
            finality: self.finality,
            supertype: self.supertype.take(),
            fields,
        });
    }
}

/// Builder for a struct field that forward-references a same-group sibling; created by
/// [`StructTypeBuilder::forward_ref_field`]. Commit with [`finish`](Self::finish).
pub struct ForwardRefFieldBuilder<'p, 'a> {
    parent: &'p mut StructTypeBuilder<'a>,
    target: PendingType,
    mutability: Mutability,
    nullable: bool,
}

impl<'p, 'a> ForwardRefFieldBuilder<'p, 'a> {
    /// Sets the field's mutability. Defaults to [`Mutability::Const`].
    pub fn mutability(mut self, mutability: Mutability) -> Self {
        self.mutability = mutability;
        self
    }

    /// Sets whether the reference is nullable. Defaults to `true`.
    pub fn nullable(mut self, is_nullable: bool) -> Self {
        self.nullable = is_nullable;
        self
    }

    /// Commits this field and returns to the struct builder.
    pub fn finish(self) -> &'p mut StructTypeBuilder<'a> {
        self.parent.fields.push(FieldDef::Forward {
            target: self.target,
            mutable: matches!(self.mutability, Mutability::Var),
            nullable: self.nullable,
        });
        self.parent
    }
}

/// Builder for an array type within a [`RecGroupBuilder`]; returned by
/// [`RecGroupBuilder::define_array`]. The element type must be set via
/// [`element`](Self::element) or [`forward_ref_element`](Self::forward_ref_element);
/// call [`finish`](Self::finish) to commit.
pub struct ArrayTypeBuilder<'a> {
    rec: &'a mut RecGroupBuilder,
    index: u32,
    finality: Finality,
    supertype: Option<SuperDef>,
    element: Option<FieldDef>,
}

impl<'a> ArrayTypeBuilder<'a> {
    pub(super) fn new(rec: &'a mut RecGroupBuilder, index: u32) -> Self {
        ArrayTypeBuilder {
            rec,
            index,
            finality: Finality::Final,
            supertype: None,
            element: None,
        }
    }

    /// Sets this array type's finality. Defaults to [`Finality::Final`].
    pub fn finality(&mut self, finality: Finality) -> &mut Self {
        self.finality = finality;
        self
    }

    /// Sets the supertype to an already-registered array type.
    pub fn supertype(&mut self, supertype: ArrayType) -> &mut Self {
        let same = supertype.engine().same(&self.rec.engine);
        self.rec.check_engine(same, "supertype");
        self.supertype = Some(SuperDef::Array(supertype));
        self
    }

    /// Sets the supertype to another array being defined in the same rec group.
    #[track_caller]
    pub fn forward_supertype(&mut self, supertype: PendingType) -> &mut Self {
        self.rec.check_owns(supertype);
        self.supertype = Some(SuperDef::Forward(supertype.index));
        self
    }

    /// Sets the element type to an already-known type.
    pub fn element(&mut self, ty: FieldType) -> &mut Self {
        self.rec.check_field_engine(&ty, "element type");
        self.element = Some(FieldDef::Registered(ty));
        self
    }

    /// Sets the element type to a reference to another type being defined in the same rec group;
    /// configure it on the returned builder and commit with [`ForwardRefElementBuilder::finish`].
    #[track_caller]
    pub fn forward_ref_element(&mut self, ty: PendingType) -> ForwardRefElementBuilder<'_, 'a> {
        self.rec.check_owns(ty);
        ForwardRefElementBuilder {
            parent: self,
            target: ty,
            mutability: Mutability::Const,
            nullable: true,
        }
    }

    /// Commits this array definition to the rec group.
    pub fn finish(&mut self) {
        let index = self.index as usize;
        let Some(element) = self.element.take() else {
            self.rec.record_error(crate::format_err!(
                "array type {index} was declared but its element type was never set"
            ));
            return;
        };
        self.rec.members[index] = Some(MemberDef::Array {
            finality: self.finality,
            supertype: self.supertype.take(),
            element,
        });
    }
}

/// Builder for an array element that forward-references a same-group sibling; created by
/// [`ArrayTypeBuilder::forward_ref_element`]. Commit with [`finish`](Self::finish).
pub struct ForwardRefElementBuilder<'p, 'a> {
    parent: &'p mut ArrayTypeBuilder<'a>,
    target: PendingType,
    mutability: Mutability,
    nullable: bool,
}

impl<'p, 'a> ForwardRefElementBuilder<'p, 'a> {
    /// Sets the element's mutability. Defaults to [`Mutability::Const`].
    pub fn mutability(mut self, mutability: Mutability) -> Self {
        self.mutability = mutability;
        self
    }

    /// Sets whether the reference is nullable. Defaults to `true`.
    pub fn nullable(mut self, is_nullable: bool) -> Self {
        self.nullable = is_nullable;
        self
    }

    /// Commits this element and returns to the array builder.
    pub fn finish(self) -> &'p mut ArrayTypeBuilder<'a> {
        self.parent.element = Some(FieldDef::Forward {
            target: self.target,
            mutable: matches!(self.mutability, Mutability::Var),
            nullable: self.nullable,
        });
        self.parent
    }
}

/// Builder for a function type within a [`RecGroupBuilder`]; returned by
/// [`RecGroupBuilder::define_func`]. Call [`finish`](Self::finish) to commit.
pub struct FuncTypeBuilder<'a> {
    rec: &'a mut RecGroupBuilder,
    index: u32,
    finality: Finality,
    supertype: Option<SuperDef>,
    params: Vec<ValDef>,
    results: Vec<ValDef>,
}

impl<'a> FuncTypeBuilder<'a> {
    pub(super) fn new(rec: &'a mut RecGroupBuilder, index: u32) -> Self {
        FuncTypeBuilder {
            rec,
            index,
            finality: Finality::Final,
            supertype: None,
            params: Vec::new(),
            results: Vec::new(),
        }
    }

    /// Sets this function type's finality. Defaults to [`Finality::Final`].
    pub fn finality(&mut self, finality: Finality) -> &mut Self {
        self.finality = finality;
        self
    }

    /// Sets the supertype to an already-registered function type.
    pub fn supertype(&mut self, supertype: FuncType) -> &mut Self {
        let same = supertype.engine().same(&self.rec.engine);
        self.rec.check_engine(same, "supertype");
        self.supertype = Some(SuperDef::Func(supertype));
        self
    }

    /// Sets the supertype to another function being defined in the same rec group.
    #[track_caller]
    pub fn forward_supertype(&mut self, supertype: PendingType) -> &mut Self {
        self.rec.check_owns(supertype);
        self.supertype = Some(SuperDef::Forward(supertype.index));
        self
    }

    /// Appends a parameter whose type is already known.
    pub fn param(&mut self, ty: ValType) -> &mut Self {
        self.rec.check_val_engine(&ty, "type");
        self.params.push(ValDef::Registered(ty));
        self
    }

    /// Appends a result whose type is already known.
    pub fn result(&mut self, ty: ValType) -> &mut Self {
        self.rec.check_val_engine(&ty, "type");
        self.results.push(ValDef::Registered(ty));
        self
    }

    /// Appends a parameter referencing another type being defined in the same rec group;
    /// commit with [`ForwardRefFuncValBuilder::finish`]. Defaults to nullable.
    #[track_caller]
    pub fn forward_ref_param(&mut self, ty: PendingType) -> ForwardRefFuncValBuilder<'_, 'a> {
        self.rec.check_owns(ty);
        ForwardRefFuncValBuilder {
            parent: self,
            target: ty,
            nullable: true,
            is_result: false,
        }
    }

    /// Appends a result referencing another type being defined in the same rec group;
    /// commit with [`ForwardRefFuncValBuilder::finish`]. Defaults to nullable.
    #[track_caller]
    pub fn forward_ref_result(&mut self, ty: PendingType) -> ForwardRefFuncValBuilder<'_, 'a> {
        self.rec.check_owns(ty);
        ForwardRefFuncValBuilder {
            parent: self,
            target: ty,
            nullable: true,
            is_result: true,
        }
    }

    /// Commits this function definition to the rec group.
    pub fn finish(&mut self) {
        self.rec.members[self.index as usize] = Some(MemberDef::Func {
            finality: self.finality,
            supertype: self.supertype.take(),
            params: core::mem::take(&mut self.params),
            results: core::mem::take(&mut self.results),
        });
    }
}

/// Builder for a function parameter or result that forward-references a same-group sibling;
/// created by [`FuncTypeBuilder::forward_ref_param`] / [`FuncTypeBuilder::forward_ref_result`].
/// Commit with [`finish`](Self::finish).
pub struct ForwardRefFuncValBuilder<'p, 'a> {
    parent: &'p mut FuncTypeBuilder<'a>,
    target: PendingType,
    nullable: bool,
    is_result: bool,
}

impl<'p, 'a> ForwardRefFuncValBuilder<'p, 'a> {
    /// Sets whether the reference is nullable. Defaults to `true`.
    pub fn nullable(mut self, is_nullable: bool) -> Self {
        self.nullable = is_nullable;
        self
    }

    /// Commits this parameter/result and returns to the function builder.
    pub fn finish(self) -> &'p mut FuncTypeBuilder<'a> {
        let val = ValDef::Forward {
            target: self.target,
            nullable: self.nullable,
        };
        if self.is_result {
            self.parent.results.push(val);
        } else {
            self.parent.params.push(val);
        }
        self.parent
    }
}

macro_rules! builder_debug {
    ($ty:ident) => {
        impl core::fmt::Debug for $ty<'_> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("index", &self.index)
                    .finish_non_exhaustive()
            }
        }
    };
}
macro_rules! forward_ref_debug {
    ($ty:ident) => {
        impl core::fmt::Debug for $ty<'_, '_> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("target", &self.target)
                    .finish_non_exhaustive()
            }
        }
    };
}
builder_debug!(StructTypeBuilder);
builder_debug!(ArrayTypeBuilder);
builder_debug!(FuncTypeBuilder);
forward_ref_debug!(ForwardRefFieldBuilder);
forward_ref_debug!(ForwardRefElementBuilder);
forward_ref_debug!(ForwardRefFuncValBuilder);
