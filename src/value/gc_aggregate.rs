//! GC aggregate references: `structref`/`arrayref` (and their `*Pre` allocators). Split out of
//! [`gc_ref`](super::gc_ref) to stay under the file-size cap; the core handle types (`Rooted`,
//! `AnyRef`) and the shared heap-slot helpers live there.

use crate::canon::{Layout, RefKind};
use crate::store::{
    anyref_handle_slot, read_slot, slot_accepts, write_slot, AsContext, AsContextMut, GcObject,
    StoreInner,
};
use crate::value::gc_type::{ArrayType, StorageType, StructType};
use crate::value::{Mutability, Val};
use crate::{Error, Result};

use super::gc_ref::{gc_object, AnyRef, Rooted};

/// Wraps a freshly-allocated host GC object's slot as a `Rooted`: registers it as a host root (so a
/// guest collection across this handle keeps it alive) and captures the slot generation (so a
/// stale handle faults if the slot is later collected and reused — #27g).
fn root_new_gc<T>(inner: &mut StoreInner, idx: u32) -> Rooted<T> {
    let handle = anyref_handle_slot(idx);
    inner.push_gc_root(handle, RefKind::Any);
    let generation = inner.gc.generation(idx).unwrap_or(0);
    Rooted::from_raw_gen(handle, generation)
}

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
        let type_id = allocator.ty.canonical_id();
        let mut ctx = store.as_context_mut();
        // Reserve through the limiter (collect-then-grow) before building the body — the field
        // values are host-held `Val`s (rooted if they are GC refs), so a collection here is safe.
        let charge = ctx.inner().gc_object_charge(*size);
        ctx.0.gc_reserve_host(charge)?;
        let inner = ctx.inner_mut();
        inner.pin_gc_type(type_id); // keep the type alive for the object's (store) lifetime
        let mut data = vec![0u8; *size];
        for (slot, v) in slots.iter().zip(fields) {
            write_slot(*slot, &mut data, *v);
        }
        let idx = inner.alloc_gc(GcObject::new_struct(type_id, data.into_boxed_slice()))?;
        Ok(root_new_gc(inner, idx))
    }
}

impl Rooted<StructRef> {
    /// Reads field `index`.
    pub fn field(&self, store: impl AsContext, index: usize) -> Result<Val> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = self.gc_slot_checked(inner)?;
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
        let slot = self.gc_slot_checked(inner)?;
        let type_id = gc_object(inner, slot)?.header.type_id;
        Ok(StructType::from_id(inner.engine(), type_id))
    }

    /// Whether this struct's type is a subtype of `ty`. Mirrors `wasmtime::StructRef::matches_ty`.
    pub fn matches_ty(&self, store: impl AsContext, ty: &StructType) -> Result<bool> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = self.gc_slot_checked(inner)?;
        let type_id = gc_object(inner, slot)?.header.type_id;
        Ok(inner.engine().is_subtype(type_id, ty.canonical_id()))
    }

    /// Writes `value` to field `index`. Errors if the field is out of bounds, immutable, or
    /// `value` doesn't match the field's type.
    pub fn set_field(&self, mut store: impl AsContextMut, index: usize, value: Val) -> Result<()> {
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        let slot = self.gc_slot_checked(inner)?;
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

    /// Allocates an `i8` array initialized from the given byte slice.
    ///
    /// This bypasses the transient `Val` slice used by [`ArrayRef::new_fixed`]
    /// and copies the packed element body directly.
    pub fn new_from_i8_slice(
        mut store: impl AsContextMut,
        allocator: &ArrayRefPre,
        elems: &[u8],
    ) -> Result<Rooted<ArrayRef>> {
        Self::validate_i8_allocator(allocator)?;
        let byte_len = Self::i8_slice_len(elems)?;
        let type_id = allocator.ty.canonical_id();
        let mut ctx = store.as_context_mut();
        let charge = ctx.inner().gc_object_charge(byte_len);
        ctx.0.gc_reserve_host(charge)?;
        let inner = ctx.inner_mut();
        inner.pin_gc_type(type_id);
        let idx = inner.alloc_gc(GcObject::new_array(
            type_id,
            elems.to_vec().into_boxed_slice(),
        ))?;
        Ok(root_new_gc(inner, idx))
    }

    /// Async sibling of [`ArrayRef::new_from_i8_slice`].
    #[cfg(feature = "async")]
    pub async fn new_from_i8_slice_async<T: 'static>(
        mut store: impl AsContextMut<Data = T>,
        allocator: &ArrayRefPre,
        elems: &[u8],
    ) -> Result<Rooted<ArrayRef>> {
        Self::validate_i8_allocator(allocator)?;
        let byte_len = Self::i8_slice_len(elems)?;
        let type_id = allocator.ty.canonical_id();
        let mut ctx = store.as_context_mut();
        let charge = ctx.inner().gc_object_charge(byte_len);
        ctx.store_mut().gc_reserve_host_async(charge).await?;
        let inner = ctx.inner_mut();
        inner.pin_gc_type(type_id);
        let idx = inner.alloc_gc(GcObject::new_array(
            type_id,
            elems.to_vec().into_boxed_slice(),
        ))?;
        Ok(root_new_gc(inner, idx))
    }

    fn validate_i8_allocator(allocator: &ArrayRefPre) -> Result<()> {
        if allocator.ty.element_type() != StorageType::I8 {
            return Err(Error::msg(
                "element type mismatch: cannot initialize a non-i8 array from a byte slice",
            ));
        }
        debug_assert_eq!(allocator.layout.stride(), 1);
        Ok(())
    }

    fn i8_slice_len(elems: &[u8]) -> Result<usize> {
        let len = u32::try_from(elems.len()).map_err(|_| Error::msg("array too large"))?;
        Ok(len as usize)
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
        let type_id = allocator.ty.canonical_id();
        let mut ctx = store.as_context_mut();
        // Reserve through the limiter (collect-then-grow) before building the (possibly large) body,
        // so a hostile element count traps here instead of allocating the `Vec` first. The element
        // values are host-held `Val`s (rooted if GC refs), so a collection here is safe.
        let charge = ctx.inner().gc_object_charge(byte_len);
        ctx.0.gc_reserve_host(charge)?;
        let inner = ctx.inner_mut();
        inner.pin_gc_type(type_id);
        let mut data = vec![0u8; byte_len];
        for i in 0..count {
            write_slot(allocator.layout.elem_at(i), &mut data, *elem_at(i));
        }
        // The element count is implicit in the body length, not stored.
        let idx = inner.alloc_gc(GcObject::new_array(type_id, data.into_boxed_slice()))?;
        Ok(root_new_gc(inner, idx))
    }
}

impl Rooted<ArrayRef> {
    /// The number of elements (derived from the body length and the element stride).
    pub fn len(&self, store: impl AsContext) -> Result<u32> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = self.gc_slot_checked(inner)?;
        let obj = gc_object(inner, slot)?;
        let stride = Layout::for_array(&inner.engine().array_field(obj.header.type_id)).stride();
        Ok(obj.array_len(stride))
    }

    /// Reads element `index`.
    pub fn get(&self, store: impl AsContext, index: u32) -> Result<Val> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = self.gc_slot_checked(inner)?;
        let obj = gc_object(inner, slot)?;
        let layout = Layout::for_array(&inner.engine().array_field(obj.header.type_id));
        if index >= obj.array_len(layout.stride()) {
            return Err(Error::msg("array index out of bounds"));
        }
        Ok(read_slot(layout.elem_at(index as usize), &obj.data))
    }

    /// Copies this `i8` array's raw element bytes into `dst`.
    ///
    /// The destination length must exactly match this array's length.
    pub fn copy_to_i8_slice(&self, store: impl AsContext, dst: &mut [u8]) -> Result<()> {
        let ctx = store.as_context();
        let inner = ctx.inner();
        let slot = self.gc_slot_checked(inner)?;
        let obj = gc_object(inner, slot)?;
        let field_ty = inner.engine().array_field(obj.header.type_id);
        if *field_ty.element_type() != StorageType::I8 {
            return Err(Error::msg(
                "element type mismatch: cannot copy a non-i8 array into a byte slice",
            ));
        }
        let len = obj.array_len(Layout::for_array(&field_ty).stride());
        let dst_len = u32::try_from(dst.len()).map_err(|_| Error::msg("destination too large"))?;
        if dst_len != len {
            return Err(Error::msg(format!(
                "destination slice length is {dst_len} but the array length is {len}"
            )));
        }
        dst.copy_from_slice(&obj.data);
        Ok(())
    }

    /// Writes `value` to element `index`. Errors if the index is out of bounds, the element type
    /// is immutable, or `value` doesn't match it.
    pub fn set(&self, mut store: impl AsContextMut, index: u32, value: Val) -> Result<()> {
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        let slot = self.gc_slot_checked(inner)?;
        let type_id = gc_object(inner, slot)?.header.type_id;
        let field_ty = inner.engine().array_field(type_id);
        let len = gc_object(inner, slot)?.array_len(Layout::for_array(&field_ty).stride());
        if index >= len {
            return Err(Error::msg("array index out of bounds"));
        }
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
