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
    decode, encode, refkind_of_heap, refkind_of_irheap, stack_slot_for_field, Cell, RefTag,
    SLOT_BYTES,
};

/// Direct cell → `Val` for the host boundary: one match on the type instead of the layered
/// GC-slot codec (three nested matches), which was measurable at per-call frequency.
/// Non-scalars fall back to the generic [`decode`].
#[inline]
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
#[inline]
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
use crate::error::InternalError;
use crate::store::{read_slot, NULL_REF};
use crate::value::{Val, ValType};
use crate::Result;

impl Execution {
    fn expected_tag(ty: &ValType) -> RefTag {
        match ty {
            ValType::Ref(reference) => RefTag::of_refkind(refkind_of_heap(reference.heap_type())),
            _ => RefTag::NONE,
        }
    }

    fn operand_floor(&self) -> Result<usize> {
        let frame = self
            .frames
            .last()
            .ok_or(InternalError::FrameStack("missing current frame"))?;
        Ok(frame.operand_base as usize)
    }

    fn ensure_operands(&self, needed: usize) -> Result<usize> {
        if self.values.len() != self.shadow.len() {
            return Err(InternalError::OperandStack("value/shadow length mismatch").into());
        }
        let floor = self.operand_floor()?;
        let available = self
            .values
            .len()
            .checked_sub(floor)
            .ok_or(InternalError::OperandStack(
                "stack is below the current frame floor",
            ))?;
        if available < needed {
            return Err(InternalError::OperandStack("operand stack underflow").into());
        }
        Ok(self.values.len() - needed)
    }

    fn ensure_scalar_operands(&self, needed: usize) -> Result<usize> {
        let base = self.ensure_operands(needed)?;
        if self.shadow[base..].iter().any(|tag| *tag != RefTag::NONE) {
            return Err(
                InternalError::OperandStack("numeric operation found a reference tag").into(),
            );
        }
        Ok(base)
    }

    #[inline]
    fn pop_scalar(&mut self) -> Result<Cell> {
        let base = self.ensure_scalar_operands(1)?;
        let cell = self.values[base];
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(cell)
    }

    /// Pushes a scalar cell (shadow tag `NONE`) straight from its bytes — the arithmetic hot
    /// path, skipping the `Val` round-trip through the GC slot codec that [`push`] pays.
    #[inline]
    fn push_scalar<const N: usize>(&mut self, bytes: [u8; N]) {
        self.shadow.push(RefTag::NONE);
        self.values.push(Cell::of_bytes(bytes));
    }

    #[inline]
    pub(super) fn push_i32(&mut self, v: i32) {
        self.push_scalar(v.to_le_bytes());
    }

    #[inline]
    pub(super) fn push_i64(&mut self, v: i64) {
        self.push_scalar(v.to_le_bytes());
    }

    #[inline]
    pub(super) fn push_f32(&mut self, v: f32) {
        self.push_scalar(v.to_bits().to_le_bytes());
    }

    #[inline]
    pub(super) fn push_f64(&mut self, v: f64) {
        self.push_scalar(v.to_bits().to_le_bytes());
    }

    /// Pushes a local's default value straight as a cell (locals init on every call): scalars and
    /// `v128` default to all-zero bits, references to the null handle with their hierarchy tag —
    /// exactly what `push(Val::default_for(ty))` produces, minus the `Val` round-trip.
    #[inline]
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
    #[inline]
    pub(super) fn push_f32_bits(&mut self, bits: u32) {
        self.push_scalar(bits.to_le_bytes());
    }

    #[inline]
    pub(super) fn push_f64_bits(&mut self, bits: u64) {
        self.push_scalar(bits.to_le_bytes());
    }

    /// In-place binary op over the top two cells: the result overwrites the first operand's slot
    /// and the stack shrinks by one — one bounds region, no pop/push round-trips. For scalar ops
    /// only (operand and result shadow tags are all `NONE`, so the shadow just shrinks).
    #[inline]
    pub(super) fn binop_cells(&mut self, f: impl FnOnce(Cell, Cell) -> Cell) -> Result<()> {
        let base = self.ensure_scalar_operands(2)?;
        let n = base + 2;
        self.values[n - 2] = f(self.values[n - 2], self.values[n - 1]);
        self.values.truncate(n - 1);
        self.shadow.truncate(n - 1);
        Ok(())
    }

    /// Fallible [`binop_cells`](Self::binop_cells) (div/rem trap paths). The stack is adjusted
    /// only on success — a trapping op leaves it to the unwinder.
    #[inline]
    pub(super) fn binop_cells_try(
        &mut self,
        f: impl FnOnce(Cell, Cell) -> crate::Result<Cell>,
    ) -> crate::Result<()> {
        let base = self.ensure_scalar_operands(2)?;
        let n = base + 2;
        self.values[n - 2] = f(self.values[n - 2], self.values[n - 1])?;
        self.values.truncate(n - 1);
        self.shadow.truncate(n - 1);
        Ok(())
    }

    /// In-place unary op over the top cell — no stack movement at all. Scalar ops only.
    #[inline]
    pub(super) fn unop_cell(&mut self, f: impl FnOnce(Cell) -> Cell) -> Result<()> {
        let base = self.ensure_scalar_operands(1)?;
        let n = base + 1;
        self.values[n - 1] = f(self.values[n - 1]);
        Ok(())
    }

    #[inline]
    pub(super) fn pop(&mut self) -> Result<Cell> {
        let base = self.ensure_operands(1)?;
        let cell = self.values[base];
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(cell)
    }

    /// Pops a cell together with its root-shadow tag (for type-agnostic moves that must carry the
    /// reference hierarchy: `select`, `br_on_null`/`br_on_non_null`).
    #[inline]
    pub(super) fn pop_tagged(&mut self) -> Result<(Cell, RefTag)> {
        let base = self.ensure_operands(1)?;
        let pair = (self.values[base], self.shadow[base]);
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(pair)
    }

    #[inline]
    pub(super) fn push(&mut self, v: Val) {
        self.shadow.push(RefTag::of_val(&v));
        self.values.push(encode(v));
    }

    /// Pushes an already-encoded cell with its known shadow tag (type-agnostic moves:
    /// `local.get`, `select`, `br_on_null`).
    #[inline]
    pub(super) fn push_cell(&mut self, cell: Cell, tag: RefTag) {
        self.shadow.push(tag);
        self.values.push(cell);
    }

    /// The cell + shadow tag at operand index `i` (a `local.get` source).
    #[inline]
    pub(super) fn cell_at(&self, i: usize) -> Result<(Cell, RefTag)> {
        if self.values.len() != self.shadow.len() {
            return Err(InternalError::OperandStack("value/shadow length mismatch").into());
        }
        let cell = self
            .values
            .get(i)
            .copied()
            .ok_or(InternalError::OperandStack("local index out of bounds"))?;
        Ok((cell, self.shadow[i]))
    }

    /// Writes a cell + shadow tag at operand index `i` (a `local.set`/`local.tee` target).
    #[inline]
    pub(super) fn set_cell(&mut self, i: usize, cell: Cell, tag: RefTag) -> Result<()> {
        if self.values.len() != self.shadow.len() || i >= self.values.len() {
            return Err(InternalError::OperandStack("local index out of bounds").into());
        }
        self.values[i] = cell;
        self.shadow[i] = tag;
        Ok(())
    }

    /// The top operand as an `i32` without popping (an `array.new*` count peek).
    #[inline]
    pub(super) fn top_i32(&self) -> Result<i32> {
        let base = self.ensure_scalar_operands(1)?;
        Ok(self.values[base].unwrap_i32())
    }

    /// The top cell + shadow tag without popping (a `local.tee` source).
    #[inline]
    pub(super) fn top_cell(&self) -> Result<(Cell, RefTag)> {
        let base = self.ensure_operands(1)?;
        Ok((self.values[base], self.shadow[base]))
    }

    #[inline]
    pub(super) fn pop_i32(&mut self) -> Result<i32> {
        Ok(self.pop_scalar()?.unwrap_i32())
    }

    #[inline]
    pub(super) fn pop_i64(&mut self) -> Result<i64> {
        Ok(self.pop_scalar()?.unwrap_i64())
    }

    #[inline]
    pub(super) fn pop_f32(&mut self) -> Result<f32> {
        Ok(self.pop_scalar()?.unwrap_f32())
    }

    #[inline]
    pub(super) fn pop_f64(&mut self) -> Result<f64> {
        Ok(self.pop_scalar()?.unwrap_f64())
    }

    #[cfg(feature = "simd")]
    #[inline]
    pub(super) fn pop_v128_cell(&mut self) -> Result<Cell> {
        self.pop_scalar()
    }

    /// Pops two scalar `i32` operands transactionally for the fused compare-and-branch op.
    #[inline]
    pub(super) fn pop_i32_pair(&mut self) -> Result<(i32, i32)> {
        let base = self.ensure_scalar_operands(2)?;
        let pair = (
            self.values[base].unwrap_i32(),
            self.values[base + 1].unwrap_i32(),
        );
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(pair)
    }

    /// Pops an index/length/address operand, widening to `u64`. `is_64` (from the target
    /// memory/table's type) selects the width â there is no runtime tag to read (#42).
    #[inline]
    pub(super) fn pop_index(&mut self, is_64: bool) -> Result<u64> {
        let cell = self.pop_scalar()?;
        Ok(if is_64 {
            cell.unwrap_i64() as u64
        } else {
            u64::from(cell.unwrap_i32() as u32)
        })
    }

    /// Pushes a size/grow result as i64 for a 64-bit memory/table, else i32 (#42).
    #[inline]
    pub(super) fn push_index(&mut self, is_64: bool, v: u64) {
        self.push(if is_64 {
            Val::I64(v as i64)
        } else {
            Val::I32(v as u32 as i32)
        });
    }

    /// Pops a reference operand of a statically-known hierarchy (null â the typed null `Val`).
    #[inline]
    pub(super) fn pop_ref(&mut self, kind: RefKind) -> Result<Val> {
        let base = self.ensure_operands(1)?;
        if self.shadow[base] != RefTag::of_refkind(kind) {
            return Err(
                InternalError::OperandStack("reference tag does not match operand type").into(),
            );
        }
        let cell = self.pop()?;
        Ok(read_slot(Slot::Ref { offset: 0, kind }, cell.bytes()))
    }

    /// Pops a reference when the precise hierarchy is carried only by the shadow tag.
    #[inline]
    pub(super) fn pop_tagged_ref(&mut self) -> Result<(Cell, RefTag)> {
        let base = self.ensure_operands(1)?;
        if self.shadow[base].refkind().is_none() {
            return Err(
                InternalError::OperandStack("reference operation found a scalar tag").into(),
            );
        }
        let pair = (self.values[base], self.shadow[base]);
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(pair)
    }

    #[inline]
    pub(super) fn pop_anyref(&mut self) -> Result<Val> {
        self.pop_ref(RefKind::Any)
    }

    /// Pops the value for a GC field/element write: decoded to the field's hierarchy/scalar kind
    /// (the caller's `write_slot` re-narrows packed `i8`/`i16` into the body).
    #[inline]
    pub(super) fn pop_val_for(&mut self, field: Slot) -> Result<Val> {
        let stack_slot = stack_slot_for_field(field);
        let base = self.ensure_operands(1)?;
        let expected = match stack_slot {
            Slot::Ref { kind, .. } => RefTag::of_refkind(kind),
            Slot::Scalar { .. } => RefTag::NONE,
        };
        if self.shadow[base] != expected {
            return Err(
                InternalError::OperandStack("operand tag does not match field type").into(),
            );
        }
        let cell = self.pop()?;
        Ok(read_slot(stack_slot, cell.bytes()))
    }

    /// Pops one value using its declared type, validating the shadow before mutation.
    #[inline]
    pub(super) fn pop_typed(&mut self, ty: &ValType) -> Result<Val> {
        let base = self.ensure_operands(1)?;
        if self.shadow[base] != Self::expected_tag(ty) {
            return Err(
                InternalError::OperandStack("operand tag does not match value type").into(),
            );
        }
        let value = decode_val(self.values[base], ty);
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(value)
    }

    /// Pops `select`'s two same-typed values and scalar condition as one transaction.
    #[inline]
    pub(super) fn pop_select(&mut self) -> Result<(Cell, RefTag)> {
        let base = self.ensure_operands(3)?;
        let a_tag = self.shadow[base];
        let b_tag = self.shadow[base + 1];
        if a_tag != b_tag || self.shadow[base + 2] != RefTag::NONE {
            return Err(InternalError::OperandStack("select operand tag mismatch").into());
        }
        let selected = if self.values[base + 2].unwrap_i32() != 0 {
            (self.values[base], a_tag)
        } else {
            (self.values[base + 1], b_tag)
        };
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(selected)
    }

    /// Splits off the top `tys.len()` operand cells and decodes them to `Val`s (host-call args).
    #[inline]
    pub(super) fn pop_params(&mut self, tys: &[ValType]) -> Result<Vec<Val>> {
        let mut out = Vec::with_capacity(tys.len());
        self.pop_params_into(tys, &mut out)?;
        Ok(out)
    }

    /// Alloc-free [`pop_params`](Self::pop_params): decodes the top `tys.len()` operands into
    /// `out` (the reused host-call scratch buffer) and pops them.
    #[inline]
    pub(super) fn pop_params_into(&mut self, tys: &[ValType], out: &mut Vec<Val>) -> Result<()> {
        let base = self.ensure_operands(tys.len())?;
        for (tag, ty) in self.shadow[base..].iter().zip(tys) {
            if *tag != Self::expected_tag(ty) {
                return Err(InternalError::OperandStack("host parameter tag mismatch").into());
            }
        }
        out.extend(
            self.values[base..]
                .iter()
                .zip(tys)
                .map(|(&c, t)| decode(c, t)),
        );
        self.values.truncate(base);
        self.shadow.truncate(base);
        Ok(())
    }

    /// Checks the completed call's exact result count and shadow tags before either stack is
    /// mutated. The signature is the only type information available for the untyped cells.
    pub(super) fn validate_result_tags(&self, base: usize, tys: &[ValType]) -> Result<()> {
        if self.values.len() != self.shadow.len() || base > self.values.len() {
            return Err(InternalError::OperandStack("invalid result stack boundary").into());
        }
        let tags = &self.shadow[base..];
        if tags.len() != tys.len() {
            return Err(InternalError::ResultShape("wrong number of execution results").into());
        }
        if tags
            .iter()
            .zip(tys)
            .any(|(tag, ty)| *tag != Self::expected_tag(ty))
        {
            return Err(InternalError::ResultShape("execution result tag mismatch").into());
        }
        Ok(())
    }

    /// Encodes and pushes host-call results back onto the operand stack.
    #[inline]
    pub(super) fn push_results(&mut self, results: Vec<Val>) {
        self.push_results_slice(&results);
    }

    /// Borrowing [`push_results`](Self::push_results) (the reused scratch buffer survives).
    /// Indexed loops for the same reason as [`pop_params_into`](Self::pop_params_into).
    #[inline]
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
    #[inline]
    pub(crate) fn operand_roots(&self) -> Result<Vec<(u32, RefKind)>> {
        if self.values.len() != self.shadow.len() {
            return Err(InternalError::OperandStack(
                "value/shadow length mismatch during root scan",
            )
            .into());
        }
        Ok(self
            .values
            .iter()
            .zip(&self.shadow)
            .filter_map(|(cell, tag)| {
                let kind = tag.refkind()?;
                (!cell.is_null()).then(|| (cell.handle(), kind))
            })
            .collect())
    }
}
