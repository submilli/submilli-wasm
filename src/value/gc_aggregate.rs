//! GC aggregate references: `structref`/`arrayref` (and their `*Pre` allocators). Split out of
//! [`gc_ref`](super::gc_ref) to stay under the file-size cap; the core handle types (`Rooted`,
//! `AnyRef`) and the shared heap-slot helpers live there.

use crate::canon::Layout;
use crate::store::{
    anyref_handle_slot, read_slot, slot_accepts, write_slot, AsContext, AsContextMut, GcObject,
};
use crate::value::gc_type::{ArrayType, StructType};
use crate::value::{Mutability, Val};
use crate::{Error, Result};

use super::gc_ref::{gc_object, gc_slot, AnyRef, Rooted};

/// A GC struct instance (`structref`).
#[derive(Debug)]
pub struct StructRef {
    _private: (),
}

/// Pre-allocation handle for [`StructRef::new`]: holds the `StructType` (a registration, so the
/// type stays alive until allocation) plus its cached packed layout (amortizing the lookup —
/// wasmtime's `*Pre` purpose).
#[derive(Debug)]
pub struct StructRefPre {
    ty: StructType,
    layout: Layout,
}

impl StructRefPre {
    pub fn new(store: impl AsContextMut, ty: StructType) -> Self {
        let _ = store; // no rooting/registration needed under the null collector
        let fields: Vec<_> = ty.fields().collect();
        StructRefPre {
            ty,
            layout: Layout::for_struct(&fields),
        }
    }
}

impl StructRef {
    /// Allocates a struct from `allocator` with the given field values (which must match the
    /// type's fields in count and kind).
    pub fn new(
        mut store: impl AsContextMut,
        allocator: &StructRefPre,
        fields: &[Val],
    ) -> Result<Rooted<StructRef>> {
        let Layout::Struct {
            fields: slots,
            size,
        } = &allocator.layout
        else {
            unreachable!("struct pre carries a struct layout");
        };
        if fields.len() != slots.len() {
            return Err(Error::msg("wrong number of struct fields"));
        }
        for (slot, v) in slots.iter().zip(fields) {
            if !slot_accepts(*slot, v) {
                return Err(Error::msg("struct field value has the wrong type"));
            }
        }
        let mut data = vec![0u8; *size];
        for (slot, v) in slots.iter().zip(fields) {
            write_slot(*slot, &mut data, *v);
        }
        let type_id = allocator.ty.canonical_id();
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        inner.gc_check_capacity(*size)?;
        inner.pin_gc_type(type_id); // keep the type alive for the object's (store) lifetime
        let idx = inner.alloc_gc(GcObject::new_struct(type_id, data.into_boxed_slice()))?;
        Ok(Rooted::from_raw(anyref_handle_slot(idx)))
    }
}

impl Rooted<StructRef> {
    /// Reads field `index`.
    pub fn field(&self, store: impl AsContext, index: usize) -> Result<Val> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = gc_slot(inner, self.raw())?;
        let obj = gc_object(inner, slot)?;
        let layout = Layout::for_struct(&inner.engine().struct_fields(obj.header.type_id));
        let field = layout
            .get_field(index)
            .ok_or_else(|| Error::msg("struct field index out of bounds"))?;
        Ok(read_slot(field, &obj.data))
    }

    /// This struct's type.
    pub fn ty(&self, store: impl AsContext) -> Result<StructType> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = gc_slot(inner, self.raw())?;
        let type_id = gc_object(inner, slot)?.header.type_id;
        Ok(StructType::from_id(inner.engine(), type_id))
    }

    /// Whether this struct's type is a subtype of `ty`. Mirrors `wasmtime::StructRef::matches_ty`.
    pub fn matches_ty(&self, store: impl AsContext, ty: &StructType) -> Result<bool> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = gc_slot(inner, self.raw())?;
        let type_id = gc_object(inner, slot)?.header.type_id;
        Ok(inner.engine().is_subtype(type_id, ty.canonical_id()))
    }

    /// Writes `value` to field `index`. Errors if the field is out of bounds, immutable, or
    /// `value` doesn't match the field's type.
    pub fn set_field(&self, mut store: impl AsContextMut, index: usize, value: Val) -> Result<()> {
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        let slot = gc_slot(inner, self.raw())?;
        let type_id = gc_object(inner, slot)?.header.type_id;
        let fields = inner.engine().struct_fields(type_id);
        let field_ty = fields
            .get(index)
            .ok_or_else(|| Error::msg("struct field index out of bounds"))?;
        if field_ty.mutability() != Mutability::Var {
            return Err(Error::msg("struct field is not mutable"));
        }
        let field = Layout::for_struct(&fields).field(index);
        if !slot_accepts(field, &value) {
            return Err(Error::msg("struct field value has the wrong type"));
        }
        let obj = inner
            .gc_object_mut(slot)
            .ok_or_else(|| Error::msg("dangling gc reference"))?;
        write_slot(field, &mut obj.data, value);
        Ok(())
    }

    /// Upcasts this `structref` to an `anyref`.
    pub fn to_anyref(self) -> Rooted<AnyRef> {
        Rooted::from_raw(self.raw())
    }
}

impl From<Rooted<StructRef>> for Rooted<AnyRef> {
    fn from(r: Rooted<StructRef>) -> Self {
        Rooted::from_raw(r.raw())
    }
}

/// A GC array instance (`arrayref`).
#[derive(Debug)]
pub struct ArrayRef {
    _private: (),
}

/// Pre-allocation handle for [`ArrayRef::new`]: holds the `ArrayType` (a registration) + cached
/// element layout (like [`StructRefPre`]).
#[derive(Debug)]
pub struct ArrayRefPre {
    ty: ArrayType,
    layout: Layout,
}

impl ArrayRefPre {
    pub fn new(store: impl AsContextMut, ty: ArrayType) -> Self {
        let _ = store;
        let layout = Layout::for_array(&ty.field_type());
        ArrayRefPre { ty, layout }
    }
}

impl ArrayRef {
    /// Allocates an array of `len` copies of `elem`.
    pub fn new(
        store: impl AsContextMut,
        allocator: &ArrayRefPre,
        elem: &Val,
        len: u32,
    ) -> Result<Rooted<ArrayRef>> {
        let count = len as usize;
        Self::alloc(store, allocator, count, |_| elem)
    }

    /// Allocates a fixed array from the given elements.
    pub fn new_fixed(
        store: impl AsContextMut,
        allocator: &ArrayRefPre,
        elems: &[Val],
    ) -> Result<Rooted<ArrayRef>> {
        Self::alloc(store, allocator, elems.len(), |i| &elems[i])
    }

    /// Shared array constructor: validates each element, packs the body, allocates.
    fn alloc<'a>(
        mut store: impl AsContextMut,
        allocator: &ArrayRefPre,
        count: usize,
        elem_at: impl Fn(usize) -> &'a Val,
    ) -> Result<Rooted<ArrayRef>> {
        let stride = allocator.layout.stride();
        let byte_len = count
            .checked_mul(stride)
            .ok_or_else(|| Error::msg("array too large"))?;
        for i in 0..count {
            if !slot_accepts(allocator.layout.elem_at(0), elem_at(i)) {
                return Err(Error::msg("array element value has the wrong type"));
            }
        }
        let mut data = vec![0u8; byte_len];
        for i in 0..count {
            write_slot(allocator.layout.elem_at(i), &mut data, *elem_at(i));
        }
        let type_id = allocator.ty.canonical_id();
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        inner.gc_check_capacity(byte_len)?;
        inner.pin_gc_type(type_id);
        let idx = inner.alloc_gc(GcObject::new_array(
            type_id,
            count as u32,
            data.into_boxed_slice(),
        ))?;
        Ok(Rooted::from_raw(anyref_handle_slot(idx)))
    }
}

impl Rooted<ArrayRef> {
    /// The number of elements.
    pub fn len(&self, store: impl AsContext) -> Result<u32> {
        let ctx = store.as_context();
        let slot = gc_slot(ctx.inner(), self.raw())?;
        Ok(gc_object(ctx.inner(), slot)?.header.len)
    }

    /// Reads element `index`.
    pub fn get(&self, store: impl AsContext, index: u32) -> Result<Val> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = gc_slot(inner, self.raw())?;
        let obj = gc_object(inner, slot)?;
        if index >= obj.header.len {
            return Err(Error::msg("array index out of bounds"));
        }
        let layout = Layout::for_array(&inner.engine().array_field(obj.header.type_id));
        Ok(read_slot(layout.elem_at(index as usize), &obj.data))
    }

    /// Writes `value` to element `index`. Errors if the index is out of bounds, the element type
    /// is immutable, or `value` doesn't match it.
    pub fn set(&self, mut store: impl AsContextMut, index: u32, value: Val) -> Result<()> {
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        let slot = gc_slot(inner, self.raw())?;
        let (type_id, len) = {
            let obj = gc_object(inner, slot)?;
            (obj.header.type_id, obj.header.len)
        };
        if index >= len {
            return Err(Error::msg("array index out of bounds"));
        }
        let field_ty = inner.engine().array_field(type_id);
        if field_ty.mutability() != Mutability::Var {
            return Err(Error::msg("array element is not mutable"));
        }
        let elem = Layout::for_array(&field_ty).elem_at(index as usize);
        if !slot_accepts(elem, &value) {
            return Err(Error::msg("array element value has the wrong type"));
        }
        let obj = inner
            .gc_object_mut(slot)
            .ok_or_else(|| Error::msg("dangling gc reference"))?;
        write_slot(elem, &mut obj.data, value);
        Ok(())
    }

    /// Upcasts this `arrayref` to an `anyref`.
    pub fn to_anyref(self) -> Rooted<AnyRef> {
        Rooted::from_raw(self.raw())
    }
}

impl From<Rooted<ArrayRef>> for Rooted<AnyRef> {
    fn from(r: Rooted<ArrayRef>) -> Self {
        Rooted::from_raw(r.raw())
    }
}
