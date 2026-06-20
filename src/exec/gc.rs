//! Execution of GC struct + `i31` instructions (arrays live in [`super::gc_array`]). A struct's
//! body is a single packed byte buffer; the per-type [`Layout`](crate::canon::Layout) gives each
//! field's slot (offset + kind), and `store::gc`'s codecs read/write `Val`s through it. Field
//! access decodes the `anyref` handle, trapping on null rather than panicking.

// `i31` (un)boxing is intentional 31-bit two's-complement wraparound.
#![allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use super::Execution;
use crate::canon::Layout;
use crate::instance::Instance;
use crate::module::op::Op;
use crate::store::{
    anyref_handle_i31, anyref_handle_slot, anyref_value, decode_anyref_handle, default_for_slot,
    read_slot, read_slot_packed, write_slot, AnyRefHandle, GcObject, StoreInner,
};
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

impl Execution {
    /// Dispatches a GC aggregate op: struct + `i31` here, arrays to [`Execution::exec_gc_array`].
    pub(super) fn exec_gc(
        &mut self,
        inner: &mut StoreInner,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        match op {
            Op::StructNew(ty) => self.struct_new(inner, instance, *ty, false),
            Op::StructNewDefault(ty) => self.struct_new(inner, instance, *ty, true),
            Op::StructGet { ty, field } => self.struct_get(inner, instance, *ty, *field, None),
            Op::StructGetS { ty, field } => {
                self.struct_get(inner, instance, *ty, *field, Some(true))
            }
            Op::StructGetU { ty, field } => {
                self.struct_get(inner, instance, *ty, *field, Some(false))
            }
            Op::StructSet { ty, field } => self.struct_set(inner, instance, *ty, *field),
            Op::RefI31 => {
                let v = self.pop_i32();
                self.push(anyref_value(anyref_handle_i31(v)));
                Ok(())
            }
            Op::I31GetS => self.i31_get(true),
            Op::I31GetU => self.i31_get(false),
            _ => self.exec_gc_array(inner, op, instance),
        }
    }

    fn struct_new(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        default: bool,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let type_id = module.inner().canonical_type_id(ty);
        let layout = module.inner().layout(ty);
        let Layout::Struct { fields, size } = layout else {
            unreachable!("struct.new on non-struct type");
        };
        let mut data = vec![0u8; *size];
        if default {
            for &slot in fields.as_ref() {
                write_slot(slot, &mut data, default_for_slot(slot));
            }
        } else {
            // Operands sit on the stack in field order; fill from the last field back.
            for &slot in fields.iter().rev() {
                let v = self.pop();
                write_slot(slot, &mut data, v);
            }
        }
        let handle = inner.alloc_gc(GcObject::new_struct(type_id, data.into_boxed_slice()))?;
        self.push(anyref_value(anyref_handle_slot(handle)));
        Ok(())
    }

    fn struct_get(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        ty: u32,
        field: u32,
        ext: Option<bool>,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let slot = module.inner().layout(ty).field(field as usize);
        let r = self.pop();
        let obj = anyref_slot(&r, Trap::NullStructReference)?;
        let data = &inner.gc_object(obj).expect("live gc slot").data;
        let v = match ext {
            None => read_slot(slot, data),
            Some(signed) => Val::I32(read_slot_packed(slot, data, signed)),
        };
        self.push(v);
        Ok(())
    }

    fn struct_set(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        ty: u32,
        field: u32,
    ) -> Result<()> {
        let module = inner.instance(instance).module.clone();
        let slot = module.inner().layout(ty).field(field as usize);
        let v = self.pop();
        let r = self.pop();
        let obj = anyref_slot(&r, Trap::NullStructReference)?;
        let data = &mut inner.gc_object_mut(obj).expect("live gc slot").data;
        write_slot(slot, data, v);
        Ok(())
    }

    fn i31_get(&mut self, signed: bool) -> Result<()> {
        let handle = match self.pop() {
            Val::AnyRef(Some(r)) => r.raw(),
            Val::AnyRef(None) => return Err(Trap::NullI31Reference.into()),
            _ => unreachable!("i31.get on non-anyref"),
        };
        let v = match decode_anyref_handle(handle) {
            // `get_u` reads the same 31-bit payload zero-extended (mask off the sign extension).
            AnyRefHandle::I31(s) => {
                if signed {
                    s
                } else {
                    s & 0x7FFF_FFFF
                }
            }
            AnyRefHandle::Slot(_) => unreachable!("i31.get on non-i31"),
        };
        self.push(Val::I32(v));
        Ok(())
    }
}

/// Decodes a (non-null) `anyref` operand to its heap slot; null traps with `null_trap` (which
/// the spec spells per kind: "null array reference" / "null structure reference"). An `i31` here
/// is impossible by validation (struct/array ops take a concrete aggregate ref).
pub(super) fn anyref_slot(r: &Val, null_trap: Trap) -> Result<u32> {
    match r {
        Val::AnyRef(Some(rooted)) => match decode_anyref_handle(rooted.raw()) {
            AnyRefHandle::Slot(i) => Ok(i),
            AnyRefHandle::I31(_) => unreachable!("aggregate op on i31"),
        },
        Val::AnyRef(None) => Err(null_trap.into()),
        _ => unreachable!("operand validated as anyref"),
    }
}
