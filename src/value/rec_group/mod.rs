//! `RecGroupBuilder` — the embedder API for declaring a whole recursion group of GC types at once
//! (mirrors wasmtime PR #13687). Self-referential / mutually-recursive host types can't be built
//! with `StructType::new` alone (a type would need its own id before it exists); here you
//! `declare_*` to get a [`PendingType`] handle, use it as a forward reference while defining
//! siblings (via the `forward_ref_*` builder methods), and `build()` validates and registers the
//! whole group in one interning step.
//!
//! Each type is defined via a nested builder (e.g. [`RecGroupBuilder::define_struct`]) and
//! committed by calling `finish` on that builder; a definition never finished is treated as
//! though the type was never defined. Already-registered types (and abstract heap types) are
//! used directly via the normal [`FieldType`](crate::FieldType)/[`ValType`](crate::ValType)
//! APIs; the `forward_ref_*` methods are only for references to same-group siblings.
//!
//! The order of `declare_*` calls fixes the members' order within the rec group, which is
//! semantically significant: two groups with the same types in a different order are distinct.

mod builders;
mod lower;
mod validate;

pub use builders::{
    ArrayTypeBuilder, ForwardRefElementBuilder, ForwardRefFieldBuilder, ForwardRefFuncValBuilder,
    FuncTypeBuilder, StructTypeBuilder,
};

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::canon::{AggKind, CanonicalTypeId, GroupId, ModuleType};
use crate::engine::Engine;
use crate::value::{
    ArrayType, FieldType, Finality, FuncType, HeapType, StorageType, StructType, ValType,
};
use crate::{Error, Result};

/// Distinguishes handles from different builders so a handle can't be used with the wrong group.
static NEXT_BUILDER_ID: AtomicUsize = AtomicUsize::new(0);

/// A handle to a type being defined in a [`RecGroupBuilder`].
///
/// Obtained from [`RecGroupBuilder::declare_struct`] and friends; used both to define the type
/// (via [`RecGroupBuilder::define_struct`] and friends) and to forward-reference it from sibling
/// definitions (via the `forward_ref_*` builder methods). It records the kind it was declared
/// as, so a forward reference lowers to the right concrete heap type before the target's body
/// is defined.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct PendingType {
    builder_id: usize,
    index: u32,
    kind: AggKind,
}

/// A struct field / array element accumulated by a builder: a registered [`FieldType`] or a
/// forward reference to a sibling.
enum FieldDef {
    Registered(FieldType),
    Forward {
        target: PendingType,
        mutable: bool,
        nullable: bool,
    },
}

/// A function param/result accumulated by a builder.
enum ValDef {
    Registered(ValType),
    Forward { target: PendingType, nullable: bool },
}

/// A member's declared supertype: a sibling (by group index) or an already-registered type.
enum SuperDef {
    Forward(u32),
    Struct(StructType),
    Array(ArrayType),
    Func(FuncType),
}

/// One member definition committed by a nested builder's `finish`.
enum MemberDef {
    Struct {
        finality: Finality,
        supertype: Option<SuperDef>,
        fields: Vec<FieldDef>,
    },
    Array {
        finality: Finality,
        supertype: Option<SuperDef>,
        element: FieldDef,
    },
    Func {
        finality: Finality,
        supertype: Option<SuperDef>,
        params: Vec<ValDef>,
        results: Vec<ValDef>,
    },
}

impl MemberDef {
    fn supertype(&self) -> Option<&SuperDef> {
        match self {
            MemberDef::Struct { supertype, .. }
            | MemberDef::Array { supertype, .. }
            | MemberDef::Func { supertype, .. } => supertype.as_ref(),
        }
    }
}

/// Builds a whole recursion group of GC types, allowing forward references between members.
pub struct RecGroupBuilder {
    engine: Engine,
    builder_id: usize,
    /// The first error encountered while adding types (e.g. a type from a different engine).
    /// Surfaced by [`build`](Self::build); the chaining builder methods stay infallible.
    error: Option<Error>,
    /// The committed definition of each member, or `None` if its builder never `finish`ed.
    members: Vec<Option<MemberDef>>,
}

impl core::fmt::Debug for RecGroupBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RecGroupBuilder")
            .field("members", &self.members.len())
            .finish_non_exhaustive()
    }
}

impl RecGroupBuilder {
    pub fn new(engine: &Engine) -> Self {
        RecGroupBuilder {
            engine: engine.clone(),
            builder_id: NEXT_BUILDER_ID.fetch_add(1, Ordering::Relaxed),
            error: None,
            members: Vec::new(),
        }
    }

    fn declare(&mut self, kind: AggKind) -> PendingType {
        let index = u32::try_from(self.members.len()).expect("too many types in a rec group");
        self.members.push(None);
        PendingType {
            builder_id: self.builder_id,
            index,
            kind,
        }
    }

    /// Declares a struct slot, returning a handle usable as a forward reference.
    pub fn declare_struct(&mut self) -> PendingType {
        self.declare(AggKind::Struct)
    }

    /// Declares an array slot.
    pub fn declare_array(&mut self) -> PendingType {
        self.declare(AggKind::Array)
    }

    /// Declares a function-type slot.
    pub fn declare_func(&mut self) -> PendingType {
        self.declare(AggKind::Func)
    }

    #[track_caller]
    fn check_owns(&self, ty: PendingType) {
        assert_eq!(
            ty.builder_id, self.builder_id,
            "`PendingType` used with a different `RecGroupBuilder` than it came from"
        );
    }

    /// Records an error to be surfaced by [`build`](Self::build); only the first is kept.
    fn record_error(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    fn check_engine(&mut self, same_engine: bool, what: &str) {
        if !same_engine {
            self.record_error(crate::format_err!(
                "{what} is associated with a different engine"
            ));
        }
    }

    /// Records an error unless every concrete type reachable from `ty` belongs to this engine.
    fn check_field_engine(&mut self, ty: &FieldType, what: &str) {
        if let StorageType::ValType(v) = ty.element_type() {
            self.check_val_engine(v, what);
        }
    }

    fn check_val_engine(&mut self, ty: &ValType, what: &str) {
        let same = match ty {
            ValType::Ref(rt) => match rt.heap_type() {
                HeapType::ConcreteStruct(t) => t.engine().same(&self.engine),
                HeapType::ConcreteArray(t) => t.engine().same(&self.engine),
                HeapType::ConcreteFunc(t) => t.engine().same(&self.engine),
                _ => true,
            },
            _ => true,
        };
        self.check_engine(same, what);
    }

    /// Begins defining `ty` as a struct; commit with [`StructTypeBuilder::finish`] (committing
    /// replaces any previous definition).
    ///
    /// # Panics
    ///
    /// Panics if the handle came from another builder or was not declared via
    /// [`declare_struct`](Self::declare_struct).
    #[track_caller]
    pub fn define_struct(&mut self, ty: PendingType) -> StructTypeBuilder<'_> {
        self.check_owns(ty);
        assert_eq!(
            ty.kind,
            AggKind::Struct,
            "handle was not declared as a struct type"
        );
        StructTypeBuilder::new(self, ty.index)
    }

    /// Begins defining `ty` as an array; commit with [`ArrayTypeBuilder::finish`].
    ///
    /// # Panics
    ///
    /// Panics if the handle came from another builder or was not declared via
    /// [`declare_array`](Self::declare_array).
    #[track_caller]
    pub fn define_array(&mut self, ty: PendingType) -> ArrayTypeBuilder<'_> {
        self.check_owns(ty);
        assert_eq!(
            ty.kind,
            AggKind::Array,
            "handle was not declared as an array type"
        );
        ArrayTypeBuilder::new(self, ty.index)
    }

    /// Begins defining `ty` as a function type; commit with [`FuncTypeBuilder::finish`].
    ///
    /// # Panics
    ///
    /// Panics if the handle came from another builder or was not declared via
    /// [`declare_func`](Self::declare_func).
    #[track_caller]
    pub fn define_func(&mut self, ty: PendingType) -> FuncTypeBuilder<'_> {
        self.check_owns(ty);
        assert_eq!(
            ty.kind,
            AggKind::Func,
            "handle was not declared as a function type"
        );
        FuncTypeBuilder::new(self, ty.index)
    }

    /// Registers the whole group: lowers each member to module IR (sibling handles → relative
    /// concrete refs, already-registered types → an externals table), interns it, and validates
    /// declared supertypes. An empty group is allowed.
    pub fn build(self) -> Result<RecGroup> {
        let RecGroupBuilder {
            engine,
            builder_id,
            error,
            members,
        } = self;
        if let Some(error) = error {
            return Err(error);
        }

        let n = members.len();
        let mut defs = Vec::with_capacity(n);
        for (i, m) in members.into_iter().enumerate() {
            defs.push(
                m.ok_or_else(|| Error::msg(format!("type {i} was declared but never defined")))?,
            );
        }

        let mut externals = Vec::new();
        let mut lowered = Vec::with_capacity(n);
        let mut kinds = Vec::with_capacity(n);
        for def in &defs {
            let (finality, supertype, body, kind) = lower::lower(def, n, &mut externals);
            lowered.push(ModuleType {
                group: 0,
                finality,
                supertype,
                body,
            });
            kinds.push(kind);
        }

        // The group is interned with one registration, which this `RecGroup` adopts (released on
        // drop). Extracted `StructType`/etc. handles take their own registrations via `from_id`.
        let (ids, group) = engine.intern_host_group(&lowered, &externals);
        let group = RecGroup {
            engine: engine.clone(),
            builder_id,
            group,
            ids,
            kinds,
        };

        // Supertypes are validated only now, once forward references resolve to registered
        // types. On failure `group` is dropped, which releases the registration.
        for (i, def) in defs.iter().enumerate() {
            validate::supertype(&engine, &group, i, def.supertype())?;
        }
        Ok(group)
    }
}

/// A registered recursion group: the canonical types produced by [`RecGroupBuilder::build`].
/// Holds one registration on the group (released on `Drop`), keeping all its member types alive
/// while it lives; types retrieved via the getters hold their own registrations.
#[derive(Debug)]
pub struct RecGroup {
    engine: Engine,
    builder_id: usize,
    group: GroupId,
    ids: Vec<CanonicalTypeId>,
    kinds: Vec<AggKind>,
}

impl Drop for RecGroup {
    fn drop(&mut self) {
        self.engine.release_group(self.group);
    }
}

impl RecGroup {
    /// The number of types in this rec group.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether this rec group was built without declaring any types.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    #[track_caller]
    fn index_of(&self, ty: PendingType) -> usize {
        assert_eq!(
            ty.builder_id, self.builder_id,
            "`PendingType` used with a different `RecGroup` than it came from"
        );
        ty.index as usize
    }

    /// The struct type for the given handle, or `None` if it was declared as a different kind.
    pub fn get_struct(&self, ty: PendingType) -> Option<StructType> {
        let i = self.index_of(ty);
        (self.kinds[i] == AggKind::Struct).then(|| StructType::from_id(&self.engine, self.ids[i]))
    }

    /// The array type for the given handle, or `None` if it was declared as a different kind.
    pub fn get_array(&self, ty: PendingType) -> Option<ArrayType> {
        let i = self.index_of(ty);
        (self.kinds[i] == AggKind::Array).then(|| ArrayType::from_id(&self.engine, self.ids[i]))
    }

    /// The function type for the given handle, or `None` if it was declared as a different kind.
    pub fn get_func(&self, ty: PendingType) -> Option<FuncType> {
        let i = self.index_of(ty);
        (self.kinds[i] == AggKind::Func).then(|| FuncType::from_id(&self.engine, self.ids[i]))
    }

    /// Every member of the group in declaration order, each as a concrete [`HeapType`].
    pub fn types(&self) -> impl ExactSizeIterator<Item = HeapType> + '_ {
        self.ids
            .iter()
            .zip(&self.kinds)
            .map(|(&id, &kind)| match kind {
                AggKind::Struct => HeapType::ConcreteStruct(StructType::from_id(&self.engine, id)),
                AggKind::Array => HeapType::ConcreteArray(ArrayType::from_id(&self.engine, id)),
                AggKind::Func => HeapType::ConcreteFunc(FuncType::from_id(&self.engine, id)),
            })
    }
}
