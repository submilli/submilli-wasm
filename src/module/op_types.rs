//! Payload types of the [`Op`](super::Op) enum: index aliases, immediates, and branch edges.

pub(crate) type TypeIdx = u32;
pub(crate) type FuncIdx = u32;
pub(crate) type TableIdx = u32;
pub(crate) type GlobalIdx = u32;
pub(crate) type LocalIdx = u32;
pub(crate) type DataIdx = u32;
pub(crate) type ElemIdx = u32;

/// A memory-access immediate: target memory index (#41) + static offset (`align` is validation-only).
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct MemArg {
    pub memory: u32,
    pub offset: u64,
}

/// A resolved branch edge: on a taken branch the top `keep` operands are moved down over
/// `pop` discarded ones, then execution jumps to `ip`.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BranchTarget {
    pub ip: u32,
    pub keep: u32,
    pub pop: u32,
}

/// A `br_table`'s target list, stored out-of-line in [`CompiledFunc::br_tables`]: the `len` case
/// targets occupy `[base .. base + len]` and the default sits at `base + len`. Keeping the targets
/// out of the `Op` enum is what makes `Op` non-drop (the `Box` here was its only drop field) — so a
/// `Vec<Op>` frees in one shot. (Size is separately pinned at 24 bytes by the `MemArg` loads/stores
/// and the `BrOnCast`/`BrOnCastFail` variants, not by this.)
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BrTableRange {
    pub base: u32,
    pub len: u32,
}

/// The i32 comparison of a fused [`Op::BrIfCmp`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[rustfmt::skip]
pub(crate) enum CmpKind { Eq, Ne, LtS, LtU, GtS, GtU, LeS, LeU, GeS, GeU }
