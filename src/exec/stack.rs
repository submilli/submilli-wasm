//! Operand-stack operations on [`Execution`]: typed pushes/pops over the untyped [`Cell`] slots,
//! with the GC root shadow (`RefTag` per slot) maintained in lockstep. The `Cell`/`RefTag` types
//! and the `Val` codec live in [`super::cell`].

// Little-endian (un)packing is intentional narrowing; operand-stack / local indexing is
// bounds-guaranteed by validation (stack height - #33).
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing
)]

use super::cell::{
    decode, encode, refkind_of_irheap, stack_slot_for_field, Cell, RefTag, SLOT_BYTES,
};

/// Direct cell → `Val` for the host boundary: one match on the type instead of the layered
/// GC-slot codec (three nested matches), which was measurable at per-call frequency.
/// Non-scalars fall back to the generic [`decode`].
fn decode_val(c: Cell, t: &ValType) -> Val {
    match t {
        ValType::I32 => Val::I32(c.unwrap_i32()),
        ValType::I64 => Val::I64(c.unwrap_i64()),
        ValType::F32 => Val::F32(c.unwrap_f32().to_bits()),
        ValType::F64 => Val::F64(c.unwrap_f64().to_bits()),
        _ => decode(c, t),
    }
}

/// Direct `Val` → cell (see [`decode_val`]); non-scalars fall back to the generic [`encode`].
fn encode_val(v: Val) -> Cell {
    match v {
        Val::I32(x) => Cell::from_i32(x),
        Val::I64(x) => Cell::from_i64(x),
        Val::F32(bits) => Cell::of_bytes(bits.to_le_bytes()),
        Val::F64(bits) => Cell::of_bytes(bits.to_le_bytes()),
        _ => encode(v),
    }
}
use super::Execution;
use crate::canon::{IrVal, RefKind, Slot};
use crate::store::{read_slot, NULL_REF};
use crate::value::{Val, ValType};

impl Execution {
    /// Pushes a scalar cell (shadow tag `NONE`) straight from its bytes — the arithmetic hot
    /// path, skipping the `Val` round-trip through the GC slot codec that [`push`] pays.
    fn push_scalar<const N: usize>(&mut self, bytes: [u8; N]) {
        self.shadow.push(RefTag::NONE);
        self.values.push(Cell::of_bytes(bytes));
    }

    pub(super) fn push_i32(&mut self, v: i32) {
        self.push_scalar(v.to_le_bytes());
    }

    pub(super) fn push_i64(&mut self, v: i64) {
        self.push_scalar(v.to_le_bytes());
    }

    pub(super) fn push_f32(&mut self, v: f32) {
        self.push_scalar(v.to_bits().to_le_bytes());
    }

    pub(super) fn push_f64(&mut self, v: f64) {
        self.push_scalar(v.to_bits().to_le_bytes());
    }

    /// Pushes a local's default value straight as a cell (locals init on every call): scalars and
    /// `v128` default to all-zero bits, references to the null handle with their hierarchy tag —
    /// exactly what `push(Val::default_for(ty))` produces, minus the `Val` round-trip.
    pub(super) fn push_default(&mut self, ty: &IrVal) {
        if let IrVal::Ref { heap, .. } = ty {
            self.shadow
                .push(RefTag::of_refkind(refkind_of_irheap(heap)));
            self.values.push(Cell::of_bytes(NULL_REF.to_le_bytes()));
        } else {
            self.push_scalar([0u8; SLOT_BYTES]);
        }
    }

    /// Raw-bits pushes for `f32.const`/`f64.const`, whose `Op` immediates are already bit patterns.
    pub(super) fn push_f32_bits(&mut self, bits: u32) {
        self.push_scalar(bits.to_le_bytes());
    }

    pub(super) fn push_f64_bits(&mut self, bits: u64) {
        self.push_scalar(bits.to_le_bytes());
    }

    /// In-place binary op over the top two cells: the result overwrites the first operand's slot
    /// and the stack shrinks by one — one bounds region, no pop/push round-trips. For scalar ops
    /// only (operand and result shadow tags are all `NONE`, so the shadow just shrinks).
    pub(super) fn binop_cells(&mut self, f: impl FnOnce(Cell, Cell) -> Cell) {
        let n = self.values.len();
        self.values[n - 2] = f(self.values[n - 2], self.values[n - 1]);
        self.values.truncate(n - 1);
        self.shadow.truncate(n - 1);
    }

    /// Fallible [`binop_cells`](Self::binop_cells) (div/rem trap paths). The stack is adjusted
    /// only on success — a trapping op leaves it to the unwinder.
    pub(super) fn binop_cells_try(
        &mut self,
        f: impl FnOnce(Cell, Cell) -> crate::Result<Cell>,
    ) -> crate::Result<()> {
        let n = self.values.len();
        self.values[n - 2] = f(self.values[n - 2], self.values[n - 1])?;
        self.values.truncate(n - 1);
        self.shadow.truncate(n - 1);
        Ok(())
    }

    /// In-place unary op over the top cell — no stack movement at all. Scalar ops only.
    pub(super) fn unop_cell(&mut self, f: impl FnOnce(Cell) -> Cell) {
        let n = self.values.len();
        self.values[n - 1] = f(self.values[n - 1]);
    }

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
    /// memory/table's type) selects the width â there is no runtime tag to read (#42).
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

    /// Pops a reference operand of a statically-known hierarchy (null â the typed null `Val`).
    pub(super) fn pop_ref(&mut self, kind: RefKind) -> Val {
        let cell = self.pop();
        read_slot(Slot::Ref { offset: 0, kind }, cell.bytes())
    }

    pub(super) fn pop_anyref(&mut self) -> Val {
        self.pop_ref(RefKind::Any)
    }

    /// Pops the value for a GC field/element write: decoded to the field's hierarchy/scalar kind
    /// (the caller's `write_slot` re-narrows packed `i8`/`i16` into the body).
    pub(super) fn pop_val_for(&mut self, field: Slot) -> Val {
        let cell = self.pop();
        read_slot(stack_slot_for_field(field), cell.bytes())
    }

    /// Splits off the top `tys.len()` operand cells and decodes them to `Val`s (host-call args).
    pub(super) fn pop_params(&mut self, tys: &[ValType]) -> Vec<Val> {
        let mut out = Vec::with_capacity(tys.len());
        self.pop_params_into(tys, &mut out);
        out
    }

    /// Alloc-free [`pop_params`](Self::pop_params): decodes the top `tys.len()` operands into
    /// `out` (the reused host-call scratch buffer) and pops them.
    pub(super) fn pop_params_into(&mut self, tys: &[ValType], out: &mut Vec<Val>) {
        let base = self.values.len() - tys.len();
        out.extend(
            self.values[base..]
                .iter()
                .zip(tys)
                .map(|(&c, t)| decode(c, t)),
        );
        self.values.truncate(base);
        self.shadow.truncate(base);
    }

    /// Encodes and pushes host-call results back onto the operand stack.
    pub(super) fn push_results(&mut self, results: Vec<Val>) {
        self.push_results_slice(&results);
    }

    /// Borrowing [`push_results`](Self::push_results) (the reused scratch buffer survives).
    /// Indexed loops for the same reason as [`pop_params_into`](Self::pop_params_into).
    pub(super) fn push_results_slice(&mut self, results: &[Val]) {
        self.shadow.reserve(results.len());
        self.values.reserve(results.len());
        for v in results {
            self.shadow.push(RefTag::of_val(v));
            self.values.push(encode_val(*v));
        }
    }

    /// Iterates the live operand/local roots: each `(handle, RefKind)` for a non-null reference
    /// slot, recovered from the root shadow. Drives the tracing collector's stack-root scan (#27g).
    pub(crate) fn operand_roots(&self) -> impl Iterator<Item = (u32, RefKind)> + '_ {
        self.values
            .iter()
            .zip(&self.shadow)
            .filter_map(|(cell, tag)| {
                let kind = tag.refkind()?;
                (!cell.is_null()).then(|| (cell.handle(), kind))
            })
    }
}
