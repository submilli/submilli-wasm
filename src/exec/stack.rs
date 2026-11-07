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
#[cold]
fn lockstep_error() -> crate::Error {
    InternalError::OperandStack("value/shadow length mismatch").into()
}

#[cold]
fn underflow_error() -> crate::Error {
    InternalError::OperandStack("operand stack underflow").into()
}

#[cold]
fn slot_error() -> crate::Error {
    InternalError::OperandStack("operand slot out of range").into()
}

#[cold]
fn scalar_tag_error() -> crate::Error {
    InternalError::OperandStack("numeric operation found a reference tag").into()
}

/// Clears the root-shadow tags of the dead slots `range` (an empty range is a no-op).
fn clear_tags(shadow: &mut [RefTag], range: std::ops::Range<usize>) {
    if let Some(dead) = shadow.get_mut(range) {
        dead.fill(RefTag::NONE);
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

    /// Index of the lowest of the top `needed` operands of the current stack window (`sp`; see
    /// [`enter_window`](Self::enter_window)), refusing to pop below the current frame's
    /// floor or to proceed with the value/shadow stacks out of lockstep (both impossible for
    /// validated code; corrupt bytecode must still fail closed, not read the caller's operands).
    /// Kept to two compares against cached state: walking the frame stack here on every op once
    /// cost ~30% of CoreMark.
    #[allow(clippy::inline_always)] // called by nearly every op; must not stay an out-of-line call
    #[inline(always)]
    pub(super) fn ensure_operands(&self, needed: usize) -> Result<usize> {
        debug_assert_eq!(
            self.floor,
            self.frames
                .last()
                .map_or(usize::MAX, |frame| frame.operand_base as usize),
            "stale cached frame floor (a frame push/pop skipped `sync_floor`)"
        );
        let len = self.values.len();
        if len != self.shadow.len() {
            return Err(lockstep_error());
        }
        match self.sp.checked_sub(needed) {
            Some(base) if base >= self.floor && self.sp <= len => Ok(base),
            _ => Err(underflow_error()),
        }
    }

    /// [`ensure_operands`](Self::ensure_operands) for numeric ops, also refusing reference-tagged
    /// operands: the in-place helpers (`binop_cells`/`unop_cell`) keep the first operand's shadow
    /// tag, so a reference reaching a numeric op would leave a scalar result tagged as a GC root.
    /// Impossible for validated code; corrupt bytecode must fail closed.
    #[allow(clippy::inline_always)]
    #[inline(always)]
    fn ensure_scalar_operands(&self, needed: usize) -> Result<usize> {
        let base = self.ensure_operands(needed)?;
        // `needed` is a constant at every (inlined) call site: this unrolls to 1–2 byte compares.
        for i in base..base + needed {
            if self.tag_at(i)? != RefTag::NONE {
                return Err(scalar_tag_error());
            }
        }
        Ok(base)
    }

    /// The shadow tag at `i`. Fallible: a broken value/shadow invariant must surface as an error,
    /// never a panic.
    #[allow(clippy::inline_always)]
    #[inline(always)]
    fn tag_at(&self, i: usize) -> Result<RefTag> {
        self.shadow
            .get(i)
            .copied()
            .ok_or_else(|| InternalError::OperandStack("value/shadow length mismatch").into())
    }

    /// The shadow tags from `base` up (see [`tag_at`](Self::tag_at)).
    fn tags_from(&self, base: usize) -> Result<&[RefTag]> {
        self.shadow
            .get(base..)
            .ok_or_else(|| InternalError::OperandStack("value/shadow length mismatch").into())
    }

    #[inline]
    fn pop_scalar(&mut self) -> Result<Cell> {
        let base = self.ensure_scalar_operands(1)?;
        let cell = self.values[base];
        self.sp = base;
        Ok(cell)
    }

    /// Pushes onto the current stack window: writes slot `sp` and advances it. Infallible: a push
    /// past the buffer's end (impossible for compiled code — every static height, including
    /// block ends and catch payloads, counts toward the frame's size) extends it rather than
    /// failing.
    #[inline]
    fn push_slot(&mut self, cell: Cell, tag: RefTag) {
        let sp = self.sp;
        if let (Some(slot), Some(slot_tag)) = (self.values.get_mut(sp), self.shadow.get_mut(sp)) {
            *slot = cell;
            *slot_tag = tag;
        } else {
            self.values.push(cell);
            self.shadow.push(tag);
        }
        self.sp = self.values.len().min(sp.saturating_add(1));
    }

    /// Writes a local's default value to slot `at` (locals init on every call): scalars and
    /// `v128` default to all-zero bits, references to the null handle with their hierarchy tag —
    /// exactly what `Val::default_for(ty)` encodes, minus the `Val` round-trip.
    #[inline]
    pub(super) fn set_default(&mut self, at: usize, ty: &IrVal) -> Result<()> {
        if let IrVal::Ref { heap, .. } = ty {
            let tag = RefTag::of_refkind(refkind_of_irheap(heap));
            self.set_cell(at, Cell::of_bytes(NULL_REF.to_le_bytes()), tag)
        } else {
            self.set_scalar(at, Cell::of_bytes([0u8; SLOT_BYTES]))
        }
    }

    /// The scalar cell at absolute slot `at`, refusing a reference-tagged slot (a numeric op on a
    /// reference is impossible for validated code; corrupt bytecode must fail closed).
    #[allow(clippy::inline_always)]
    #[inline(always)]
    pub(super) fn scalar_at(&self, at: usize) -> Result<Cell> {
        match (self.values.get(at), self.shadow.get(at)) {
            (Some(&cell), Some(&RefTag::NONE)) => Ok(cell),
            (Some(_), Some(_)) => Err(scalar_tag_error()),
            _ => Err(slot_error()),
        }
    }

    /// Writes a scalar result (shadow tag `NONE`) to absolute slot `at`.
    #[allow(clippy::inline_always)]
    #[inline(always)]
    pub(super) fn set_scalar(&mut self, at: usize, cell: Cell) -> Result<()> {
        match (self.values.get_mut(at), self.shadow.get_mut(at)) {
            (Some(slot), Some(tag)) => {
                *slot = cell;
                *tag = RefTag::NONE;
                Ok(())
            }
            _ => Err(slot_error()),
        }
    }

    /// Enters a stack-window op whose operands end at absolute index `top`: its handler then
    /// pops/pushes relative to `sp`. Also the frame's GC safepoint: everything up to `top` is live.
    #[inline]
    pub(super) fn enter_window(&mut self, top: usize) -> Result<()> {
        if top > self.values.len() || top < self.floor {
            return Err(slot_error());
        }
        self.sp = top;
        self.set_live_end(top)
    }

    /// Records the top frame's exact live height (a safepoint; see `Frame::live_end`).
    #[inline]
    pub(super) fn set_live_end(&mut self, end: usize) -> Result<()> {
        if let Some(frame) = self.frames.last_mut() {
            frame.live_end = u32::try_from(end).map_err(|_| slot_error())?;
        }
        Ok(())
    }

    /// Widens the top frame's live range to the buffer's end before host code runs at an
    /// arbitrary op (the epoch callback), where its `live_end` may be stale: call results, catch
    /// payloads and hot-op references can sit above it. Over-approximate but sound — every
    /// collection clears the tags of the slots it does not scan (see `operand_roots`), so a
    /// widened range only ever holds references to allocated objects.
    pub(super) fn widen_top_live_range(&mut self) -> Result<()> {
        self.set_live_end(self.values.len())
    }

    /// Reads the value at slot `at` as `ty`, refusing a tag that doesn't match the type.
    #[inline]
    pub(super) fn take_typed(&self, at: usize, ty: &ValType) -> Result<Val> {
        let (cell, tag) = self.cell_at(at)?;
        if tag != Self::expected_tag(ty) {
            return Err(
                InternalError::OperandStack("operand tag does not match value type").into(),
            );
        }
        Ok(decode_val(cell, ty))
    }

    /// `select` over slots `at` (val1), `at + 1` (val2), `at + 2` (i32 cond); result to `at`.
    #[inline]
    pub(super) fn select_at(&mut self, at: usize) -> Result<()> {
        let (a, a_tag) = self.cell_at(at)?;
        let (b, b_tag) = self.cell_at(at + 1)?;
        let cond = self.scalar_at(at + 2)?;
        if a_tag != b_tag {
            return Err(InternalError::OperandStack("select operand tag mismatch").into());
        }
        let (cell, tag) = if cond.unwrap_i32() != 0 {
            (a, a_tag)
        } else {
            (b, b_tag)
        };
        self.set_cell(at, cell, tag)
    }

    /// An index/length/address operand at slot `at`, widened to `u64`. `is_64` (from the target
    /// memory/table's type) selects the width — there is no runtime tag to read (#42).
    #[inline]
    pub(super) fn index_at(&self, at: usize, is_64: bool) -> Result<u64> {
        let cell = self.scalar_at(at)?;
        Ok(if is_64 {
            cell.unwrap_i64() as u64
        } else {
            u64::from(cell.unwrap_i32() as u32)
        })
    }

    /// Writes a size/grow result to slot `at` (the new top): i64 for a 64-bit memory/table, else
    /// i32 (#42).
    #[inline]
    pub(super) fn put_index(&mut self, at: usize, is_64: bool, v: u64) -> Result<()> {
        let cell = if is_64 {
            Cell::from_i64(v as i64)
        } else {
            Cell::from_i32(v as u32 as i32)
        };
        self.set_scalar(at, cell)
    }

    /// Consumes the i32 condition/index at slot `at` (the stack top).
    #[inline]
    pub(super) fn cond_at(&self, at: usize) -> Result<i32> {
        let v = self.scalar_at(at)?.unwrap_i32();
        Ok(v)
    }

    /// Consumes the two i32 operands at slots `at`, `at + 1` (a fused compare-and-branch).
    #[inline]
    pub(super) fn i32_pair_at(&self, at: usize) -> Result<(i32, i32)> {
        let pair = (
            self.scalar_at(at)?.unwrap_i32(),
            self.scalar_at(at + 1)?.unwrap_i32(),
        );
        Ok(pair)
    }

    /// The reference at slot `at` with its hierarchy tag, refusing a scalar-tagged slot.
    #[inline]
    pub(super) fn ref_at(&self, at: usize) -> Result<(Cell, RefTag)> {
        let (cell, tag) = self.cell_at(at)?;
        if tag.refkind().is_none() {
            return Err(
                InternalError::OperandStack("reference operation found a scalar tag").into(),
            );
        }
        Ok((cell, tag))
    }

    /// Consumes `tys.len()` values from slot `at` (a throw's args), each decoded against its type
    /// after checking its shadow tag.
    pub(super) fn take_params(&self, at: usize, tys: &[ValType]) -> Result<Vec<Val>> {
        let end = at.checked_add(tys.len()).ok_or_else(slot_error)?;
        let mut out = Vec::with_capacity(tys.len());
        for (i, ty) in (at..end).zip(tys) {
            let (cell, tag) = self.cell_at(i)?;
            if tag != Self::expected_tag(ty) {
                return Err(InternalError::OperandStack("parameter tag mismatch").into());
            }
            out.push(decode(cell, ty));
        }
        Ok(out)
    }

    /// The reference of a statically-known hierarchy at slot `at` (null → the typed null).
    pub(super) fn take_ref(&self, at: usize, kind: RefKind) -> Result<Val> {
        let (cell, tag) = self.cell_at(at)?;
        if tag != RefTag::of_refkind(kind) {
            return Err(
                InternalError::OperandStack("reference tag does not match operand type").into(),
            );
        }
        Ok(read_slot(Slot::Ref { offset: 0, kind }, cell.bytes()))
    }

    /// Overwrites slot `at` with a `Val` (encoded against its own hierarchy tag).
    #[inline]
    pub(super) fn set_val(&mut self, at: usize, v: Val) -> Result<()> {
        self.set_cell(at, encode(v), RefTag::of_val(&v))
    }

    /// Binary scalar op over slots `at`, `at + 1`; the result lands in `at`.
    #[inline]
    pub(super) fn binop_at(&mut self, at: usize, f: impl FnOnce(Cell, Cell) -> Cell) -> Result<()> {
        let (a, b) = (self.scalar_at(at)?, self.scalar_at(at + 1)?);
        self.set_scalar(at, f(a, b))
    }

    /// Fallible [`binop_at`](Self::binop_at) (div/rem traps leave the slots untouched).
    #[inline]
    pub(super) fn binop_try_at(
        &mut self,
        at: usize,
        f: impl FnOnce(Cell, Cell) -> Result<Cell>,
    ) -> Result<()> {
        let r = f(self.scalar_at(at)?, self.scalar_at(at + 1)?)?;
        self.set_scalar(at, r)
    }

    /// Unary scalar op on slot `at`, in place.
    #[inline]
    pub(super) fn unop_at(&mut self, at: usize, f: impl FnOnce(Cell) -> Cell) -> Result<()> {
        let a = self.scalar_at(at)?;
        self.set_scalar(at, f(a))
    }

    /// Fallible [`unop_at`](Self::unop_at) (trapping float-to-int truncations).
    #[inline]
    pub(super) fn unop_try_at(
        &mut self,
        at: usize,
        f: impl FnOnce(Cell) -> Result<Cell>,
    ) -> Result<()> {
        let r = f(self.scalar_at(at)?)?;
        self.set_scalar(at, r)
    }

    #[inline]
    pub(super) fn pop(&mut self) -> Result<Cell> {
        let base = self.ensure_operands(1)?;
        let cell = self.values[base];
        self.sp = base;
        Ok(cell)
    }

    #[inline]
    pub(super) fn push(&mut self, v: Val) {
        self.push_slot(encode(v), RefTag::of_val(&v));
    }

    /// Pushes an already-encoded cell with its known shadow tag (type-agnostic moves:
    /// `local.get`, `select`, `br_on_null`).
    #[inline]
    pub(super) fn push_cell(&mut self, cell: Cell, tag: RefTag) {
        self.push_slot(cell, tag);
    }

    /// The cell + shadow tag at operand index `i` (a `local.get` source).
    #[inline]
    pub(super) fn cell_at(&self, i: usize) -> Result<(Cell, RefTag)> {
        let cell = self
            .values
            .get(i)
            .copied()
            .ok_or(InternalError::OperandStack("local index out of bounds"))?;
        Ok((cell, self.tag_at(i)?))
    }

    /// Writes a cell + shadow tag at operand index `i` (a `local.set`/`local.tee` target).
    #[inline]
    pub(super) fn set_cell(&mut self, i: usize, cell: Cell, tag: RefTag) -> Result<()> {
        match (self.values.get_mut(i), self.shadow.get_mut(i)) {
            (Some(slot), Some(slot_tag)) => {
                *slot = cell;
                *slot_tag = tag;
                Ok(())
            }
            _ => Err(InternalError::OperandStack("local index out of bounds").into()),
        }
    }

    /// The top operand as an `i32` without popping (an `array.new*` count peek).
    #[inline]
    pub(super) fn top_i32(&self) -> Result<i32> {
        let base = self.ensure_scalar_operands(1)?;
        Ok(self.values[base].unwrap_i32())
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

    /// Pops a reference operand of a statically-known hierarchy (null â the typed null `Val`).
    #[inline]
    pub(super) fn pop_ref(&mut self, kind: RefKind) -> Result<Val> {
        let base = self.ensure_operands(1)?;
        if self.tag_at(base)? != RefTag::of_refkind(kind) {
            return Err(
                InternalError::OperandStack("reference tag does not match operand type").into(),
            );
        }
        let cell = self.pop()?;
        Ok(read_slot(Slot::Ref { offset: 0, kind }, cell.bytes()))
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
        if self.tag_at(base)? != expected {
            return Err(
                InternalError::OperandStack("operand tag does not match field type").into(),
            );
        }
        let cell = self.pop()?;
        Ok(read_slot(stack_slot, cell.bytes()))
    }

    /// Alloc-free [`pop_params`](Self::pop_params): decodes the top `tys.len()` operands into
    /// `out` (the reused host-call scratch buffer) and pops them.
    #[inline]
    pub(super) fn pop_params_into(&mut self, tys: &[ValType], out: &mut Vec<Val>) -> Result<()> {
        let base = self.ensure_operands(tys.len())?;
        for (tag, ty) in self.tags_from(base)?.iter().zip(tys) {
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
        self.sp = base;
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

    /// Borrowing [`push_results`](Self::push_results) (the reused scratch buffer survives).
    /// Indexed loops for the same reason as [`pop_params_into`](Self::pop_params_into).
    #[inline]
    pub(super) fn push_results_slice(&mut self, results: &[Val]) {
        for v in results {
            self.push_slot(encode_val(*v), RefTag::of_val(v));
        }
    }

    /// The live operand/local roots for a collection: each `(handle, RefKind)` for a non-null
    /// reference slot in a frame's live range (`[locals_base, live_end)`), recovered from the root
    /// shadow (#27g). Every slot outside those ranges is dead and has its tag cleared: the
    /// collection may free its referent, and no later scan — notably a widened one
    /// (`widen_top_live_range`) — may trace a freed handle.
    pub(crate) fn operand_roots(&mut self) -> Result<Vec<(u32, RefKind)>> {
        if self.values.len() != self.shadow.len() {
            return Err(InternalError::OperandStack(
                "value/shadow length mismatch during root scan",
            )
            .into());
        }
        let mut roots = Vec::new();
        let (mut prev_base, mut dead_from) = (0, 0);
        for frame in &self.frames {
            let (base, end) = (frame.locals_base as usize, frame.live_end as usize);
            // Frames stack upward; out of order, a dead gap could overlap a later live range.
            if base < prev_base {
                return Err(
                    InternalError::FrameStack("frames out of order during root scan").into(),
                );
            }
            prev_base = base;
            let (Some(cells), Some(tags)) =
                (self.values.get(base..end), self.shadow.get(base..end))
            else {
                return Err(InternalError::OperandStack("frame live range out of bounds").into());
            };
            roots.extend(cells.iter().zip(tags).filter_map(|(cell, tag)| {
                let kind = tag.refkind()?;
                (!cell.is_null()).then(|| (cell.handle(), kind))
            }));
            clear_tags(&mut self.shadow, dead_from..base);
            dead_from = dead_from.max(end);
        }
        let len = self.shadow.len();
        clear_tags(&mut self.shadow, dead_from..len);
        Ok(roots)
    }
}
