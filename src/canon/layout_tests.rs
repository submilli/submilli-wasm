//! Unit tests for the GC byte-layout computation.
#![allow(clippy::unwrap_used)]

use super::*;
use crate::canon::{CompositeBody, IrField, IrHeap, IrStorage, IrVal};

fn field(storage: IrStorage) -> IrField {
    IrField {
        mutable: true,
        storage,
    }
}

#[test]
fn mixed_struct_offsets_pack_tightly() {
    // { i8, i32, ref func, i16 } → offsets 0, 1, 5, 9; size 11.
    let body = CompositeBody::Struct(vec![
        field(IrStorage::I8),
        field(IrStorage::Val(IrVal::I32)),
        field(IrStorage::Val(IrVal::Ref {
            nullable: true,
            heap: IrHeap::Func,
        })),
        field(IrStorage::I16),
    ]);
    let Layout::Struct { fields, size } = Layout::from_body(&body).unwrap() else {
        panic!("expected struct layout");
    };
    assert_eq!(
        fields[0],
        Slot::Scalar {
            offset: 0,
            kind: ScalarKind::I8
        }
    );
    assert_eq!(
        fields[1],
        Slot::Scalar {
            offset: 1,
            kind: ScalarKind::I32
        }
    );
    assert_eq!(
        fields[2],
        Slot::Ref {
            offset: 5,
            kind: RefKind::Func
        }
    );
    assert_eq!(
        fields[3],
        Slot::Scalar {
            offset: 9,
            kind: ScalarKind::I16
        }
    );
    assert_eq!(size, 11);
}

#[test]
fn packed_i8_array_has_unit_stride() {
    let body = CompositeBody::Array(field(IrStorage::I8));
    let layout = Layout::from_body(&body).unwrap();
    assert_eq!(layout.stride(), 1);
    assert_eq!(layout.body_size(100), 100);
    assert_eq!(layout.elem_at(7).offset(), 7);
}

#[test]
fn ref_array_uses_handle_width() {
    let body = CompositeBody::Array(field(IrStorage::Val(IrVal::Ref {
        nullable: true,
        heap: IrHeap::Extern,
    })));
    let layout = Layout::from_body(&body).unwrap();
    assert_eq!(layout.stride(), REF_WIDTH);
    assert_eq!(
        layout.elem_at(3),
        Slot::Ref {
            offset: 12,
            kind: RefKind::Extern
        }
    );
}

#[test]
fn v128_and_f64_widths() {
    let body = CompositeBody::Array(field(IrStorage::Val(IrVal::V128)));
    assert_eq!(Layout::from_body(&body).unwrap().stride(), 16);
    let body = CompositeBody::Array(field(IrStorage::Val(IrVal::F64)));
    assert_eq!(Layout::from_body(&body).unwrap().stride(), 8);
}

#[test]
fn func_type_has_no_layout() {
    let body = CompositeBody::Func {
        params: vec![IrVal::I32],
        results: vec![],
    };
    assert!(Layout::from_body(&body).is_none());
}
