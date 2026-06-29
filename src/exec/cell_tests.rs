#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
use super::*;
use crate::func::Func;

#[test]
fn scalar_cells_round_trip() {
    assert_eq!(decode(encode(Val::I32(-7)), &ValType::I32).unwrap_i32(), -7);
    assert_eq!(
        decode(encode(Val::I64(i64::MIN)), &ValType::I64).unwrap_i64(),
        i64::MIN
    );
    let f32_bits = (-1.5f32).to_bits();
    assert_eq!(
        decode(encode(Val::F32(f32_bits)), &ValType::F32)
            .unwrap_f32()
            .to_bits(),
        f32_bits
    );
    let f64_bits = 2.5f64.to_bits();
    assert_eq!(
        decode(encode(Val::F64(f64_bits)), &ValType::F64)
            .unwrap_f64()
            .to_bits(),
        f64_bits
    );
}

#[test]
fn ref_cells_encode_handle_and_null_sentinel() {
    let nonnull = encode(Val::FuncRef(Some(Func::from_raw(5))));
    assert!(!nonnull.is_null());
    assert_eq!(nonnull.handle(), 5);
    // Every null reference encodes to the reserved sentinel (not all-zero).
    assert!(encode(Val::FuncRef(None)).is_null());
    assert!(encode(Val::ExternRef(None)).is_null());
    assert!(encode(Val::AnyRef(None)).is_null());
    assert!(encode(Val::ExnRef(None)).is_null());
}

#[cfg(feature = "simd")]
#[test]
fn v128_cell_round_trips() {
    let v = V128::from(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128);
    assert_eq!(
        decode(encode(Val::V128(v)), &ValType::V128).unwrap_v128(),
        v
    );
}

// Soundness of the 8-byte cell rests on validation never admitting a `v128` value type when
// the `simd` feature is off (ARCHITECTURE §7) — so no `v128` ever needs 16 bytes on the stack.
#[cfg(not(feature = "simd"))]
#[test]
fn v128_type_rejected_without_simd_feature() {
    let engine = crate::Engine::default();
    let bytes = wat::parse_str("(module (func (param v128)))").unwrap();
    assert!(crate::Module::validate(&engine, &bytes).is_err());
}
