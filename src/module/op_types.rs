//! Payload types of the [`Op`](super::Op) enum: index aliases, immediates, and branch edges.

pub(crate) type TypeIdx = u32;
pub(crate) type FuncIdx = u32;
pub(crate) type TableIdx = u32;
pub(crate) type GlobalIdx = u32;
pub(crate) type LocalIdx = u32;
pub(crate) type DataIdx = u32;
pub(crate) type ElemIdx = u32;
/// A frame-relative operand slot: locals occupy `[0, n_locals)`, operand-stack position `k` lives
/// at `n_locals + k` (its height is static per op, so no runtime stack pointer is needed). An op's
/// operands sit contiguously from its slot, and its result is written back there.
pub(crate) type Slot = u32;

/// A load's operands (memory 0): `dst` ← the bytes at the address in slot `addr`, plus `offset`.
/// A pooled memarg (another memory, or an offset of at least [`BIG_MEMARG`]) sets `offset` to
/// [`BIG_MEMARG`] and puts its pool index in `addr`; the address then sits in place at `dst`.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct LoadArg {
    pub dst: Slot,
    pub addr: Slot,
    pub offset: u32,
}

/// A store's operands (memory 0): the value in slot `val` → the address in slot `addr`, plus
/// `offset`. Pooled like [`LoadArg`]: `offset` is [`BIG_MEMARG`], the pool index is in `val`, and
/// the value sits at `addr + 1`.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoreArg {
    pub addr: Slot,
    pub val: Slot,
    pub offset: u32,
}

/// The `u32::MAX`-offset sentinel of a [`LoadArg`]/[`StoreArg`] whose memarg lives out of line
/// (`align` is validation-only, so a memarg is just its memory index and offset).
pub(crate) const BIG_MEMARG: u32 = u32::MAX;

/// A pooled memory immediate (see [`LoadArg`]): the real memory index + full 64-bit offset.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BigMemArg {
    pub memory: u32,
    pub offset: u64,
}

/// A resolved branch edge: on a taken branch the top `keep` operands are moved down over
/// `pop` discarded ones, then execution jumps to `ip`. `keep`/`pop` are `u16` to keep every
/// branch payload within the 16-byte `Op`: `keep` is a label arity (spec-capped at 1000);
/// `pop` is an operand-stack delta, and compilation rejects a function whose operand stack
/// outgrows `u16` (a resource bound, like the stack limit — see `control::branch`).
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BranchTarget {
    pub ip: u32,
    pub keep: u16,
    pub pop: u16,
}

/// A `br_table`'s target list, stored out-of-line in [`CompiledFunc::br_tables`]: the `len` case
/// targets occupy `[base .. base + len]` and the default sits at `base + len`. Keeping the targets
/// out of the `Op` enum is what makes `Op` non-drop (the `Box` here was its only drop field) — so a
/// `Vec<Op>` frees in one shot. (`br_on_cast` edges share this pool via a packed index.)
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BrTableRange {
    pub base: u32,
    pub len: u32,
}

/// Bit 31 of a pooled `br_on_cast` edge index (`Op::BrOnCast`/`BrOnCastFail`): the cast's
/// `nullable` flag; the low 31 bits index `CompiledFunc::br_tables`.
pub(crate) const NULLABLE_BIT: u32 = 1 << 31;

/// The i32 comparison of a fused [`Op::BrIfCmp`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[rustfmt::skip]
pub(crate) enum CmpKind { Eq, Ne, LtS, LtU, GtS, GtU, LeS, LeU, GeS, GeU }
