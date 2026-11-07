//! The internal instruction set (`Op`) and `CompiledFunc` layout.
//!
//! A flat, one-variant-per-opcode enum so the interpreter hot loop is a single
//! `match`. Structured control flow is lowered away by the compile pass — branches
//! carry their resolved target plus the operand-stack fixup inline (the folded
//! sidetable, see ARCHITECTURE §5).

use crate::canon::IrHeap;

pub(crate) use super::op_types::*;

/// One internal instruction. 16 bytes and **not** a drop type: wide immediates
/// live out of line (`br_table`/`br_on_cast` edges and rare 64-bit memarg offsets in per-function
/// pools), `BranchTarget` packs its stack fixup into two `u16`s, and nothing inline is `Box`ed —
/// a `Vec<Op>` frees in one shot at teardown, and the op stream is 33% smaller resident than the
/// previous 24-byte layout at unchanged decode shape (fixed width, aligned fields).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum Op {
    // --- control ---
    Unreachable,
    Nop,
    // Every branch carries the slot of the operand-stack top after its operands are consumed
    // (the condition's slot for a conditional branch): the base of its `keep`/`pop` fixup.
    Br(BranchTarget, Slot),
    BrIf(BranchTarget, Slot),
    /// Branch when the i32 condition is zero (used to lower `if`).
    BrIfNot(BranchTarget, Slot),
    /// Fused i32 compare-and-branch: pops two i32s, branches when `cmp(a, b) != negate` — an
    /// adjacent relop + `br_if`/`br_if_not` pair collapsed at compile time (one dispatch, no
    /// intermediate bool). Fused only when nothing can jump between the pair (see `control`).
    BrIfCmp {
        kind: CmpKind,
        negate: bool,
        target: BranchTarget,
        /// The compared pair's first slot.
        s: Slot,
    },
    /// Targets live in [`CompiledFunc::br_tables`]; see [`BrTableRange`].
    BrTable(BrTableRange, Slot),
    // Calls: a direct call's slot is its first argument (the callee's frame starts there; results
    // land there). Indirect/ref calls carry their table-index / funcref operand's slot instead
    // (read before the callee is known); the arguments sit just below it.
    Call(FuncIdx, Slot),
    CallIndirect {
        type_idx: TypeIdx,
        table: TableIdx,
        s: Slot,
    },
    /// `call_ref`: call the funcref operand (null traps); type index is validation-only.
    CallRef(TypeIdx, Slot),
    /// Tail calls (#39): replace the current frame with the callee, which returns to the caller's caller.
    ReturnCall(FuncIdx, Slot),
    ReturnCallIndirect {
        type_idx: TypeIdx,
        table: TableIdx,
        s: Slot,
    },
    ReturnCallRef(TypeIdx, Slot),
    /// Branch when the popped reference is null, dropping it; on fall-through it stays (non-null).
    BrOnNull(BranchTarget, Slot),
    /// Branch when the popped reference is non-null, keeping it on the target; on null it is dropped.
    BrOnNonNull(BranchTarget, Slot),
    /// `throw $tag`: take the tag's args (from the slot), build an exception, and unwind
    /// (module-relative tag index).
    Throw(u32, Slot),
    /// `throw_ref`: re-throw the `exnref` operand (null traps; stack-polymorphic).
    ThrowRef(Slot),

    // --- parametric ---
    Select(Slot),

    // --- variable ---
    /// `(dst, local)`: copy a local (its slot is its index) to `dst`.
    LocalGet(Slot, LocalIdx),
    /// `(src, local)`: copy `src` into a local — also `local.tee` (the value just stays in its
    /// slot, which a static-height tee keeps live).
    LocalSet(Slot, LocalIdx),
    GlobalGet(Slot, GlobalIdx),
    GlobalSet(Slot, GlobalIdx),

    // --- memory loads ---
    I32Load(MemArg, Slot),
    I64Load(MemArg, Slot),
    F32Load(MemArg, Slot),
    F64Load(MemArg, Slot),
    I32Load8S(MemArg, Slot),
    I32Load8U(MemArg, Slot),
    I32Load16S(MemArg, Slot),
    I32Load16U(MemArg, Slot),
    I64Load8S(MemArg, Slot),
    I64Load8U(MemArg, Slot),
    I64Load16S(MemArg, Slot),
    I64Load16U(MemArg, Slot),
    I64Load32S(MemArg, Slot),
    I64Load32U(MemArg, Slot),

    // --- memory stores ---
    I32Store(MemArg, Slot),
    I64Store(MemArg, Slot),
    F32Store(MemArg, Slot),
    F64Store(MemArg, Slot),
    I32Store8(MemArg, Slot),
    I32Store16(MemArg, Slot),
    I64Store8(MemArg, Slot),
    I64Store16(MemArg, Slot),
    I64Store32(MemArg, Slot),

    // --- memory management (carry an explicit memory index, #41) ---
    /// `(memory, dst)`.
    MemorySize(u32, Slot),
    /// `(memory, delta)`: the old size (or -1) replaces the delta.
    MemoryGrow(u32, Slot),
    /// `(segment, memory, dst)`: operands `dst, src, len` from the slot.
    MemoryInit(DataIdx, u32, Slot),
    DataDrop(DataIdx),
    /// `memory.copy dst_mem src_mem` — the two indices may differ; operands `dst, src, len`.
    MemoryCopy(u32, u32, Slot),
    /// `(memory, dst)`: operands `dst, val, len`.
    MemoryFill(u32, Slot),

    // --- table management (bulk-memory subset) ---
    /// Operands `dst, src, len` from `s`.
    TableInit {
        elem: ElemIdx,
        table: TableIdx,
        s: Slot,
    },
    /// Operands `dst, src, len` from `s`.
    TableCopy {
        dst_table: TableIdx,
        src_table: TableIdx,
        s: Slot,
    },
    ElemDrop(ElemIdx),

    // --- references + table ref-ops (reference-types) ---
    RefNull(IrHeap, Slot),
    RefFunc(FuncIdx, Slot),
    RefIsNull(Slot),
    /// `ref.as_non_null`: trap if the reference is null, else leave it in place.
    RefAsNonNull(Slot),
    /// `(table, s)`: the index at `s` is replaced by the element.
    TableGet(TableIdx, Slot),
    /// `(table, s)`: operands `index, value`.
    TableSet(TableIdx, Slot),
    TableSize(TableIdx, Slot),
    /// `(table, s)`: operands `init, delta`; the old size (or -1) replaces them.
    TableGrow(TableIdx, Slot),
    /// `(table, s)`: operands `dst, value, len`.
    TableFill(TableIdx, Slot),

    // --- constants (floats as raw bits) ---
    I32Const(Slot, i32),
    I64Const(Slot, i64),
    F32Const(Slot, u32),
    F64Const(Slot, u64),

    // --- i32 comparisons / numeric ---
    I32Eqz(Slot),
    I32Eq(Slot),
    I32Ne(Slot),
    I32LtS(Slot),
    I32LtU(Slot),
    I32GtS(Slot),
    I32GtU(Slot),
    I32LeS(Slot),
    I32LeU(Slot),
    I32GeS(Slot),
    I32GeU(Slot),
    I32Clz(Slot),
    I32Ctz(Slot),
    I32Popcnt(Slot),
    I32Add(Slot),
    I32Sub(Slot),
    I32Mul(Slot),
    I32DivS(Slot),
    I32DivU(Slot),
    I32RemS(Slot),
    I32RemU(Slot),
    I32And(Slot),
    I32Or(Slot),
    I32Xor(Slot),
    I32Shl(Slot),
    I32ShrS(Slot),
    I32ShrU(Slot),
    I32Rotl(Slot),
    I32Rotr(Slot),

    // --- i64 comparisons / numeric ---
    I64Eqz(Slot),
    I64Eq(Slot),
    I64Ne(Slot),
    I64LtS(Slot),
    I64LtU(Slot),
    I64GtS(Slot),
    I64GtU(Slot),
    I64LeS(Slot),
    I64LeU(Slot),
    I64GeS(Slot),
    I64GeU(Slot),
    I64Clz(Slot),
    I64Ctz(Slot),
    I64Popcnt(Slot),
    I64Add(Slot),
    I64Sub(Slot),
    I64Mul(Slot),
    I64DivS(Slot),
    I64DivU(Slot),
    I64RemS(Slot),
    I64RemU(Slot),
    I64And(Slot),
    I64Or(Slot),
    I64Xor(Slot),
    I64Shl(Slot),
    I64ShrS(Slot),
    I64ShrU(Slot),
    I64Rotl(Slot),
    I64Rotr(Slot),

    // --- f32 ---
    F32Eq(Slot),
    F32Ne(Slot),
    F32Lt(Slot),
    F32Gt(Slot),
    F32Le(Slot),
    F32Ge(Slot),
    F32Abs(Slot),
    F32Neg(Slot),
    F32Ceil(Slot),
    F32Floor(Slot),
    F32Trunc(Slot),
    F32Nearest(Slot),
    F32Sqrt(Slot),
    F32Add(Slot),
    F32Sub(Slot),
    F32Mul(Slot),
    F32Div(Slot),
    F32Min(Slot),
    F32Max(Slot),
    F32Copysign(Slot),

    // --- f64 ---
    F64Eq(Slot),
    F64Ne(Slot),
    F64Lt(Slot),
    F64Gt(Slot),
    F64Le(Slot),
    F64Ge(Slot),
    F64Abs(Slot),
    F64Neg(Slot),
    F64Ceil(Slot),
    F64Floor(Slot),
    F64Trunc(Slot),
    F64Nearest(Slot),
    F64Sqrt(Slot),
    F64Add(Slot),
    F64Sub(Slot),
    F64Mul(Slot),
    F64Div(Slot),
    F64Min(Slot),
    F64Max(Slot),
    F64Copysign(Slot),
    // --- conversions ---
    I32WrapI64(Slot),
    I32TruncF32S(Slot),
    I32TruncF32U(Slot),
    I32TruncF64S(Slot),
    I32TruncF64U(Slot),
    I64ExtendI32S(Slot),
    I64ExtendI32U(Slot),
    I64TruncF32S(Slot),
    I64TruncF32U(Slot),
    I64TruncF64S(Slot),
    I64TruncF64U(Slot),
    F32ConvertI32S(Slot),
    F32ConvertI32U(Slot),
    F32ConvertI64S(Slot),
    F32ConvertI64U(Slot),
    F32DemoteF64(Slot),
    F64ConvertI32S(Slot),
    F64ConvertI32U(Slot),
    F64ConvertI64S(Slot),
    F64ConvertI64U(Slot),
    F64PromoteF32(Slot),
    I32ReinterpretF32(Slot),
    I64ReinterpretF64(Slot),
    F32ReinterpretI32(Slot),
    F64ReinterpretI64(Slot),
    // --- sign-extension ops ---
    I32Extend8S(Slot),
    I32Extend16S(Slot),
    I64Extend8S(Slot),
    I64Extend16S(Slot),
    I64Extend32S(Slot),
    // --- saturating float-to-int ---
    I32TruncSatF32S(Slot),
    I32TruncSatF32U(Slot),
    I32TruncSatF64S(Slot),
    I32TruncSatF64U(Slot),
    I64TruncSatF32S(Slot),
    I64TruncSatF32U(Slot),
    I64TruncSatF64S(Slot),
    I64TruncSatF64U(Slot),

    // GC ops are *stack-window* ops: each carries `top`, the operand-stack top (one past its last
    // operand) before the op — their handlers pop/push relative to it.
    // --- GC aggregates: structs ---
    StructNew(TypeIdx, Slot),
    StructNewDefault(TypeIdx, Slot),
    StructGet {
        ty: TypeIdx,
        field: u32,
        top: Slot,
    },
    StructGetS {
        ty: TypeIdx,
        field: u32,
        top: Slot,
    },
    StructGetU {
        ty: TypeIdx,
        field: u32,
        top: Slot,
    },
    StructSet {
        ty: TypeIdx,
        field: u32,
        top: Slot,
    },

    // --- GC aggregates: arrays ---
    ArrayNew(TypeIdx, Slot),
    ArrayNewDefault(TypeIdx, Slot),
    ArrayNewFixed {
        ty: TypeIdx,
        n: u32,
        top: Slot,
    },
    ArrayNewData {
        ty: TypeIdx,
        data: DataIdx,
        top: Slot,
    },
    ArrayNewElem {
        ty: TypeIdx,
        elem: ElemIdx,
        top: Slot,
    },
    ArrayGet(TypeIdx, Slot),
    ArrayGetS(TypeIdx, Slot),
    ArrayGetU(TypeIdx, Slot),
    ArraySet(TypeIdx, Slot),
    ArrayLen(Slot),
    ArrayFill(TypeIdx, Slot),
    ArrayCopy {
        dst: TypeIdx,
        src: TypeIdx,
        top: Slot,
    },
    ArrayInitData {
        ty: TypeIdx,
        data: DataIdx,
        top: Slot,
    },
    ArrayInitElem {
        ty: TypeIdx,
        elem: ElemIdx,
        top: Slot,
    },

    // --- GC i31 ---
    RefI31(Slot),
    I31GetS(Slot),
    I31GetU(Slot),

    // --- GC casts / equality ---
    RefTest {
        ty: IrHeap,
        nullable: bool,
        top: Slot,
    },
    RefCast {
        ty: IrHeap,
        nullable: bool,
        top: Slot,
    },
    RefEq(Slot),
    /// `any.convert_extern` / `extern.convert_any`: externref ↔ anyref.
    AnyConvertExtern(Slot),
    ExternConvertAny(Slot),
    /// `br_on_cast` / `br_on_cast_fail`: the branch edge lives out of line in
    /// `CompiledFunc::br_tables` (shared with `br_table` cases) — `target`'s bit 31 carries the
    /// cast's `nullable` flag, the low 31 bits index the pool. Keeps the variant inside 16 bytes.
    /// `heap` indexes the module's `cast_heaps` pool (inline, the `IrHeap` would push the variant
    /// past 16 bytes).
    BrOnCast {
        heap: u32,
        target: u32,
        top: Slot,
    },
    BrOnCastFail {
        heap: u32,
        target: u32,
        top: Slot,
    },
    /// Fixed-width SIMD (`v128`, #37); the ~236 vector ops live in [`SimdOp`](super::op_simd::SimdOp).
    /// A stack-window op like the GC ones: `(op, top)`.
    #[cfg(feature = "simd")]
    Simd(super::op_simd::SimdOp, Slot),
}

impl Op {
    /// A stack-window op's `top` (one past its last operand), or `None` for a slot-addressed op.
    pub(crate) fn window_top(&self) -> Option<Slot> {
        match *self {
            Op::StructNew(_, top)
            | Op::StructNewDefault(_, top)
            | Op::StructGet { top, .. }
            | Op::StructGetS { top, .. }
            | Op::StructGetU { top, .. }
            | Op::StructSet { top, .. }
            | Op::ArrayNew(_, top)
            | Op::ArrayNewDefault(_, top)
            | Op::ArrayNewFixed { top, .. }
            | Op::ArrayNewData { top, .. }
            | Op::ArrayNewElem { top, .. }
            | Op::ArrayGet(_, top)
            | Op::ArrayGetS(_, top)
            | Op::ArrayGetU(_, top)
            | Op::ArraySet(_, top)
            | Op::ArrayLen(top)
            | Op::ArrayFill(_, top)
            | Op::ArrayCopy { top, .. }
            | Op::ArrayInitData { top, .. }
            | Op::ArrayInitElem { top, .. }
            | Op::RefI31(top)
            | Op::I31GetS(top)
            | Op::I31GetU(top)
            | Op::RefTest { top, .. }
            | Op::RefCast { top, .. }
            | Op::RefEq(top)
            | Op::AnyConvertExtern(top)
            | Op::ExternConvertAny(top)
            | Op::BrOnCast { top, .. }
            | Op::BrOnCastFail { top, .. } => Some(top),
            #[cfg(feature = "simd")]
            Op::Simd(_, top) => Some(top),
            _ => None,
        }
    }
}

/// The compiled-function record lives in [`super::code`]; re-exported here so the long-standing
/// `module::op::CompiledFunc` path keeps resolving.
pub(crate) use super::code::CompiledFunc;

// The two load-bearing layout properties (see the enum doc). 16 bytes holds with the `simd`
// feature too: `SimdOp`'s 16-byte immediates live in the `v128s` pool.
const _: () = assert!(std::mem::size_of::<Op>() == 16);
const _: () = assert!(!std::mem::needs_drop::<Op>());
