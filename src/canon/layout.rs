//! Byte-layout of a GC aggregate type, computed once per type from the module IR. A GC object's
//! body is a single tightly-packed `Box<[u8]>`; the field/element *types* live here (encoded
//! once per type), not per element. Scalars occupy their natural width; references occupy a
//! 4-byte handle. The interpreter reads/writes the body through these slots (see `store::gc`).

use super::{CompositeBody, IrField, IrHeap, IrStorage, IrVal};

/// Width of a reference handle in a packed GC body (a `u32` slot/i31/arena handle).
pub(crate) const REF_WIDTH: usize = 4;

/// A scalar field/element storage kind and its byte width.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScalarKind {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    V128,
}

impl ScalarKind {
    pub(crate) fn width(self) -> usize {
        match self {
            ScalarKind::I8 => 1,
            ScalarKind::I16 => 2,
            ScalarKind::I32 | ScalarKind::F32 => 4,
            ScalarKind::I64 | ScalarKind::F64 => 8,
            ScalarKind::V128 => 16,
        }
    }

    /// Whether this is a packed sub-`i32` integer (`i8`/`i16`), read via `*.get_s`/`get_u`.
    pub(crate) fn is_packed(self) -> bool {
        matches!(self, ScalarKind::I8 | ScalarKind::I16)
    }
}

/// Which reference hierarchy a ref field/element belongs to — selects the `Val` variant the
/// stored handle materializes into. Mirrors `Val::null_for_heap`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RefKind {
    Func,
    Extern,
    Any,
    Exn,
}

/// One field (struct) or the element (array): a typed slot at a byte offset within the body.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Scalar { offset: usize, kind: ScalarKind },
    Ref { offset: usize, kind: RefKind },
}

impl Slot {
    pub(crate) fn offset(self) -> usize {
        match self {
            Slot::Scalar { offset, .. } | Slot::Ref { offset, .. } => offset,
        }
    }

    pub(crate) fn width(self) -> usize {
        match self {
            Slot::Scalar { kind, .. } => kind.width(),
            Slot::Ref { .. } => REF_WIDTH,
        }
    }
}

/// The packed byte layout of a struct or array type.
#[derive(Clone, Debug)]
pub(crate) enum Layout {
    Struct(StructLayout),
    Array(ArrayLayout),
}

/// The packed byte layout of a struct type: each field at a precomputed offset; `size` is the
/// total body length.
#[derive(Clone, Debug)]
pub(crate) struct StructLayout {
    fields: Box<[Slot]>,
    size: usize,
}

/// The packed byte layout of an array type: a homogeneous element repeated. `stride` is the
/// element width (`elem` carries a dummy offset 0 — each element `k` lives at `k * stride`).
#[derive(Copy, Clone, Debug)]
pub(crate) struct ArrayLayout {
    elem: Slot,
    stride: usize,
}

impl Layout {
    /// Builds the layout for an aggregate body (returns `None` for function types, which are
    /// never heap-allocated).
    pub(crate) fn from_body(body: &CompositeBody) -> Option<Layout> {
        match body {
            CompositeBody::Func { .. } => None,
            CompositeBody::Struct(fields) => Some(Layout::Struct(StructLayout::of(
                fields.iter().map(|f| |offset| field_slot(f, offset)),
            ))),
            CompositeBody::Array(f) => Some(Layout::Array(ArrayLayout::of(field_slot(f, 0)))),
        }
    }

    /// Builds a struct layout from public field descriptors (host-built `StructType`).
    pub(crate) fn for_struct(fields: &[crate::value::FieldType]) -> StructLayout {
        StructLayout::of(fields.iter().map(|f| |offset| pub_field_slot(f, offset)))
    }

    /// Builds an array layout from a public element descriptor (host-built `ArrayType`).
    pub(crate) fn for_array(field: &crate::value::FieldType) -> ArrayLayout {
        ArrayLayout::of(pub_field_slot(field, 0))
    }

    /// The struct layout, or `None` for an array.
    pub(crate) fn as_struct(&self) -> Option<&StructLayout> {
        match self {
            Layout::Struct(layout) => Some(layout),
            Layout::Array(_) => None,
        }
    }

    /// The array layout, or `None` for a struct.
    pub(crate) fn as_array(&self) -> Option<ArrayLayout> {
        match self {
            Layout::Array(layout) => Some(*layout),
            Layout::Struct(_) => None,
        }
    }
}

impl StructLayout {
    /// Lays the fields out back to back; each item builds its slot at the offset it is given.
    fn of<F: FnOnce(usize) -> Slot>(slot_at: impl Iterator<Item = F>) -> StructLayout {
        let mut size = 0;
        let fields = slot_at
            .map(|slot_at| {
                let slot = slot_at(size);
                size += slot.width();
                slot
            })
            .collect();
        StructLayout { fields, size }
    }

    pub(crate) fn fields(&self) -> &[Slot] {
        &self.fields
    }

    /// The slot of field `i`, or `None` if out of range.
    pub(crate) fn field(&self, i: usize) -> Option<Slot> {
        self.fields.get(i).copied()
    }

    /// Total byte size of a struct body.
    pub(crate) fn size(&self) -> usize {
        self.size
    }
}

impl ArrayLayout {
    fn of(elem: Slot) -> ArrayLayout {
        ArrayLayout {
            elem,
            stride: elem.width(),
        }
    }

    /// The element slot at index `i` (offset = `i * stride`).
    pub(crate) fn elem_at(self, i: usize) -> Slot {
        with_offset(self.elem, i * self.stride)
    }

    pub(crate) fn stride(self) -> usize {
        self.stride
    }

    /// Total byte size of a body holding `len` elements.
    pub(crate) fn body_size(self, len: usize) -> usize {
        len * self.stride
    }
}

fn field_slot(f: &IrField, offset: usize) -> Slot {
    match &f.storage {
        IrStorage::I8 => Slot::Scalar {
            offset,
            kind: ScalarKind::I8,
        },
        IrStorage::I16 => Slot::Scalar {
            offset,
            kind: ScalarKind::I16,
        },
        IrStorage::Val(v) => val_slot(v, offset),
    }
}

fn val_slot(v: &IrVal, offset: usize) -> Slot {
    let kind = match v {
        IrVal::I32 => ScalarKind::I32,
        IrVal::I64 => ScalarKind::I64,
        IrVal::F32 => ScalarKind::F32,
        IrVal::F64 => ScalarKind::F64,
        IrVal::V128 => ScalarKind::V128,
        IrVal::Ref { heap, .. } => {
            return Slot::Ref {
                offset,
                kind: ref_kind(heap),
            }
        }
    };
    Slot::Scalar { offset, kind }
}

/// The reference hierarchy of a heap type (mirrors `Val::null_for_heap`).
fn ref_kind(heap: &IrHeap) -> RefKind {
    use super::AggKind;
    match heap {
        IrHeap::Func | IrHeap::NoFunc | IrHeap::Concrete(_, AggKind::Func) => RefKind::Func,
        IrHeap::Extern | IrHeap::NoExtern => RefKind::Extern,
        IrHeap::Exn | IrHeap::NoExn => RefKind::Exn,
        _ => RefKind::Any,
    }
}

/// Maps a public `FieldType` (host descriptor) to a slot at `offset`.
fn pub_field_slot(f: &crate::value::FieldType, offset: usize) -> Slot {
    use crate::value::StorageType;
    match f.element_type() {
        StorageType::I8 => Slot::Scalar {
            offset,
            kind: ScalarKind::I8,
        },
        StorageType::I16 => Slot::Scalar {
            offset,
            kind: ScalarKind::I16,
        },
        StorageType::ValType(v) => pub_val_slot(v, offset),
    }
}

fn pub_val_slot(v: &crate::value::ValType, offset: usize) -> Slot {
    use crate::value::ValType;
    let kind = match v {
        ValType::I32 => ScalarKind::I32,
        ValType::I64 => ScalarKind::I64,
        ValType::F32 => ScalarKind::F32,
        ValType::F64 => ScalarKind::F64,
        ValType::V128 => ScalarKind::V128,
        ValType::Ref(rt) => {
            return Slot::Ref {
                offset,
                kind: pub_ref_kind(rt.heap_type()),
            }
        }
    };
    Slot::Scalar { offset, kind }
}

/// The reference hierarchy of a public heap type (mirrors `ref_kind` over `IrHeap`).
fn pub_ref_kind(heap: &crate::value::HeapType) -> RefKind {
    use crate::value::HeapType as H;
    match heap {
        H::Func | H::NoFunc | H::ConcreteFunc(_) => RefKind::Func,
        H::Extern | H::NoExtern => RefKind::Extern,
        H::Exn | H::NoExn => RefKind::Exn,
        _ => RefKind::Any,
    }
}

fn with_offset(slot: Slot, offset: usize) -> Slot {
    match slot {
        Slot::Scalar { kind, .. } => Slot::Scalar { offset, kind },
        Slot::Ref { kind, .. } => Slot::Ref { offset, kind },
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
