//! Execution of GC array instructions. An array body is a single packed byte buffer of
//! `len * stride` bytes; the per-type [`Layout`](crate::canon::Layout) gives the element slot
//! (offset `i * stride` + kind) and `store::gc`'s codecs read/write `Val`s through it.
//! Construction pre-checks the byte size so a hostile element count traps rather than aborting on
//! a failed host allocation; every index access is bounds-checked against the element count
//! (`ArrayOutOfBounds`), and data/elem segment ranges against the segment (`Memory`/`Table`OOB).

// Index/width juggling on validated inputs is intentional narrowing. Indexing is into the
// wasmparser-validated data/elem index space or guarded by a just-checked bound (array element
// count, segment range) — never unchecked guest input (#33 carve-out).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing
)]

use super::gc::anyref_slot;
use super::Execution;
use crate::canon::{CanonicalTypeId, Layout, Slot};
use crate::instance::Instance;
use crate::module::op::Op;
use crate::store::{
    anyref_handle_slot, anyref_value, default_for_slot, read_slot, read_slot_packed, write_slot,
    GcObject, StoreInner,
};
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

impl Execution {
    /// Dispatches a GC array op (routed here from [`Execution::exec_gc`]).
    pub(super) fn exec_gc_array(
        &mut self,
        inner: &mut StoreInner,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        match op {
            Op::ArrayNew(ty) => self.array_new(inner, instance, *ty, false),
            Op::ArrayNewDefault(ty) => self.array_new(inner, instance, *ty, true),
            Op::ArrayNewFixed { ty, n } => self.array_new_fixed(inner, instance, *ty, *n),
            Op::ArrayNewData { ty, data } => self.array_new_data(inner, instance, *ty, *data),
            Op::ArrayNewElem { ty, elem } => self.array_new_elem(inner, instance, *ty, *elem),
            Op::ArrayGet(ty) => self.array_get(inner, instance, *ty, None),
            Op::ArrayGetS(ty) => self.array_get(inner, instance, *ty, Some(true)),
            Op::ArrayGetU(ty) => self.array_get(inner, instance, *ty, Some(false)),
            Op::ArraySet(ty) => self.array_set(inner, instance, *ty),
            Op::ArrayLen => self.array_len(inner),
            Op::ArrayFill(ty) => self.array_fill(inner, instance, *ty),
            Op::ArrayCopy { dst, .. } => self.array_copy(inner, instance, *dst),
            Op::ArrayInitData { ty, data } => self.array_init_data(inner, instance, *ty, *data),
            Op::ArrayInitElem { ty, elem } => self.array_init_elem(inner, instance, *ty, *elem),
            // Not an array op â try the casts (then numerics) in the fall-through chain.
            _ => self.exec_cast(inner, op, instance),
        }
    }

    fn array_new(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        default: bool,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let type_id = module.inner().canonical_type_id(ty);
        let stride = module.inner().layout(ty).stride();
        let count = self.pop_i32() as u32 as usize;
        // `byte_len` (count Ã stride) was already bounded by `gc_reserve` before this op ran
        // (limiter or abort cap), so a too-large array has already trapped â no abort-cap re-check
        // here, which would otherwise cap a limiter-approved large array.
        let byte_len = elem_bytes(count, stride)?;
        let mut data = vec![0u8; byte_len];
        let fill = if default {
            default_for_slot(module.inner().layout(ty).elem_at(0))
        } else {
            self.pop_val_for(module.inner().layout(ty).elem_at(0))
        };
        write_each(module.inner().layout(ty), &mut data, count, fill);
        self.alloc_array(inner, type_id, data)
    }

    fn array_new_fixed(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        n: u32,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let type_id = module.inner().canonical_type_id(ty);
        let layout = module.inner().layout(ty);
        let count = n as usize;
        let mut data = vec![0u8; layout.body_size(count)];
        for i in (0..count).rev() {
            let v = self.pop_val_for(layout.elem_at(i));
            write_slot(layout.elem_at(i), &mut data, v);
        }
        self.alloc_array(inner, type_id, data)
    }

    fn array_new_data(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        data: u32,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let type_id = module.inner().canonical_type_id(ty);
        let stride = module.inner().layout(ty).stride();
        let count = self.pop_i32() as u32 as usize;
        let offset = self.pop_i32() as u32 as usize;
        let byte_len = elem_bytes(count, stride)?;
        let dropped = inner.instance(instance).dropped_data[data as usize];
        let seg = &module.inner().datas[data as usize].bytes;
        let seg_len = if dropped { 0 } else { seg.len() };
        range(offset, byte_len, seg_len, Trap::MemoryOutOfBounds)?;
        inner.gc_check_capacity(byte_len)?;
        // Scalar elements share the segment's little-endian layout â a direct byte copy.
        let body = seg[offset..offset + byte_len].to_vec();
        self.alloc_array(inner, type_id, body)
    }

    fn array_new_elem(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        elem: u32,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let type_id = module.inner().canonical_type_id(ty);
        let layout = module.inner().layout(ty);
        let count = self.pop_i32() as u32 as usize;
        let offset = self.pop_i32() as u32 as usize;
        let refs = self.segment_refs(inner, instance, elem);
        range(offset, count, refs.len(), Trap::TableOutOfBounds)?;
        inner.gc_check_capacity(elem_bytes(count, layout.stride())?)?;
        let mut data = vec![0u8; layout.body_size(count)];
        for (i, r) in refs[offset..offset + count].iter().enumerate() {
            write_slot(layout.elem_at(i), &mut data, Val::from_ref(r.clone()));
        }
        self.alloc_array(inner, type_id, data)
    }

    fn array_get(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        ty: u32,
        ext: Option<bool>,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let layout = module.inner().layout(ty);
        let idx = self.pop_i32() as u32 as usize;
        let r = self.pop_anyref();
        let obj = anyref_slot(&r, Trap::NullArrayReference)?;
        let slot = self.elem_slot(inner, obj, layout, idx)?;
        let data = &inner.gc_object(obj).expect("live gc slot").data;
        self.push(match ext {
            None => read_slot(slot, data),
            Some(signed) => Val::I32(read_slot_packed(slot, data, signed)),
        });
        Ok(())
    }

    fn array_set(&mut self, inner: &mut StoreInner, instance: Instance, ty: u32) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let layout = module.inner().layout(ty);
        let v = self.pop_val_for(layout.elem_at(0));
        let idx = self.pop_i32() as u32 as usize;
        let r = self.pop_anyref();
        let obj = anyref_slot(&r, Trap::NullArrayReference)?;
        let slot = self.elem_slot(inner, obj, layout, idx)?;
        write_slot(
            slot,
            &mut inner.gc_object_mut(obj).expect("live gc slot").data,
            v,
        );
        Ok(())
    }

    fn array_len(&mut self, inner: &StoreInner) -> Result<()> {
        let r = self.pop_anyref();
        let obj = anyref_slot(&r, Trap::NullArrayReference)?;
        // `array.len` carries no type immediate (it's polymorphic), so the element stride is
        // recovered from the object's canonical type via the engine registry. Typically called once
        // per array (a loop bound), so this lookup is amortized.
        let type_id = inner.gc_object(obj).expect("live gc slot").header.type_id;
        let stride = Layout::for_array(&inner.engine().array_field(type_id)).stride();
        self.push(Val::I32(arr_len(inner, obj, stride) as i32));
        Ok(())
    }

    fn array_fill(&mut self, inner: &mut StoreInner, instance: Instance, ty: u32) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let layout = module.inner().layout(ty);
        let len = self.pop_i32() as u32 as usize;
        let v = self.pop_val_for(layout.elem_at(0));
        let idx = self.pop_i32() as u32 as usize;
        let r = self.pop_anyref();
        let obj = anyref_slot(&r, Trap::NullArrayReference)?;
        range(
            idx,
            len,
            arr_len(inner, obj, layout.stride()),
            Trap::ArrayOutOfBounds,
        )?;
        let data = &mut inner.gc_object_mut(obj).expect("live gc slot").data;
        for i in idx..idx + len {
            write_slot(layout.elem_at(i), data, v);
        }
        Ok(())
    }

    fn array_copy(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        dst_ty: u32,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let stride = module.inner().layout(dst_ty).stride();
        let len = self.pop_i32() as u32 as usize;
        let src_idx = self.pop_i32() as u32 as usize;
        let src_r = self.pop_anyref();
        let dst_idx = self.pop_i32() as u32 as usize;
        let dst_r = self.pop_anyref();
        let src = anyref_slot(&src_r, Trap::NullArrayReference)?;
        let dst = anyref_slot(&dst_r, Trap::NullArrayReference)?;
        range(
            src_idx,
            len,
            arr_len(inner, src, stride),
            Trap::ArrayOutOfBounds,
        )?;
        range(
            dst_idx,
            len,
            arr_len(inner, dst, stride),
            Trap::ArrayOutOfBounds,
        )?;
        // Byte copy (handles match-width ref handles too); snapshot so src==dst overlap is safe.
        let from = src_idx * stride;
        let snapshot =
            inner.gc_object(src).expect("live gc slot").data[from..from + len * stride].to_vec();
        let to = dst_idx * stride;
        inner.gc_object_mut(dst).expect("live gc slot").data[to..to + len * stride]
            .copy_from_slice(&snapshot);
        Ok(())
    }

    fn array_init_data(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        data: u32,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let stride = module.inner().layout(ty).stride();
        let len = self.pop_i32() as u32 as usize;
        let src = self.pop_i32() as u32 as usize;
        let dst = self.pop_i32() as u32 as usize;
        let r = self.pop_anyref();
        let obj = anyref_slot(&r, Trap::NullArrayReference)?;
        // Array (dst) range before the data (src) range â a `len` overrunning both reports
        // "out of bounds array access" (matches the spec ordering).
        range(
            dst,
            len,
            arr_len(inner, obj, stride),
            Trap::ArrayOutOfBounds,
        )?;
        let byte_len = elem_bytes(len, stride)?;
        let dropped = inner.instance(instance).dropped_data[data as usize];
        let seg = &module.inner().datas[data as usize].bytes;
        let seg_len = if dropped { 0 } else { seg.len() };
        range(src, byte_len, seg_len, Trap::MemoryOutOfBounds)?;
        let bytes = seg[src..src + byte_len].to_vec();
        let to = dst * stride;
        inner.gc_object_mut(obj).expect("live gc slot").data[to..to + byte_len]
            .copy_from_slice(&bytes);
        Ok(())
    }

    fn array_init_elem(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        elem: u32,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let layout = module.inner().layout(ty);
        let len = self.pop_i32() as u32 as usize;
        let src = self.pop_i32() as u32 as usize;
        let dst = self.pop_i32() as u32 as usize;
        let r = self.pop_anyref();
        let obj = anyref_slot(&r, Trap::NullArrayReference)?;
        // Array (dst) range before the elem-segment (src) range, as in `array.init_data`.
        range(
            dst,
            len,
            arr_len(inner, obj, layout.stride()),
            Trap::ArrayOutOfBounds,
        )?;
        let refs = self.segment_refs(inner, instance, elem);
        range(src, len, refs.len(), Trap::TableOutOfBounds)?;
        let data = &mut inner.gc_object_mut(obj).expect("live gc slot").data;
        for (i, r) in refs[src..src + len].iter().enumerate() {
            write_slot(layout.elem_at(dst + i), data, Val::from_ref(r.clone()));
        }
        Ok(())
    }

    /// Allocates a fully-initialized array object and pushes its reference. The element count is
    /// implicit in `data.len()` (= count Ã stride).
    fn alloc_array(
        &mut self,
        inner: &mut StoreInner,
        type_id: CanonicalTypeId,
        data: Vec<u8>,
    ) -> Result<()> {
        let slot = inner.alloc_gc(GcObject::new_array(type_id, data.into_boxed_slice()))?;
        self.push(anyref_value(anyref_handle_slot(slot)));
        Ok(())
    }

    /// The element slot at `idx`, bounds-checked against the object's element count.
    fn elem_slot(&self, inner: &StoreInner, obj: u32, layout: &Layout, idx: usize) -> Result<Slot> {
        if idx < arr_len(inner, obj, layout.stride()) {
            Ok(layout.elem_at(idx))
        } else {
            Err(Trap::ArrayOutOfBounds.into())
        }
    }

    /// The element instance for segment `elem` (evaluated once at instantiation; empty if
    /// `elem.drop`ped).
    fn segment_refs(
        &self,
        inner: &StoreInner,
        instance: Instance,
        elem: u32,
    ) -> Vec<crate::value::Ref> {
        inner.instance(instance).elems[elem as usize].clone()
    }
}

fn arr_len(inner: &StoreInner, obj: u32, stride: usize) -> usize {
    inner
        .gc_object(obj)
        .expect("live gc slot")
        .array_len(stride) as usize
}

/// `count * width`, trapping (allocation-too-large) on overflow.
fn elem_bytes(count: usize, width: usize) -> Result<usize> {
    count
        .checked_mul(width)
        .ok_or_else(|| Trap::AllocationTooLarge.into())
}

/// Bounds-checks `[start, start+len)` against `total`, trapping with `trap` if it overflows.
fn range(start: usize, len: usize, total: usize, trap: Trap) -> Result<()> {
    match start.checked_add(len) {
        Some(end) if end <= total => Ok(()),
        _ => Err(trap.into()),
    }
}

/// Writes `count` copies of `v` into a fresh array body via the type's element slot.
fn write_each(layout: &Layout, data: &mut [u8], count: usize, v: Val) {
    for i in 0..count {
        write_slot(layout.elem_at(i), data, v);
    }
}
