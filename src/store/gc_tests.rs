#![allow(clippy::unwrap_used)]
use super::*;
use crate::canon::{RefKind, ScalarKind, Slot};
use crate::store::gc_codec::{read_slot, read_slot_packed, write_slot};
use crate::value::V128;

fn scalar(offset: usize, kind: ScalarKind) -> Slot {
    Slot::Scalar { offset, kind }
}

#[test]
fn alloc_and_read_struct_and_array() {
    let mut heap = GcHeap::new(true, 0);
    // struct { i32, i32 } at offsets 0,4.
    let mut sdata = vec![0u8; 8];
    write_slot(scalar(0, ScalarKind::I32), &mut sdata, Val::I32(1));
    write_slot(scalar(4, ScalarKind::I32), &mut sdata, Val::I32(2));
    let s = heap
        .alloc(GcObject::new_struct(
            CanonicalTypeId::new(0),
            sdata.into_boxed_slice(),
        ))
        .unwrap();
    // array i64[3] — 24 bytes / stride 8 = 3 elements (len is derived, not stored).
    let a = heap
        .alloc(GcObject::new_array(
            CanonicalTypeId::new(1),
            vec![0u8; 24].into_boxed_slice(),
        ))
        .unwrap();
    assert_ne!(s, a);
    let so = heap.get(s).unwrap();
    assert_eq!(so.header.kind, ObjKind::Struct);
    assert_eq!(
        read_slot(scalar(4, ScalarKind::I32), &so.data).unwrap_i32(),
        2
    );
    let ao = heap.get(a).unwrap();
    assert_eq!(ao.header.kind, ObjKind::Array);
    assert_eq!(ao.array_len(8), 3);
    assert!(heap.get(999).is_none());
}

#[test]
fn scalar_round_trip_per_kind() {
    let cases = [
        (ScalarKind::I8, Val::I32(-1), 1),
        (ScalarKind::I16, Val::I32(-1), 2),
        (ScalarKind::I32, Val::I32(-123_456), 4),
        (ScalarKind::I64, Val::I64(i64::MIN), 8),
        (ScalarKind::F32, Val::F32(1.5_f32.to_bits()), 4),
        (ScalarKind::F64, Val::F64(2.5_f64.to_bits()), 8),
        (ScalarKind::V128, Val::V128(V128::from(u128::MAX)), 16),
    ];
    for (kind, v, width) in cases {
        let mut data = vec![0u8; width];
        write_slot(scalar(0, kind), &mut data, v);
        let got = read_slot(scalar(0, kind), &data);
        // Packed kinds read back zero-extended; compare via the i32 low bits there.
        match kind {
            ScalarKind::I8 => assert_eq!(got.unwrap_i32(), 0xFF),
            ScalarKind::I16 => assert_eq!(got.unwrap_i32(), 0xFFFF),
            _ => assert_eq!(format!("{got:?}"), format!("{v:?}")),
        }
    }
}

#[test]
fn packed_sign_and_zero_extension() {
    let mut data = vec![0u8; 2];
    write_slot(scalar(0, ScalarKind::I8), &mut data, Val::I32(-1));
    assert_eq!(read_slot_packed(scalar(0, ScalarKind::I8), &data, true), -1);
    assert_eq!(
        read_slot_packed(scalar(0, ScalarKind::I8), &data, false),
        0xFF
    );
}

#[test]
fn ref_slot_null_and_nonnull() {
    let slot = Slot::Ref {
        offset: 0,
        kind: RefKind::Any,
    };
    let mut data = vec![0u8; 4];
    write_slot(slot, &mut data, Val::AnyRef(None));
    assert!(matches!(read_slot(slot, &data), Val::AnyRef(None)));
    write_slot(slot, &mut data, anyref_value(anyref_handle_slot(42)));
    match read_slot(slot, &data) {
        Val::AnyRef(Some(r)) => assert_eq!(r.raw(), 42),
        _ => panic!("expected non-null anyref"),
    }
}

#[test]
fn packed_i8_array_body_is_compact() {
    // An i8[1000] body is 1000 bytes, not 1000 * size_of::<Val>().
    let obj = GcObject::new_array(CanonicalTypeId::new(0), vec![0u8; 1000].into());
    assert_eq!(obj.data.len(), 1000);
    assert!(obj.byte_size() < 1000 + 64);
}

#[test]
fn i31_handle_round_trips() {
    for v in [0_i32, 1, -1, 42, -42, (1 << 30) - 1, -(1 << 30)] {
        assert_eq!(
            decode_anyref_handle(anyref_handle_i31(v)),
            AnyRefHandle::I31(v)
        );
    }
    assert_eq!(
        decode_anyref_handle(anyref_handle_slot(123)),
        AnyRefHandle::Slot(123)
    );
}

#[test]
fn over_abort_cap_traps_without_allocating() {
    // The abort-safety cap bounds a single allocation when no limiter is installed — a hostile
    // body size traps (via the pre-check) rather than building the `Vec` and risking OOM-abort.
    let heap = GcHeap::new(true, 0);
    assert!(heap.check_capacity(ABORT_SAFETY_CAP + 1).is_err());
    assert!(heap.check_capacity(4096).is_ok());
    assert!(!heap.can_fit_limit(ABORT_SAFETY_CAP + 1));
}

#[test]
fn sweep_frees_unmarked_and_bumps_generation() {
    let mut heap = GcHeap::new(true, 0);
    let a = heap
        .alloc(GcObject::new_struct(
            CanonicalTypeId::new(0),
            vec![0u8; 8].into(),
        ))
        .unwrap();
    let b = heap
        .alloc(GcObject::new_struct(
            CanonicalTypeId::new(0),
            vec![0u8; 8].into(),
        ))
        .unwrap();
    let gen_a = heap.generation(a).unwrap();
    heap.mark(b); // keep b, drop a
    heap.sweep();
    assert!(heap.get(a).is_none(), "unmarked slot freed");
    assert!(heap.get(b).is_some(), "marked slot kept");
    assert_eq!(
        heap.generation(a),
        Some(gen_a + 1),
        "freed slot bumps generation"
    );
    // The freed index is recycled (LIFO), with its bumped generation.
    let c = heap
        .alloc(GcObject::new_struct(
            CanonicalTypeId::new(0),
            vec![0u8; 8].into(),
        ))
        .unwrap();
    assert_eq!(c, a, "freed slot reused");
    assert_eq!(heap.generation(c), Some(gen_a + 1));
}

#[test]
fn unreserved_ceiling_follows_a_limiter_granted_reservation() {
    let mut heap = GcHeap::new(true, 0);
    // No reservation granted yet → host/const-eval allocation is bounded by the abort cap.
    assert!(!heap.can_fit_limit(ABORT_SAFETY_CAP + 1));
    // A limiter grew `reserved` past the abort cap → unreserved allocation may now follow it,
    // instead of being stuck at the cap.
    heap.grant(ABORT_SAFETY_CAP + (1 << 20));
    assert!(heap.can_fit_limit(ABORT_SAFETY_CAP + 1));
}

#[test]
fn slot_layout_is_compact() {
    // `len`/`mark` are out of the header (derived / bitmap) and `generation` is in a parallel array,
    // so a slot is just `Option<GcObject>` (24 B) + 4 B generation = 28 B/slot, no padding.
    assert!(
        core::mem::size_of::<GcObject>() <= 24,
        "GcObject = {}",
        core::mem::size_of::<GcObject>()
    );
    assert!(
        core::mem::size_of::<Option<GcObject>>() <= 24,
        "Option<GcObject> = {}",
        core::mem::size_of::<Option<GcObject>>()
    );
}
