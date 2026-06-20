//! `RecGroupBuilder` — the embedder API for declaring a whole recursion group of GC types at once
//! (mirrors wasmtime PR #13687). Self-referential / mutually-recursive host types can't be built
//! with `StructType::new` alone (a type would need its own id before it exists); here you
//! `declare_*` to get a kind-typed label, use it as a forward reference while `define_*`-ing
//! siblings, and `build()` registers the whole group in one interning step.
//!
//! The public `*Template` types ([`template`]) mirror `HeapType`/`ValType`/`StorageType`/
//! `FieldType` but can also hold a pending label; [`lower`] turns them into module IR.

mod lower;
mod template;

pub use template::{
    ArraySuperType, FieldTemplate, FuncSuperType, HeapTypeTemplate, PendingArrayId, PendingFuncId,
    PendingStructId, StorageTypeTemplate, StructSuperType, ValTypeTemplate,
};

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::canon::{AggKind, CanonicalTypeId, ModuleType};
use crate::engine::Engine;
use crate::value::{ArrayType, Finality, FuncType, StructType};
use crate::{Error, Result};

/// Distinguishes labels from different builders so a label can't be used with the wrong group.
static NEXT_BUILDER_ID: AtomicUsize = AtomicUsize::new(0);

/// One member definition accumulated by the builder.
enum MemberDef {
    Struct {
        finality: Finality,
        supertype: Option<StructSuperType>,
        fields: Vec<FieldTemplate>,
    },
    Array {
        finality: Finality,
        supertype: Option<ArraySuperType>,
        field: FieldTemplate,
    },
    Func {
        finality: Finality,
        supertype: Option<FuncSuperType>,
        params: Vec<ValTypeTemplate>,
        results: Vec<ValTypeTemplate>,
    },
}

/// Builds a whole recursion group of GC types, allowing forward references between members.
pub struct RecGroupBuilder {
    engine: Engine,
    builder_id: usize,
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
            members: Vec::new(),
        }
    }

    fn declare(&mut self) -> u32 {
        let index = u32::try_from(self.members.len()).expect("too many types in a rec group");
        self.members.push(None);
        index
    }

    /// Reserves a struct slot, returning a label usable as a forward reference.
    pub fn declare_struct(&mut self) -> PendingStructId {
        PendingStructId::new(self.builder_id, self.declare())
    }

    /// Reserves an array slot.
    pub fn declare_array(&mut self) -> PendingArrayId {
        PendingArrayId::new(self.builder_id, self.declare())
    }

    /// Reserves a function-type slot.
    pub fn declare_func(&mut self) -> PendingFuncId {
        PendingFuncId::new(self.builder_id, self.declare())
    }

    /// Defines a previously-declared struct (final, no supertype).
    pub fn define_struct(
        &mut self,
        id: PendingStructId,
        fields: impl IntoIterator<Item = FieldTemplate>,
    ) -> &mut Self {
        self.define_struct_with_finality_and_supertype(id, Finality::Final, None, fields)
    }

    /// Defines a previously-declared struct with explicit finality + optional supertype.
    pub fn define_struct_with_finality_and_supertype(
        &mut self,
        id: PendingStructId,
        finality: Finality,
        supertype: Option<StructSuperType>,
        fields: impl IntoIterator<Item = FieldTemplate>,
    ) -> &mut Self {
        assert_eq!(id.builder_id, self.builder_id, "label from another builder");
        self.members[id.index as usize] = Some(MemberDef::Struct {
            finality,
            supertype,
            fields: fields.into_iter().collect(),
        });
        self
    }

    /// Declares + defines a struct in one step.
    pub fn add_struct(
        &mut self,
        fields: impl IntoIterator<Item = FieldTemplate>,
    ) -> PendingStructId {
        let id = self.declare_struct();
        self.define_struct(id, fields);
        id
    }

    /// Defines a previously-declared array (final, no supertype).
    pub fn define_array(&mut self, id: PendingArrayId, field: FieldTemplate) -> &mut Self {
        self.define_array_with_finality_and_supertype(id, Finality::Final, None, field)
    }

    /// Defines a previously-declared array with explicit finality + optional supertype.
    pub fn define_array_with_finality_and_supertype(
        &mut self,
        id: PendingArrayId,
        finality: Finality,
        supertype: Option<ArraySuperType>,
        field: FieldTemplate,
    ) -> &mut Self {
        assert_eq!(id.builder_id, self.builder_id, "label from another builder");
        self.members[id.index as usize] = Some(MemberDef::Array {
            finality,
            supertype,
            field,
        });
        self
    }

    /// Declares + defines an array in one step.
    pub fn add_array(&mut self, field: FieldTemplate) -> PendingArrayId {
        let id = self.declare_array();
        self.define_array(id, field);
        id
    }

    /// Defines a previously-declared function type (final, no supertype).
    pub fn define_func(
        &mut self,
        id: PendingFuncId,
        params: impl IntoIterator<Item = ValTypeTemplate>,
        results: impl IntoIterator<Item = ValTypeTemplate>,
    ) -> &mut Self {
        self.define_func_with_finality_and_supertype(id, Finality::Final, None, params, results)
    }

    /// Defines a previously-declared function type with explicit finality + optional supertype.
    pub fn define_func_with_finality_and_supertype(
        &mut self,
        id: PendingFuncId,
        finality: Finality,
        supertype: Option<FuncSuperType>,
        params: impl IntoIterator<Item = ValTypeTemplate>,
        results: impl IntoIterator<Item = ValTypeTemplate>,
    ) -> &mut Self {
        assert_eq!(id.builder_id, self.builder_id, "label from another builder");
        self.members[id.index as usize] = Some(MemberDef::Func {
            finality,
            supertype,
            params: params.into_iter().collect(),
            results: results.into_iter().collect(),
        });
        self
    }

    /// Declares + defines a function type in one step.
    pub fn add_func(
        &mut self,
        params: impl IntoIterator<Item = ValTypeTemplate>,
        results: impl IntoIterator<Item = ValTypeTemplate>,
    ) -> PendingFuncId {
        let id = self.declare_func();
        self.define_func(id, params, results);
        id
    }

    /// Registers the whole group: lowers each member to module IR (sibling labels → relative
    /// concrete refs, already-registered types → an externals table) and interns it.
    pub fn build(self) -> Result<RecGroup> {
        let n = self.members.len();
        if n == 0 {
            return Err(Error::msg("a rec group must contain at least one type"));
        }
        let mut externals = Vec::new();
        let mut members = Vec::with_capacity(n);
        let mut kinds = Vec::with_capacity(n);
        for (i, m) in self.members.iter().enumerate() {
            let def = m
                .as_ref()
                .ok_or_else(|| Error::msg(format!("type {i} was declared but never defined")))?;
            let (finality, supertype, body, kind) = lower::lower(def, n, &mut externals);
            members.push(ModuleType {
                group: 0,
                finality,
                supertype,
                body,
            });
            kinds.push(kind);
        }
        let ids = self.engine.intern_host_group(&members, &externals);
        Ok(RecGroup {
            engine: self.engine,
            builder_id: self.builder_id,
            ids,
            kinds,
        })
    }
}

/// One member of a built [`RecGroup`].
#[derive(Clone, Debug)]
pub enum CompositeType {
    Struct(StructType),
    Array(ArrayType),
    Func(FuncType),
}

/// A registered recursion group: the canonical types produced by [`RecGroupBuilder::build`].
#[derive(Clone, Debug)]
pub struct RecGroup {
    engine: Engine,
    builder_id: usize,
    ids: Vec<CanonicalTypeId>,
    kinds: Vec<AggKind>,
}

impl RecGroup {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The registered struct type for label `id`.
    pub fn struct_(&self, id: PendingStructId) -> StructType {
        let i = self.check(id.builder_id, id.index, AggKind::Struct);
        StructType::from_id(&self.engine, self.ids[i])
    }

    /// The registered array type for label `id`.
    pub fn array(&self, id: PendingArrayId) -> ArrayType {
        let i = self.check(id.builder_id, id.index, AggKind::Array);
        ArrayType::from_id(&self.engine, self.ids[i])
    }

    /// The registered function type for label `id`.
    pub fn func(&self, id: PendingFuncId) -> FuncType {
        let i = self.check(id.builder_id, id.index, AggKind::Func);
        FuncType::from_id(&self.engine, self.ids[i])
    }

    /// Every member of the group, in declaration order.
    pub fn types(&self) -> impl ExactSizeIterator<Item = CompositeType> + '_ {
        self.ids
            .iter()
            .zip(&self.kinds)
            .map(|(&id, &kind)| match kind {
                AggKind::Struct => CompositeType::Struct(StructType::from_id(&self.engine, id)),
                AggKind::Array => CompositeType::Array(ArrayType::from_id(&self.engine, id)),
                AggKind::Func => CompositeType::Func(FuncType::from_id(&self.engine, id)),
            })
    }

    fn check(&self, builder_id: usize, index: u32, kind: AggKind) -> usize {
        assert_eq!(builder_id, self.builder_id, "label from another builder");
        let i = index as usize;
        assert_eq!(self.kinds[i], kind, "label kind mismatch");
        i
    }
}
