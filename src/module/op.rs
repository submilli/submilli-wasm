//! The internal instruction set (`Op`) and `CompiledFunc` layout.
//!
//! A flat, one-variant-per-opcode enum so the interpreter hot loop is a single
//! `match`. Structured control flow is lowered away by the compile pass — branches
//! carry their resolved target plus the operand-stack fixup inline (the folded
//! sidetable, see ARCHITECTURE §5).

use crate::canon::IrHeap;

pub(crate) use super::op_types::*;

/// One internal instruction. 16 bytes (without `simd`) and **not** a drop type: wide immediates
/// live out of line (`br_table`/`br_on_cast` edges and rare 64-bit memarg offsets in per-function
/// pools), `BranchTarget` packs its stack fixup into two `u16`s, and nothing inline is `Box`ed —
/// a `Vec<Op>` frees in one shot at teardown, and the op stream is 33% smaller resident than the
/// previous 24-byte layout at unchanged decode shape (fixed width, aligned fields).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum Op {
    // --- control ---
    Unreachable,
    Nop,
    Br(BranchTarget),
    BrIf(BranchTarget),
    /// Branch when the popped i32 condition is zero (used to lower `if`).
    BrIfNot(BranchTarget),
    /// Fused i32 compare-and-branch: pops two i32s, branches when `cmp(a, b) != negate` — an
    /// adjacent relop + `br_if`/`br_if_not` pair collapsed at compile time (one dispatch, no
    /// intermediate bool). Fused only when nothing can jump between the pair (see `control`).
    BrIfCmp {
        kind: CmpKind,
        negate: bool,
        target: BranchTarget,
    },
    /// Targets live in [`CompiledFunc::br_tables`]; see [`BrTableRange`].
    BrTable(BrTableRange),
    Call(FuncIdx),
    CallIndirect {
        type_idx: TypeIdx,
        table: TableIdx,
    },
    /// `call_ref`: pop a funcref operand and call it (null traps); type index is validation-only.
    CallRef(TypeIdx),
    /// Tail calls (#39): replace the current frame with the callee, which returns to the caller's caller.
    ReturnCall(FuncIdx),
    ReturnCallIndirect {
        type_idx: TypeIdx,
        table: TableIdx,
    },
    ReturnCallRef(TypeIdx),
    /// Branch when the popped reference is null, dropping it; on fall-through it stays (non-null).
    BrOnNull(BranchTarget),
    /// Branch when the popped reference is non-null, keeping it on the target; on null it is dropped.
    BrOnNonNull(BranchTarget),
    /// `throw $tag`: pop the tag's args, build an exception, and unwind (module-relative tag index).
    Throw(u32),
    /// `throw_ref`: pop an `exnref` and re-throw it (null traps; stack-polymorphic).
    ThrowRef,

    // --- parametric ---
    Drop,
    Select,

    // --- variable ---
    LocalGet(LocalIdx),
    LocalSet(LocalIdx),
    LocalTee(LocalIdx),
    GlobalGet(GlobalIdx),
    GlobalSet(GlobalIdx),

    // --- memory loads ---
    I32Load(MemArg),
    I64Load(MemArg),
    F32Load(MemArg),
    F64Load(MemArg),
    I32Load8S(MemArg),
    I32Load8U(MemArg),
    I32Load16S(MemArg),
    I32Load16U(MemArg),
    I64Load8S(MemArg),
    I64Load8U(MemArg),
    I64Load16S(MemArg),
    I64Load16U(MemArg),
    I64Load32S(MemArg),
    I64Load32U(MemArg),

    // --- memory stores ---
    I32Store(MemArg),
    I64Store(MemArg),
    F32Store(MemArg),
    F64Store(MemArg),
    I32Store8(MemArg),
    I32Store16(MemArg),
    I64Store8(MemArg),
    I64Store16(MemArg),
    I64Store32(MemArg),

    // --- memory management (carry an explicit memory index, #41) ---
    MemorySize(u32),
    MemoryGrow(u32),
    MemoryInit(DataIdx, u32),
    DataDrop(DataIdx),
    /// `memory.copy dst_mem src_mem` — the two indices may differ.
    MemoryCopy(u32, u32),
    MemoryFill(u32),

    // --- table management (bulk-memory subset) ---
    TableInit {
        elem: ElemIdx,
        table: TableIdx,
    },
    TableCopy {
        dst_table: TableIdx,
        src_table: TableIdx,
    },
    ElemDrop(ElemIdx),

    // --- references + table ref-ops (reference-types) ---
    RefNull(IrHeap),
    RefFunc(FuncIdx),
    RefIsNull,
    /// `ref.as_non_null`: trap if the top reference is null, else leave it in place.
    RefAsNonNull,
    TableGet(TableIdx),
    TableSet(TableIdx),
    TableSize(TableIdx),
    TableGrow(TableIdx),
    TableFill(TableIdx),

    // --- constants (floats as raw bits) ---
    I32Const(i32),
    I64Const(i64),
    F32Const(u32),
    F64Const(u64),

    // --- i32 comparisons / numeric ---
    I32Eqz,
    I32Eq,
    I32Ne,
    I32LtS,
    I32LtU,
    I32GtS,
    I32GtU,
    I32LeS,
    I32LeU,
    I32GeS,
    I32GeU,
    I32Clz,
    I32Ctz,
    I32Popcnt,
    I32Add,
    I32Sub,
    I32Mul,
    I32DivS,
    I32DivU,
    I32RemS,
    I32RemU,
    I32And,
    I32Or,
    I32Xor,
    I32Shl,
    I32ShrS,
    I32ShrU,
    I32Rotl,
    I32Rotr,

    // --- i64 comparisons / numeric ---
    I64Eqz,
    I64Eq,
    I64Ne,
    I64LtS,
    I64LtU,
    I64GtS,
    I64GtU,
    I64LeS,
    I64LeU,
    I64GeS,
    I64GeU,
    I64Clz,
    I64Ctz,
    I64Popcnt,
    I64Add,
    I64Sub,
    I64Mul,
    I64DivS,
    I64DivU,
    I64RemS,
    I64RemU,
    I64And,
    I64Or,
    I64Xor,
    I64Shl,
    I64ShrS,
    I64ShrU,
    I64Rotl,
    I64Rotr,

    // --- f32 ---
    F32Eq,
    F32Ne,
    F32Lt,
    F32Gt,
    F32Le,
    F32Ge,
    F32Abs,
    F32Neg,
    F32Ceil,
    F32Floor,
    F32Trunc,
    F32Nearest,
    F32Sqrt,
    F32Add,
    F32Sub,
    F32Mul,
    F32Div,
    F32Min,
    F32Max,
    F32Copysign,

    // --- f64 ---
    F64Eq,
    F64Ne,
    F64Lt,
    F64Gt,
    F64Le,
    F64Ge,
    F64Abs,
    F64Neg,
    F64Ceil,
    F64Floor,
    F64Trunc,
    F64Nearest,
    F64Sqrt,
    F64Add,
    F64Sub,
    F64Mul,
    F64Div,
    F64Min,
    F64Max,
    F64Copysign,
    // --- conversions ---
    I32WrapI64,
    I32TruncF32S,
    I32TruncF32U,
    I32TruncF64S,
    I32TruncF64U,
    I64ExtendI32S,
    I64ExtendI32U,
    I64TruncF32S,
    I64TruncF32U,
    I64TruncF64S,
    I64TruncF64U,
    F32ConvertI32S,
    F32ConvertI32U,
    F32ConvertI64S,
    F32ConvertI64U,
    F32DemoteF64,
    F64ConvertI32S,
    F64ConvertI32U,
    F64ConvertI64S,
    F64ConvertI64U,
    F64PromoteF32,
    I32ReinterpretF32,
    I64ReinterpretF64,
    F32ReinterpretI32,
    F64ReinterpretI64,
    // --- sign-extension ops ---
    I32Extend8S,
    I32Extend16S,
    I64Extend8S,
    I64Extend16S,
    I64Extend32S,
    // --- saturating float-to-int ---
    I32TruncSatF32S,
    I32TruncSatF32U,
    I32TruncSatF64S,
    I32TruncSatF64U,
    I64TruncSatF32S,
    I64TruncSatF32U,
    I64TruncSatF64S,
    I64TruncSatF64U,

    // --- GC aggregates: structs ---
    StructNew(TypeIdx),
    StructNewDefault(TypeIdx),
    StructGet {
        ty: TypeIdx,
        field: u32,
    },
    StructGetS {
        ty: TypeIdx,
        field: u32,
    },
    StructGetU {
        ty: TypeIdx,
        field: u32,
    },
    StructSet {
        ty: TypeIdx,
        field: u32,
    },

    // --- GC aggregates: arrays ---
    ArrayNew(TypeIdx),
    ArrayNewDefault(TypeIdx),
    ArrayNewFixed {
        ty: TypeIdx,
        n: u32,
    },
    ArrayNewData {
        ty: TypeIdx,
        data: DataIdx,
    },
    ArrayNewElem {
        ty: TypeIdx,
        elem: ElemIdx,
    },
    ArrayGet(TypeIdx),
    ArrayGetS(TypeIdx),
    ArrayGetU(TypeIdx),
    ArraySet(TypeIdx),
    ArrayLen,
    ArrayFill(TypeIdx),
    ArrayCopy {
        dst: TypeIdx,
        src: TypeIdx,
    },
    ArrayInitData {
        ty: TypeIdx,
        data: DataIdx,
    },
    ArrayInitElem {
        ty: TypeIdx,
        elem: ElemIdx,
    },

    // --- GC i31 ---
    RefI31,
    I31GetS,
    I31GetU,

    // --- GC casts / equality ---
    RefTest {
        ty: IrHeap,
        nullable: bool,
    },
    RefCast {
        ty: IrHeap,
        nullable: bool,
    },
    RefEq,
    /// `any.convert_extern` / `extern.convert_any`: externref ↔ anyref.
    AnyConvertExtern,
    ExternConvertAny,
    /// `br_on_cast` / `br_on_cast_fail`: the branch edge lives out of line in
    /// `CompiledFunc::br_tables` (shared with `br_table` cases) — `target`'s bit 31 carries the
    /// cast's `nullable` flag, the low 31 bits index the pool. Keeps the variant inside 16 bytes.
    BrOnCast {
        ty: IrHeap,
        target: u32,
    },
    BrOnCastFail {
        ty: IrHeap,
        target: u32,
    },
    /// Fixed-width SIMD (`v128`, #37); the ~236 vector ops live in [`SimdOp`](super::op_simd::SimdOp).
    #[cfg(feature = "simd")]
    Simd(super::op_simd::SimdOp),
}

/// The compiled-function record lives in [`super::code`]; re-exported here so the long-standing
/// `module::op::CompiledFunc` path keeps resolving.
pub(crate) use super::code::CompiledFunc;

// The two load-bearing layout properties (see the enum doc). 16 bytes holds only without the
// `simd` feature — `SimdOp`'s lane/memarg immediates widen the enum.
#[cfg(not(feature = "simd"))]
const _: () = assert!(std::mem::size_of::<Op>() == 16);
const _: () = assert!(!std::mem::needs_drop::<Op>());
