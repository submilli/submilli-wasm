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
    /// A struct: each field at a precomputed offset; `size` is the total body length.
    Struct { fields: Box<[Slot]>, size: usize },
    /// An array: a homogeneous element repeated; `stride` is the element width (`elem` carries a
    /// dummy offset 0 — each element `k` lives at `k * stride`).
    Array { elem: Slot, stride: usize },
}

impl Layout {
    /// Builds the layout for an aggregate body (returns `None` for function types, which are
    /// never heap-allocated).
    pub(crate) fn from_body(body: &CompositeBody) -> Option<Layout> {
        match body {
            CompositeBody::Func { .. } => None,
            CompositeBody::Struct(fields) => {
                let mut offset = 0;
                let slots: Vec<Slot> = fields
                    .iter()
                    .map(|f| {
                        let slot = field_slot(f, offset);
                        offset += slot.width();
                        slot
                    })
                    .collect();
                Some(Layout::Struct {
                    fields: slots.into_boxed_slice(),
                    size: offset,
                })
            }
            CompositeBody::Array(f) => {
                let elem = field_slot(f, 0);
                Some(Layout::Array {
                    elem,
                    stride: elem.width(),
                })
            }
        }
    }

    /// The slot of struct field `i` (panics for arrays / out of range — callers gate by kind).
    pub(crate) fn field(&self, i: usize) -> Slot {
        match self {
            Layout::Struct { fields, .. } => fields[i],
            Layout::Array { .. } => unreachable!("field() on an array layout"),
        }
    }

    /// The element slot at index `i` of an array (offset = `i * stride`).
    pub(crate) fn elem_at(&self, i: usize) -> Slot {
        match self {
            Layout::Array { elem, stride } => with_offset(*elem, i * stride),
            Layout::Struct { .. } => unreachable!("elem_at() on a struct layout"),
        }
    }

    /// The element stride of an array layout.
    pub(crate) fn stride(&self) -> usize {
        match self {
            Layout::Array { stride, .. } => *stride,
            Layout::Struct { .. } => unreachable!("stride() on a struct layout"),
        }
    }

    /// Total byte size of a body holding `len` elements (`len` ignored for structs).
    pub(crate) fn body_size(&self, len: usize) -> usize {
        match self {
            Layout::Struct { size, .. } => *size,
            Layout::Array { stride, .. } => len * stride,
        }
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
        IrStorage::Val(IrVal::Ref { heap, .. }) => Slot::Ref {
            offset,
            kind: ref_kind(heap),
        },
        IrStorage::Val(v) => Slot::Scalar {
            offset,
            kind: num_kind(v),
        },
    }
}

fn num_kind(v: &IrVal) -> ScalarKind {
    match v {
        IrVal::I32 => ScalarKind::I32,
        IrVal::I64 => ScalarKind::I64,
        IrVal::F32 => ScalarKind::F32,
        IrVal::F64 => ScalarKind::F64,
        IrVal::V128 => ScalarKind::V128,
        IrVal::Ref { .. } => unreachable!("ref handled as a Ref slot"),
    }
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

fn with_offset(slot: Slot, offset: usize) -> Slot {
    match slot {
        Slot::Scalar { kind, .. } => Slot::Scalar { offset, kind },
        Slot::Ref { kind, .. } => Slot::Ref { offset, kind },
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
