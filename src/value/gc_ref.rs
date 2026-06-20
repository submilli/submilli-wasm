//! GC reference handles. `externref` is real (a store-side host-payload arena);
//! the managed-GC refs (`anyref`/struct/array/exn) stay stubs until garbage collection lands.

use core::any::Any;
use core::marker::PhantomData;

use crate::canon::{CanonicalTypeId, Layout};
use crate::store::{
    anyref_handle_slot, decode_anyref_handle, read_slot, slot_accepts, write_slot, AnyRefHandle,
    AsContext, AsContextMut, GcObject, ObjKind, StoreContext, StoreContextMut,
};
use crate::value::gc_type::{ArrayType, StructType};
use crate::value::Val;
use crate::{Error, Result};

/// A rooted handle to a GC value, keeping it alive within a [`RootScope`].
/// `Copy`, regardless of the referent type (mirrors `wasmtime::Rooted`).
pub struct Rooted<T> {
    index: u32,
    _marker: PhantomData<T>,
}

impl<T> Clone for Rooted<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Rooted<T> {}

impl<T> Rooted<T> {
    /// Wraps a raw handle/index (an `anyref` handle for `AnyRef`, an arena index for
    /// `ExternRef`). Internal — the run loop builds reference values from raw handles.
    pub(crate) fn from_raw(index: u32) -> Self {
        Rooted {
            index,
            _marker: PhantomData,
        }
    }

    /// The raw handle/index behind this rooted reference.
    pub(crate) fn raw(self) -> u32 {
        self.index
    }
}

impl<T> core::fmt::Debug for Rooted<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rooted").finish_non_exhaustive()
    }
}

/// A scope bounding the lifetime of [`Rooted`] references. It delegates store access to
/// the wrapped store, so it's usable anywhere an `AsContext[Mut]` is. Reclamation on
/// drop is a no-op for now (the arena is grow-only; a tracing collector adds reclamation).
pub struct RootScope<S> {
    store: S,
}

impl<S: AsContextMut> RootScope<S> {
    pub fn new(store: S) -> Self {
        RootScope { store }
    }
}

impl<S: AsContext> AsContext for RootScope<S> {
    type Data = S::Data;

    fn as_context(&self) -> StoreContext<'_, S::Data> {
        self.store.as_context()
    }
}

impl<S: AsContextMut> AsContextMut for RootScope<S> {
    fn as_context_mut(&mut self) -> StoreContextMut<'_, S::Data> {
        self.store.as_context_mut()
    }
}

impl<S> core::fmt::Debug for RootScope<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RootScope").finish_non_exhaustive()
    }
}

/// An opaque host reference (`externref`).
#[derive(Debug)]
pub struct ExternRef {
    _private: (),
}

impl ExternRef {
    /// Wraps a host `value` as an `externref`, stored in the store's externref arena.
    pub fn new<T>(mut store: impl AsContextMut, value: T) -> Result<Rooted<ExternRef>>
    where
        T: Any + Send + Sync + 'static,
    {
        let index = store
            .as_context_mut()
            .inner_mut()
            .alloc_externref(Box::new(value));
        Ok(Rooted {
            index,
            _marker: PhantomData,
        })
    }
}

impl Rooted<ExternRef> {
    /// Borrows the host payload behind this `externref` (downcast with `Any`). `None` if
    /// the referent carries no host data. Mirrors `wasmtime::ExternRef::data` (reached
    /// there via `Rooted`'s `Deref`; we expose it directly to stay `unsafe`-free).
    pub fn data<'a, T>(
        &self,
        store: impl Into<StoreContext<'a, T>>,
    ) -> Result<Option<&'a (dyn Any + Send + Sync)>>
    where
        T: 'static,
    {
        Ok(store.into().inner().externref(self.index))
    }
}

/// A managed GC reference under the `any` hierarchy (`anyref`). The handle lives in the wrapping
/// [`Rooted`]; the inspection/cast methods are on `Rooted<AnyRef>` (the `Rooted<ExternRef>::data`
/// pattern — keeps us `unsafe`-free, and `rooted.method()` still matches wasmtime call sites).
#[derive(Debug)]
pub struct AnyRef {
    _private: (),
}

impl Rooted<AnyRef> {
    /// Reinterprets this `anyref` as a `structref`, erroring if it isn't one.
    pub fn unwrap_struct(&self, store: impl AsContext) -> Result<Rooted<StructRef>> {
        self.as_struct(store)?
            .ok_or_else(|| Error::msg("anyref is not a struct"))
    }

    /// This `anyref` as a `structref` if it is one, else `None`.
    pub fn as_struct(&self, store: impl AsContext) -> Result<Option<Rooted<StructRef>>> {
        Ok(
            if obj_kind(store.as_context().inner(), self.raw())? == Some(ObjKind::Struct) {
                Some(Rooted::from_raw(self.raw()))
            } else {
                None
            },
        )
    }

    /// Reinterprets this `anyref` as an `arrayref`, erroring if it isn't one.
    pub fn unwrap_array(&self, store: impl AsContext) -> Result<Rooted<ArrayRef>> {
        self.as_array(store)?
            .ok_or_else(|| Error::msg("anyref is not an array"))
    }

    /// This `anyref` as an `arrayref` if it is one, else `None`.
    pub fn as_array(&self, store: impl AsContext) -> Result<Option<Rooted<ArrayRef>>> {
        Ok(
            if obj_kind(store.as_context().inner(), self.raw())? == Some(ObjKind::Array) {
                Some(Rooted::from_raw(self.raw()))
            } else {
                None
            },
        )
    }
}

/// An exception reference (`exnref`).
#[derive(Debug)]
pub struct ExnRef {
    _private: (),
}

/// A GC struct instance (`structref`).
#[derive(Debug)]
pub struct StructRef {
    _private: (),
}

/// Pre-allocation handle for [`StructRef::new`]: caches the type's canonical id + packed byte
/// layout so repeated allocations skip the lookup (wasmtime's `*Pre` purpose).
#[derive(Debug)]
pub struct StructRefPre {
    type_id: CanonicalTypeId,
    layout: Layout,
}

impl StructRefPre {
    pub fn new(store: impl AsContextMut, ty: StructType) -> Self {
        let _ = store; // no rooting/registration needed under the null collector
        let fields: Vec<_> = ty.fields().collect();
        StructRefPre {
            type_id: ty.canonical_id(),
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
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        inner.gc_check_capacity(*size)?;
        let idx = inner.alloc_gc(GcObject::new_struct(
            allocator.type_id,
            data.into_boxed_slice(),
        ))?;
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

/// Pre-allocation handle for [`ArrayRef::new`] (caches the type's canonical id + element layout).
#[derive(Debug)]
pub struct ArrayRefPre {
    type_id: CanonicalTypeId,
    layout: Layout,
}

impl ArrayRefPre {
    pub fn new(store: impl AsContextMut, ty: ArrayType) -> Self {
        let _ = store;
        ArrayRefPre {
            type_id: ty.canonical_id(),
            layout: Layout::for_array(&ty.field_type()),
        }
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
        let mut ctx = store.as_context_mut();
        let inner = ctx.inner_mut();
        inner.gc_check_capacity(byte_len)?;
        let idx = inner.alloc_gc(GcObject::new_array(
            allocator.type_id,
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

/// Decodes an `anyref` handle to a heap slot, erroring on an `i31` (which isn't a heap object).
fn gc_slot(inner: &crate::store::StoreInner, handle: u32) -> Result<u32> {
    match decode_anyref_handle(handle) {
        AnyRefHandle::Slot(i) => Ok(i),
        AnyRefHandle::I31(_) => Err(Error::msg("reference is an i31, not a heap object")),
    }
    .and_then(|i| {
        gc_object(inner, i)?;
        Ok(i)
    })
}

/// The object at a heap slot, erroring if the handle dangles.
fn gc_object(inner: &crate::store::StoreInner, slot: u32) -> Result<&GcObject> {
    inner
        .gc_object(slot)
        .ok_or_else(|| Error::msg("dangling gc reference"))
}

/// The `ObjKind` of the object an `anyref` handle points at (`None` for an `i31`).
fn obj_kind(inner: &crate::store::StoreInner, handle: u32) -> Result<Option<ObjKind>> {
    match decode_anyref_handle(handle) {
        AnyRefHandle::I31(_) => Ok(None),
        AnyRefHandle::Slot(i) => Ok(Some(gc_object(inner, i)?.header.kind)),
    }
}
