//! Execution of GC casts and reference equality: `ref.test`/`ref.cast`, `br_on_cast[_fail]`,
//! and `ref.eq`. The shared [`matches_heaptype`] predicate decides whether a reference value
//! belongs to a target heap type — abstract hierarchies by kind, concrete types by canonical-id
//! subtyping against the engine registry.

use super::{cell, Execution};
use crate::canon::{AggKind, CanonicalTypeId, IrHeap, RefKind};
use crate::instance::Instance;
use crate::module::op::Op;
use crate::store::{decode_anyref_handle, AnyRefHandle, FuncEntity, ObjKind, StoreInner};
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

impl Execution {
    /// `ref.test`/`ref.cast`/`ref.eq` and the `any`/`extern` conversions (the straight-line
    /// casts; branches are in `step`).
    pub(super) fn exec_cast(
        &mut self,
        inner: &mut StoreInner,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        match op {
            Op::RefTest { ty, nullable } => {
                let r = self.pop_ref(cell::refkind_of_irheap(ty));
                let hit = matches_heaptype(inner, instance, &r, ty, *nullable);
                self.push(Val::I32(i32::from(hit)));
                Ok(())
            }
            Op::RefCast { ty, nullable } => {
                let r = self.pop_ref(cell::refkind_of_irheap(ty));
                if matches_heaptype(inner, instance, &r, ty, *nullable) {
                    self.push(r);
                    Ok(())
                } else {
                    Err(Trap::CastFailure.into())
                }
            }
            Op::RefEq => {
                let b = self.pop_anyref();
                let a = self.pop_anyref();
                self.push(Val::I32(i32::from(ref_eq(&a, &b))));
                Ok(())
            }
            Op::AnyConvertExtern => {
                let e = self.pop_ref(RefKind::Extern);
                let a = inner.any_convert_extern(e)?;
                self.push(a);
                Ok(())
            }
            Op::ExternConvertAny => {
                let a = self.pop_anyref();
                self.push(inner.extern_convert_any(a));
                Ok(())
            }
            // Not a cast op — the remaining straight-line ops are numeric (chain's end).
            _ => self.exec_numeric(inner, op, instance),
        }
    }

    /// `br_on_cast` (`on_fail=false`) / `br_on_cast_fail` (`on_fail=true`): the reference stays on
    /// the stack either way; returns the branch IP when the (possibly negated) cast test decides
    /// to take the branch.
    pub(super) fn br_on_cast(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        op: &Op,
        on_fail: bool,
    ) -> Option<u32> {
        let (ty, nullable, target) = match op {
            Op::BrOnCast {
                ty,
                nullable,
                target,
            }
            | Op::BrOnCastFail {
                ty,
                nullable,
                target,
            } => (ty, *nullable, target),
            _ => unreachable!("not a br_on_cast op"),
        };
        let r = self.pop_ref(cell::refkind_of_irheap(ty));
        let matched = matches_heaptype(inner, instance, &r, ty, nullable);
        self.push(r);
        if matched ^ on_fail {
            self.take_branch(target);
            Some(target.ip)
        } else {
            None
        }
    }
}

/// Whether reference `value` is a member of heap type `target` (with `nullable` controlling
/// whether null matches). Null is handled first; otherwise abstract targets match by hierarchy
/// and concrete targets by canonical-id subtyping.
pub(super) fn matches_heaptype(
    inner: &StoreInner,
    instance: Instance,
    value: &Val,
    target: &IrHeap,
    nullable: bool,
) -> bool {
    use IrHeap as H;
    if value.is_null_ref() {
        return nullable;
    }
    match target {
        H::Any => matches!(value, Val::AnyRef(Some(_))),
        H::Eq => is_eq(inner, value),
        H::I31 => matches!(decode_any(value), Some(AnyRefHandle::I31(_))),
        H::Struct => gc_kind(inner, value) == Some(AggKind::Struct),
        H::Array => gc_kind(inner, value) == Some(AggKind::Array),
        H::Func => matches!(value, Val::FuncRef(Some(_))),
        H::Extern => matches!(value, Val::ExternRef(Some(_))),
        H::Exn => matches!(value, Val::ExnRef(Some(_))),
        // The bottom types are inhabited only by null, already handled above.
        H::NoFunc | H::NoExtern | H::NoExn | H::None => false,
        H::Concrete(idx, kind) => concrete_matches(inner, instance, value, *idx, *kind),
    }
}

/// `eq` admits `i31`, `struct`, and `array` references (not, eventually, externalized `any`).
fn is_eq(inner: &StoreInner, value: &Val) -> bool {
    matches!(decode_any(value), Some(AnyRefHandle::I31(_))) || gc_kind(inner, value).is_some()
}

fn concrete_matches(
    inner: &StoreInner,
    instance: Instance,
    value: &Val,
    idx: u32,
    kind: AggKind,
) -> bool {
    let target_id = inner
        .instance(instance)
        .module
        .inner()
        .canonical_type_id(idx);
    let actual = match kind {
        AggKind::Func => match value {
            Val::FuncRef(Some(f)) => Some(func_type_id(inner, *f)),
            _ => None,
        },
        AggKind::Struct | AggKind::Array => gc_type_id(inner, value),
    };
    actual.is_some_and(|a| inner.engine().is_subtype(a, target_id))
}

/// The decoded `anyref` handle of a non-null `anyref` value, if it is one.
fn decode_any(value: &Val) -> Option<AnyRefHandle> {
    match value {
        Val::AnyRef(Some(r)) => Some(decode_anyref_handle(r.raw())),
        _ => None,
    }
}

/// The aggregate kind of a non-null `anyref` heap object (`None` for `i31` or non-`anyref`).
fn gc_kind(inner: &StoreInner, value: &Val) -> Option<AggKind> {
    let slot = match decode_any(value)? {
        AnyRefHandle::Slot(i) => i,
        AnyRefHandle::I31(_) => return None,
    };
    match inner.gc_object(slot).expect("live gc slot").header.kind {
        ObjKind::Struct => Some(AggKind::Struct),
        ObjKind::Array => Some(AggKind::Array),
        ObjKind::Extern => None, // an externalized host ref is `any` but not a typed aggregate
    }
}

/// The canonical type id of a non-null `anyref` heap object (`None` for `i31`, `extern`
/// wrappers, or non-`anyref`).
fn gc_type_id(inner: &StoreInner, value: &Val) -> Option<CanonicalTypeId> {
    let AnyRefHandle::Slot(i) = decode_any(value)? else {
        return None;
    };
    let obj = inner.gc_object(i).expect("live gc slot");
    match obj.header.kind {
        ObjKind::Struct | ObjKind::Array => Some(obj.header.type_id),
        ObjKind::Extern => None,
    }
}

/// The canonical type id of a function reference's signature (wasm or host).
fn func_type_id(inner: &StoreInner, f: crate::func::Func) -> CanonicalTypeId {
    match inner.func(f) {
        FuncEntity::Wasm {
            instance,
            func_index,
        } => {
            let m = inner.instance(*instance).module.inner();
            m.canonical_type_id(m.func_types[*func_index as usize])
        }
        FuncEntity::Host { ty, .. } => ty.canonical_id(),
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { ty, .. } => ty.canonical_id(),
    }
}

/// Reference equality (`ref.eq`): both operands are `eqref` (so `anyref`); equal when both null,
/// or the same heap slot / same `i31` value (both encode to the same raw handle).
fn ref_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::AnyRef(None), Val::AnyRef(None)) => true,
        (Val::AnyRef(Some(x)), Val::AnyRef(Some(y))) => x.raw() == y.raw(),
        (Val::AnyRef(_), Val::AnyRef(_)) => false,
        _ => unreachable!("ref.eq operands are eqref"),
    }
}
