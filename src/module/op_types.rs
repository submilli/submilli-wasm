//! Payload types of the [`Op`](super::Op) enum: index aliases, immediates, and branch edges.

pub(crate) type TypeIdx = u32;
pub(crate) type FuncIdx = u32;
pub(crate) type TableIdx = u32;
pub(crate) type GlobalIdx = u32;
pub(crate) type LocalIdx = u32;
pub(crate) type DataIdx = u32;
pub(crate) type ElemIdx = u32;

/// A memory-access immediate: target memory index (#41) + static offset (`align` is
/// validation-only). The inline offset is `u32`; a `u64` offset (reachable only with a
/// memory64 memarg — or the literal value `u32::MAX` in wasm32, which collides with the
/// sentinel) is demoted to the function's [`BigMemArg`] pool: `offset == u32::MAX` marks
/// the demoted form, and `memory` then holds the pool index instead of the memory index.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct MemArg {
    pub memory: u32,
    pub offset: u32,
}

/// The `u32::MAX`-offset sentinel of a [`MemArg`] whose real immediate lives out of line.
pub(crate) const BIG_MEMARG: u32 = u32::MAX;

/// A pooled wide memory immediate (see [`MemArg`]): the real memory index + full 64-bit offset.
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
