//! Packed GC-body codecs: read/write a `Val` at a typed [`Slot`] within an object's byte buffer.
//! Scalars use little-endian byte conversions (no `unsafe`, no alignment needed); references are
//! 4-byte handles with a reserved null sentinel. Shared by `exec/gc*` and `instance/const_eval`.

// Packed-scalar (un)packing is intentional narrowing / sign reinterpretation.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]

use crate::canon::{RefKind, ScalarKind, Slot};
use crate::func::Func;
use crate::value::{AnyRef, ExnRef, ExternRef, Rooted, Val, V128};

/// The null reference handle in a packed body. Distinct from every live handle: `i31` handles
/// have the top bit set; GC slot indices are capped below it (see `GcHeap::alloc`); func/extern/
/// exn arena indices are resource-bounded well under it.
pub(crate) const NULL_REF: u32 = 0x7FFF_FFFF;

/// Reads the value at `slot` from a body `data` buffer.
pub(crate) fn read_slot(slot: Slot, data: &[u8]) -> Val {
    match slot {
        Slot::Scalar { offset, kind } => read_scalar(kind, &data[offset..]),
        Slot::Ref { offset, kind } => read_ref(kind, le_u32(&data[offset..])),
    }
}

/// Writes `v` into `slot` of a mutable body `data` buffer (packed scalars are truncated).
pub(crate) fn write_slot(slot: Slot, data: &mut [u8], v: Val) {
    match slot {
        Slot::Scalar { offset, kind } => write_scalar(kind, &mut data[offset..], v),
        Slot::Ref { offset, .. } => {
            data[offset..offset + 4].copy_from_slice(&ref_handle(v).to_le_bytes());
        }
    }
}

/// Reads a packed `i8`/`i16` field as `i32`, sign- or zero-extended (`struct/array.get_s`/`get_u`).
pub(crate) fn read_slot_packed(slot: Slot, data: &[u8], signed: bool) -> i32 {
    let Slot::Scalar { offset, kind } = slot else {
        unreachable!("get_s/get_u on a reference slot");
    };
    let bytes = &data[offset..];
    match kind {
        ScalarKind::I8 if signed => i32::from(bytes[0] as i8),
        ScalarKind::I8 => i32::from(bytes[0]),
        ScalarKind::I16 if signed => i32::from(i16::from_le_bytes([bytes[0], bytes[1]])),
        ScalarKind::I16 => i32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
        _ => read_scalar(kind, bytes).unwrap_i32(),
    }
}

/// The zero/default value for a slot (numeric zero / null reference).
pub(crate) fn default_for_slot(slot: Slot) -> Val {
    match slot {
        Slot::Scalar { kind, .. } => match kind {
            ScalarKind::I8 | ScalarKind::I16 | ScalarKind::I32 => Val::I32(0),
            ScalarKind::I64 => Val::I64(0),
            ScalarKind::F32 => Val::F32(0),
            ScalarKind::F64 => Val::F64(0),
            ScalarKind::V128 => Val::V128(V128::from(0)),
        },
        Slot::Ref { kind, .. } => null_ref(kind),
    }
}

fn read_scalar(kind: ScalarKind, b: &[u8]) -> Val {
    match kind {
        ScalarKind::I8 => Val::I32(i32::from(b[0])),
        ScalarKind::I16 => Val::I32(i32::from(u16::from_le_bytes([b[0], b[1]]))),
        ScalarKind::I32 => Val::I32(i32::from_le_bytes(le::<4>(b))),
        ScalarKind::I64 => Val::I64(i64::from_le_bytes(le::<8>(b))),
        ScalarKind::F32 => Val::F32(u32::from_le_bytes(le::<4>(b))),
        ScalarKind::F64 => Val::F64(u64::from_le_bytes(le::<8>(b))),
        ScalarKind::V128 => Val::V128(V128::from(u128::from_le_bytes(le::<16>(b)))),
    }
}

fn write_scalar(kind: ScalarKind, b: &mut [u8], v: Val) {
    match kind {
        ScalarKind::I8 => b[0] = v.unwrap_i32() as u8,
        ScalarKind::I16 => b[..2].copy_from_slice(&(v.unwrap_i32() as u16).to_le_bytes()),
        ScalarKind::I32 => b[..4].copy_from_slice(&v.unwrap_i32().to_le_bytes()),
        ScalarKind::I64 => b[..8].copy_from_slice(&v.unwrap_i64().to_le_bytes()),
        ScalarKind::F32 => b[..4].copy_from_slice(&v.unwrap_f32().to_bits().to_le_bytes()),
        ScalarKind::F64 => b[..8].copy_from_slice(&v.unwrap_f64().to_bits().to_le_bytes()),
        ScalarKind::V128 => b[..16].copy_from_slice(&v.unwrap_v128().as_u128().to_le_bytes()),
    }
}

fn read_ref(kind: RefKind, handle: u32) -> Val {
    if handle == NULL_REF {
        return null_ref(kind);
    }
    match kind {
        RefKind::Func => Val::FuncRef(Some(Func::from_raw(handle))),
        RefKind::Extern => Val::ExternRef(Some(Rooted::<ExternRef>::from_raw(handle))),
        RefKind::Any => Val::AnyRef(Some(Rooted::<AnyRef>::from_raw(handle))),
        RefKind::Exn => Val::ExnRef(Some(Rooted::<ExnRef>::from_raw(handle))),
    }
}

/// Lowers a reference `Val` to its stored 4-byte handle (`NULL_REF` for null).
fn ref_handle(v: Val) -> u32 {
    match v {
        Val::FuncRef(Some(f)) => f.raw(),
        Val::ExternRef(Some(r)) => r.raw(),
        Val::AnyRef(Some(r)) => r.raw(),
        Val::ExnRef(Some(r)) => r.raw(),
        Val::FuncRef(None) | Val::ExternRef(None) | Val::AnyRef(None) | Val::ExnRef(None) => {
            NULL_REF
        }
        _ => unreachable!("reference slot stores a reference value"),
    }
}

fn null_ref(kind: RefKind) -> Val {
    match kind {
        RefKind::Func => Val::FuncRef(None),
        RefKind::Extern => Val::ExternRef(None),
        RefKind::Any => Val::AnyRef(None),
        RefKind::Exn => Val::ExnRef(None),
    }
}

/// Reads a little-endian `u32` from the head of `b`.
pub(crate) fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(le::<4>(b))
}

fn le<const N: usize>(b: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&b[..N]);
    out
}
