//! Host construction + inspection of exception objects (`exnref`), mirroring the GC host API
//! (`StructRef`/`StructRefPre`). An exception instance is `ExnEntity { tag, args }` in the store's
//! exn arena (#28b); `Rooted<ExnRef>` is a handle into it.

use crate::extern_::{val_matches, Tag};
use crate::store::{AsContext, AsContextMut, ExnEntity};
use crate::value::gc_ref::{ExnRef, Rooted};
use crate::value::{ExnType, HeapType, Val, ValType};
use crate::{Error, Result};

/// Pre-allocation handle for [`ExnRef::new`]: holds the [`ExnType`] (a registration keeping the type
/// alive) so repeated allocations amortize the type lookup — wasmtime's `*Pre` purpose.
#[derive(Debug)]
pub struct ExnRefPre {
    ty: ExnType,
}

impl ExnRefPre {
    pub fn new(store: impl AsContextMut, ty: ExnType) -> Self {
        let _ = store; // no rooting/registration needed under the null collector
        ExnRefPre { ty }
    }
}

impl ExnRef {
    /// Allocates an exception object for `tag` with `fields` (which must match the tag's parameter
    /// types in count and kind).
    pub fn new(
        mut store: impl AsContextMut,
        allocator: &ExnRefPre,
        tag: &Tag,
        fields: &[Val],
    ) -> Result<Rooted<ExnRef>> {
        let params: Vec<ValType> = allocator.ty.func().params().collect();
        if fields.len() != params.len() {
            return Err(Error::msg("wrong number of exception fields"));
        }
        for (v, ty) in fields.iter().zip(&params) {
            if !val_matches(v, ty) {
                return Err(Error::msg("exception field value has the wrong type"));
            }
        }
        Ok(store.as_context_mut().inner_mut().alloc_exn(ExnEntity {
            tag: *tag,
            args: fields.to_vec(),
        }))
    }
}

impl Rooted<ExnRef> {
    /// Reads argument `index`.
    pub fn field(&self, mut store: impl AsContextMut, index: usize) -> Result<Val> {
        let ctx = store.as_context_mut();
        ctx.inner()
            .exn(*self)
            .args
            .get(index)
            .copied()
            .ok_or_else(|| Error::msg("exception field index out of bounds"))
    }

    /// The tag this exception was thrown with.
    pub fn tag(&self, mut store: impl AsContextMut) -> Result<Tag> {
        Ok(store.as_context_mut().inner().exn(*self).tag)
    }

    /// The exception's type (its tag's signature).
    pub fn ty(&self, store: impl AsContext) -> Result<ExnType> {
        let ctx = store.as_context();
        let tag = ctx.inner().exn(*self).tag;
        ExnType::from_tag_type(&ctx.inner().tag(tag).ty)
    }

    /// Whether this `exnref`'s heap type is a subtype of `ty`.
    pub fn matches_ty(&self, store: impl AsContext, ty: &HeapType) -> Result<bool> {
        let _ = store.as_context();
        Ok(HeapType::Exn.matches(ty))
    }
}
