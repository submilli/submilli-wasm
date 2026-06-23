//! The operand-stack cell: a fixed-width *untyped* byte slot replacing the tagged `Val` on the
//! operand stack (ARCHITECTURE §7). wasm is statically typed post-validation, so the slot carries
//! no tag — `8` bytes with the `simd` feature off, `16` with it on (only `v128` needs 16; every
//! other value, including the `u32` reference handles, fits in 8). Encoding reuses the GC-body
//! codec (`store::{read_slot, write_slot}`, offset-0 slots): scalars little-endian, references a
//! 4-byte handle with the `NULL_REF` sentinel. No `unsafe`, no alignment requirement.

// Little-endian (un)packing is intentional narrowing / sign reinterpretation.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]

use super::Execution;
use crate::canon::{AggKind, IrHeap, RefKind, ScalarKind, Slot};
use crate::store::{read_slot, write_slot, NULL_REF};
#[cfg(feature = "simd")]
use crate::value::V128;
use crate::value::{HeapType, Val, ValType};

/// Byte width of one operand-stack cell. 16 holds a `v128`; 8 covers every other value.
#[cfg(feature = "simd")]
pub(super) const SLOT_BYTES: usize = 16;
#[cfg(not(feature = "simd"))]
pub(super) const SLOT_BYTES: usize = 8;

/// One operand-stack slot: raw bytes, type known from validation (not stored). `Copy` so the
/// `copy_within` branch/return fixups and local moves are plain byte copies.
#[derive(Copy, Clone)]
pub(super) struct Cell([u8; SLOT_BYTES]);

/// The reference-hierarchy tag stored in the operand-stack root shadow (one per slot, `Copy` so it
/// moves with the cell stack's `copy_within`). [`NONE`](RefTag::NONE) marks a non-reference slot;
/// the others say which arena a live handle points into, so the tracing collector decodes and
/// traces it (#27g). A compact newtype, not the `RefKind` enum, so the shadow stays a byte vector.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) struct RefTag(u8);

impl RefTag {
    pub(super) const NONE: RefTag = RefTag(0);
    const FUNC: RefTag = RefTag(1);
    const EXTERN: RefTag = RefTag(2);
    const ANY: RefTag = RefTag(3);
    const EXN: RefTag = RefTag(4);

    /// The shadow tag for a `Val` about to be pushed (its reference hierarchy, or `NONE`).
    pub(super) fn of_val(v: &Val) -> RefTag {
        match v {
            Val::FuncRef(_) => RefTag::FUNC,
            Val::ExternRef(_) => RefTag::EXTERN,
            Val::AnyRef(_) => RefTag::ANY,
            Val::ExnRef(_) => RefTag::EXN,
            _ => RefTag::NONE,
        }
    }

    /// The `RefKind` this tag denotes (for decoding the slot's handle), or `None` for a non-ref.
    pub(super) fn refkind(self) -> Option<RefKind> {
        match self {
            RefTag::FUNC => Some(RefKind::Func),
            RefTag::EXTERN => Some(RefKind::Extern),
            RefTag::ANY => Some(RefKind::Any),
            RefTag::EXN => Some(RefKind::Exn),
            _ => None,
        }
    }
}

impl Cell {
    fn lo<const N: usize>(self) -> [u8; N] {
        let mut out = [0u8; N];
        out.copy_from_slice(&self.0[..N]);
        out
    }

    pub(super) fn unwrap_i32(self) -> i32 {
        i32::from_le_bytes(self.lo())
    }

    pub(super) fn unwrap_i64(self) -> i64 {
        i64::from_le_bytes(self.lo())
    }

    pub(super) fn unwrap_f32(self) -> f32 {
        f32::from_bits(u32::from_le_bytes(self.lo()))
    }

    pub(super) fn unwrap_f64(self) -> f64 {
        f64::from_bits(u64::from_le_bytes(self.lo()))
    }

    #[cfg(feature = "simd")]
    pub(super) fn unwrap_v128(self) -> V128 {
        V128::from(u128::from_le_bytes(self.lo()))
    }

    /// The 4-byte reference handle (the cell is a reference by validation).
    pub(super) fn handle(self) -> u32 {
        u32::from_le_bytes(self.lo())
    }

    /// Whether this reference cell is null (the reserved `NULL_REF` handle).
    pub(super) fn is_null(self) -> bool {
        self.handle() == NULL_REF
    }
}

/// Encodes a `Val` into a fresh zeroed cell. The incoming `Val` is tagged, so it self-describes
/// the target slot kind; the `V128` arm is unreachable without the `simd` feature (validation
/// rejects the `v128` type, so an 8-byte cell is never asked to hold 16 bytes).
pub(super) fn encode(v: Val) -> Cell {
    let mut cell = Cell([0u8; SLOT_BYTES]);
    write_slot(slot_for_val(&v), &mut cell.0, v);
    cell
}

/// Decodes a cell back to a `Val` given its statically-known value type (globals + the host /
/// top-level call boundary, where the operand type comes from a signature, not the stack).
pub(super) fn decode(cell: Cell, ty: &ValType) -> Val {
    read_slot(slot_for_valtype(ty), &cell.0)
}

fn slot_for_val(v: &Val) -> Slot {
    match v {
        Val::I32(_) => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::I32,
        },
        Val::I64(_) => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::I64,
        },
        Val::F32(_) => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::F32,
        },
        Val::F64(_) => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::F64,
        },
        #[cfg(feature = "simd")]
        Val::V128(_) => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::V128,
        },
        #[cfg(not(feature = "simd"))]
        Val::V128(_) => unreachable!("v128 requires the simd feature"),
        Val::FuncRef(_) => Slot::Ref {
            offset: 0,
            kind: RefKind::Func,
        },
        Val::ExternRef(_) => Slot::Ref {
            offset: 0,
            kind: RefKind::Extern,
        },
        Val::AnyRef(_) => Slot::Ref {
            offset: 0,
            kind: RefKind::Any,
        },
        Val::ExnRef(_) => Slot::Ref {
            offset: 0,
            kind: RefKind::Exn,
        },
    }
}

fn slot_for_valtype(ty: &ValType) -> Slot {
    match ty {
        ValType::I32 => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::I32,
        },
        ValType::I64 => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::I64,
        },
        ValType::F32 => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::F32,
        },
        ValType::F64 => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::F64,
        },
        #[cfg(feature = "simd")]
        ValType::V128 => Slot::Scalar {
            offset: 0,
            kind: ScalarKind::V128,
        },
        #[cfg(not(feature = "simd"))]
        ValType::V128 => unreachable!("v128 requires the simd feature"),
        ValType::Ref(rt) => Slot::Ref {
            offset: 0,
            kind: refkind_of_heap(rt.heap_type()),
        },
    }
}

/// The cell kind for a GC field/element popped off the stack: the field's offset/packing is
/// irrelevant (the stack holds the unpacked `i32`/`i64`/… value), only its hierarchy matters.
fn stack_slot_for_field(field: Slot) -> Slot {
    match field {
        Slot::Scalar { kind, .. } => {
            let kind = match kind {
                ScalarKind::I8 | ScalarKind::I16 => ScalarKind::I32,
                k => k,
            };
            Slot::Scalar { offset: 0, kind }
        }
        Slot::Ref { kind, .. } => Slot::Ref { offset: 0, kind },
    }
}

/// The reference hierarchy of a heap type — selects which `Val`/`Ref` variant a stored handle
/// materializes into (mirrors `Val::null_for_heap`). Used to decode table-element refs.
pub(super) fn refkind_of_heap(heap: &HeapType) -> RefKind {
    match heap {
        HeapType::Func | HeapType::NoFunc | HeapType::ConcreteFunc(_) => RefKind::Func,
        HeapType::Extern | HeapType::NoExtern => RefKind::Extern,
        HeapType::Exn | HeapType::NoExn => RefKind::Exn,
        _ => RefKind::Any,
    }
}

/// The reference hierarchy of an IR heap type (the cast target). A `ref.test`/`ref.cast`/`br_on_cast`
/// operand shares the target's top type by validation, so this selects how to decode the operand.
pub(super) fn refkind_of_irheap(heap: &IrHeap) -> RefKind {
    match heap {
        IrHeap::Func | IrHeap::NoFunc | IrHeap::Concrete(_, AggKind::Func) => RefKind::Func,
        IrHeap::Extern | IrHeap::NoExtern => RefKind::Extern,
        IrHeap::Exn | IrHeap::NoExn => RefKind::Exn,
        _ => RefKind::Any,
    }
}

impl Execution {
    pub(super) fn pop(&mut self) -> Cell {
        self.shadow.pop();
        self.values.pop().expect("operand stack underflow")
    }

    /// Pops a cell together with its root-shadow tag (for type-agnostic moves that must carry the
    /// reference hierarchy: `select`, `br_on_null`/`br_on_non_null`).
    pub(super) fn pop_tagged(&mut self) -> (Cell, RefTag) {
        let tag = self.shadow.pop().expect("shadow underflow");
        (self.values.pop().expect("operand stack underflow"), tag)
    }

    pub(super) fn push(&mut self, v: Val) {
        self.shadow.push(RefTag::of_val(&v));
        self.values.push(encode(v));
    }

    /// Pushes an already-encoded cell with its known shadow tag (type-agnostic moves:
    /// `local.get`, `select`, `br_on_null`).
    pub(super) fn push_cell(&mut self, cell: Cell, tag: RefTag) {
        self.shadow.push(tag);
        self.values.push(cell);
    }

    /// The cell + shadow tag at operand index `i` (a `local.get` source).
    pub(super) fn cell_at(&self, i: usize) -> (Cell, RefTag) {
        (self.values[i], self.shadow[i])
    }

    /// Writes a cell + shadow tag at operand index `i` (a `local.set`/`local.tee` target).
    pub(super) fn set_cell(&mut self, i: usize, cell: Cell, tag: RefTag) {
        self.values[i] = cell;
        self.shadow[i] = tag;
    }

    /// The top operand as an `i32` without popping (an `array.new*` count peek).
    pub(super) fn top_i32(&self) -> i32 {
        self.values
            .last()
            .expect("operand stack underflow")
            .unwrap_i32()
    }

    /// The top cell + shadow tag without popping (a `local.tee` source).
    pub(super) fn top_cell(&self) -> (Cell, RefTag) {
        (
            *self.values.last().expect("operand stack underflow"),
            *self.shadow.last().expect("shadow underflow"),
        )
    }

    pub(super) fn pop_i32(&mut self) -> i32 {
        self.pop().unwrap_i32()
    }

    /// Pops an index/length/address operand, widening to `u64`. `is_64` (from the target
    /// memory/table's type) selects the width — there is no runtime tag to read (#42).
    pub(super) fn pop_index(&mut self, is_64: bool) -> u64 {
        let cell = self.pop();
        if is_64 {
            cell.unwrap_i64() as u64
        } else {
            u64::from(cell.unwrap_i32() as u32)
        }
    }

    /// Pushes a size/grow result as i64 for a 64-bit memory/table, else i32 (#42).
    pub(super) fn push_index(&mut self, is_64: bool, v: u64) {
        self.push(if is_64 {
            Val::I64(v as i64)
        } else {
            Val::I32(v as u32 as i32)
        });
    }

    /// Pops a reference operand of a statically-known hierarchy (null → the typed null `Val`).
    pub(super) fn pop_ref(&mut self, kind: RefKind) -> Val {
        let cell = self.pop();
        read_slot(Slot::Ref { offset: 0, kind }, &cell.0)
    }

    pub(super) fn pop_anyref(&mut self) -> Val {
        self.pop_ref(RefKind::Any)
    }

    /// Pops the value for a GC field/element write: decoded to the field's hierarchy/scalar kind
    /// (the caller's `write_slot` re-narrows packed `i8`/`i16` into the body).
    pub(super) fn pop_val_for(&mut self, field: Slot) -> Val {
        let cell = self.pop();
        read_slot(stack_slot_for_field(field), &cell.0)
    }

    /// Splits off the top `tys.len()` operand cells and decodes them to `Val`s (host-call args).
    pub(super) fn pop_params(&mut self, tys: &[ValType]) -> Vec<Val> {
        let cells = self.values.split_off(self.values.len() - tys.len());
        self.shadow.truncate(self.shadow.len() - tys.len());
        cells.iter().zip(tys).map(|(&c, t)| decode(c, t)).collect()
    }

    /// Encodes and pushes host-call results back onto the operand stack.
    pub(super) fn push_results(&mut self, results: Vec<Val>) {
        self.shadow.extend(results.iter().map(RefTag::of_val));
        self.values.extend(results.into_iter().map(encode));
    }

    /// Iterates the live operand/local roots: each `(handle, RefKind)` for a non-null reference
    /// slot, recovered from the root shadow. Drives the tracing collector's stack-root scan (#27g).
    pub(super) fn operand_roots(&self) -> impl Iterator<Item = (u32, RefKind)> + '_ {
        self.values
            .iter()
            .zip(&self.shadow)
            .filter_map(|(cell, tag)| {
                let kind = tag.refkind()?;
                (!cell.is_null()).then(|| (cell.handle(), kind))
            })
    }
}

#[cfg(test)]
#[path = "cell_tests.rs"]
mod tests;
