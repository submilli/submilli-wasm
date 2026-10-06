//! Store-aware dynamic value validation and boundary argument coercion.

use crate::func::Func;
use crate::store::{FuncEntity, StoreInner};
use crate::value::{FuncType, HeapType, Val, ValType};
use crate::{Error, Result};

/// Value/type compatibility that does not need store metadata. Concrete reference types are
/// checked by [`val_matches_in_store`]; this predicate still rejects cross-hierarchy references
/// and nulls supplied for non-nullable types.
pub(crate) fn val_matches(val: &Val, ty: &ValType) -> bool {
    use crate::value::HeapType as H;

    match (val, ty) {
        (Val::I32(_), ValType::I32)
        | (Val::I64(_), ValType::I64)
        | (Val::F32(_), ValType::F32)
        | (Val::F64(_), ValType::F64)
        | (Val::V128(_), ValType::V128) => true,
        (Val::FuncRef(v), ValType::Ref(r)) => match r.heap_type() {
            H::NoFunc => v.is_none() && r.is_nullable(),
            H::Func | H::ConcreteFunc(_) => v.is_some() || r.is_nullable(),
            _ => false,
        },
        (Val::ExternRef(v), ValType::Ref(r)) => match r.heap_type() {
            H::NoExtern => v.is_none() && r.is_nullable(),
            H::Extern => v.is_some() || r.is_nullable(),
            _ => false,
        },
        (Val::ExnRef(v), ValType::Ref(r)) => match r.heap_type() {
            H::NoExn => v.is_none() && r.is_nullable(),
            H::Exn => v.is_some() || r.is_nullable(),
            _ => false,
        },
        (Val::AnyRef(v), ValType::Ref(r)) => {
            if matches!(r.heap_type(), H::None) {
                v.is_none() && r.is_nullable()
            } else {
                (v.is_some() || r.is_nullable())
                    && matches!(
                        r.heap_type(),
                        H::Any
                            | H::Eq
                            | H::I31
                            | H::Struct
                            | H::ConcreteStruct(_)
                            | H::Array
                            | H::ConcreteArray(_)
                    )
            }
        }
        _ => false,
    }
}

/// Store-aware value/type compatibility for dynamic calls. Besides the variant/nullability checks
/// in [`val_matches`], this validates concrete heap types and live managed references.
pub(crate) fn val_matches_in_store(inner: &StoreInner, val: &Val, ty: &ValType) -> Result<bool> {
    use crate::value::HeapType as H;

    if !val_matches(val, ty) {
        return Ok(false);
    }
    let ValType::Ref(r) = ty else {
        return Ok(true);
    };
    validate_live_reference(inner, val)?;
    match (val, r.heap_type()) {
        (Val::FuncRef(Some(f)), H::ConcreteFunc(expected)) => {
            Ok(concrete_func_matches(inner, *f, expected))
        }
        (Val::AnyRef(Some(v)), expected) => anyref_matches(inner, *v, expected),
        _ => Ok(true),
    }
}

/// Store-aware compatibility for values supplied as call arguments. WebAssembly's host-facing
/// `ref.host` representation arrives as an externref and may be passed to an `anyref` parameter;
/// [`coerce_args`] internalizes that value before execution. Other hierarchy crossings remain
/// type mismatches.
pub(crate) fn arg_matches_in_store(inner: &StoreInner, val: &Val, ty: &ValType) -> Result<bool> {
    let (Val::ExternRef(value), ValType::Ref(reference_type)) = (val, ty) else {
        return val_matches_in_store(inner, val, ty);
    };
    if value.is_some() && matches!(reference_type.heap_type(), HeapType::Any) {
        validate_live_reference(inner, val)?;
        return Ok(true);
    }
    val_matches_in_store(inner, val, ty)
}

fn validate_live_reference(inner: &StoreInner, val: &Val) -> Result<()> {
    use crate::store::{decode_anyref_handle, AnyRefHandle};

    match val {
        Val::FuncRef(Some(func)) => {
            if func.store != 0 && func.store != inner.store_id() {
                return Err(Error::msg(
                    "function reference belongs to a different store",
                ));
            }
            let _ = inner.func(*func);
        }
        Val::ExternRef(Some(reference)) => {
            if !reference.belongs_to_store(inner) {
                return Err(Error::msg("externref belongs to a different store"));
            }
            let _ = inner.externref_checked(*reference)?;
        }
        Val::AnyRef(Some(reference)) => {
            if !reference.belongs_to_store(inner) {
                return Err(Error::msg("anyref belongs to a different store"));
            }
            if matches!(decode_anyref_handle(reference.raw()), AnyRefHandle::Slot(_)) {
                reference.gc_slot_checked(inner)?;
            }
        }
        Val::ExnRef(Some(reference)) => {
            if !reference.belongs_to_store(inner) {
                return Err(Error::msg("exnref belongs to a different store"));
            }
            let _ = inner.exn_checked(*reference)?;
        }
        _ => {}
    }
    Ok(())
}

fn concrete_func_matches(inner: &StoreInner, func: Func, expected: &FuncType) -> bool {
    let actual = match inner.func(func) {
        FuncEntity::Wasm {
            instance,
            func_index,
        } => inner
            .instance(*instance)
            .module
            .inner()
            .func_type(*func_index)
            .canonical_id(),
        FuncEntity::Host { ty, .. } => ty.canonical_id(),
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { ty, .. } => ty.canonical_id(),
    };
    inner.engine().is_subtype(actual, expected.canonical_id())
}

fn anyref_matches(
    inner: &StoreInner,
    reference: crate::value::Rooted<crate::value::AnyRef>,
    expected: &HeapType,
) -> Result<bool> {
    use crate::store::{decode_anyref_handle, AnyRefHandle, ObjKind};

    let AnyRefHandle::Slot(_) = decode_anyref_handle(reference.raw()) else {
        return Ok(matches!(
            expected,
            HeapType::Any | HeapType::Eq | HeapType::I31
        ));
    };
    let slot = reference.gc_slot_checked(inner)?;
    let object = inner
        .gc_object(slot)
        .ok_or_else(|| Error::msg("stale gc reference (object was collected)"))?;
    Ok(match expected {
        HeapType::Any => true,
        HeapType::Eq => object.header.kind != ObjKind::Extern,
        HeapType::Struct => object.header.kind == ObjKind::Struct,
        HeapType::Array => object.header.kind == ObjKind::Array,
        HeapType::ConcreteStruct(expected) if object.header.kind == ObjKind::Struct => inner
            .engine()
            .is_subtype(object.header.type_id, expected.canonical_id()),
        HeapType::ConcreteArray(expected) if object.header.kind == ObjKind::Array => inner
            .engine()
            .is_subtype(object.header.type_id, expected.canonical_id()),
        _ => false,
    })
}

pub(crate) fn ensure_values_match(
    inner: &StoreInner,
    values: &[Val],
    types: &[ValType],
    context: &'static str,
) -> Result<()> {
    if values.len() != types.len() {
        return Err(crate::error::InternalError::ResultShape(context).into());
    }
    for (value, ty) in values.iter().zip(types) {
        if !val_matches_in_store(inner, value, ty)? {
            return Err(crate::error::InternalError::ResultShape(context).into());
        }
    }
    Ok(())
}

/// Internalizes a host externref accepted by [`arg_matches_in_store`] for an `anyref` parameter.
/// The untyped operand stack stores only a bare handle, so leaving the cross-hierarchy value as an
/// externref would make execution read it against the wrong arena. Other arguments pass through
/// untouched. See ARCHITECTURE §6.
pub(crate) fn coerce_args(
    inner: &mut StoreInner,
    params: &[Val],
    ty: &FuncType,
) -> Result<Vec<Val>> {
    params
        .iter()
        .zip(ty.params())
        .map(|(&v, pty)| coerce_arg(inner, v, &pty))
        .collect()
}

fn coerce_arg(inner: &mut StoreInner, v: Val, ty: &ValType) -> Result<Val> {
    match (v, ty) {
        (Val::ExternRef(Some(_)), ValType::Ref(rt)) if matches!(rt.heap_type(), HeapType::Any) => {
            inner.any_convert_extern(v)
        }
        _ => Ok(v),
    }
}
