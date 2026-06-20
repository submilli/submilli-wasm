//! The internal instruction set (`Op`) and `CompiledFunc` layout.
//!
//! A flat, one-variant-per-opcode enum so the interpreter hot loop is a single
//! `match`. Structured control flow is lowered away by the compile pass: there
//! are no `block`/`loop`/`if`/`else`/`end` variants — branches carry their
//! resolved target plus the operand-stack fixup inline (the folded sidetable,
//! see ARCHITECTURE §5). Core wasm only; ref/table/GC/EH ops arrive in their phases.

use crate::canon::{IrHeap, IrVal};

pub(crate) type TypeIdx = u32;
pub(crate) type FuncIdx = u32;
pub(crate) type TableIdx = u32;
pub(crate) type GlobalIdx = u32;
pub(crate) type LocalIdx = u32;
pub(crate) type DataIdx = u32;
pub(crate) type ElemIdx = u32;

/// A static memory-access immediate. Single memory; `align` is validation-only
/// (handled by `wasmparser`) and not retained.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct MemArg {
    pub offset: u32,
}

/// A resolved branch edge: the target instruction plus the operand-stack fixup.
/// On a taken branch the top `keep` operands are moved down over `pop` discarded
/// operands, then execution jumps to `ip`.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BranchTarget {
    pub ip: u32,
    pub keep: u32,
    pub pop: u32,
}

/// One internal instruction.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum Op {
    // --- control ---
    Unreachable,
    Nop,
    Br(BranchTarget),
    BrIf(BranchTarget),
    /// Branch when the popped i32 condition is zero (used to lower `if`).
    BrIfNot(BranchTarget),
    BrTable {
        targets: Box<[BranchTarget]>,
        default: BranchTarget,
    },
    Call(FuncIdx),
    CallIndirect {
        type_idx: TypeIdx,
        table: TableIdx,
    },
    /// `call_ref`: pop a funcref operand and call it (null traps). The type index is
    /// validation-only — static typing guarantees the signature, so no runtime check.
    CallRef(TypeIdx),
    /// Branch when the popped reference is null, dropping it (the branch target does
    /// not receive it); on fall-through the (now non-null) reference stays.
    BrOnNull(BranchTarget),
    /// Branch when the popped reference is non-null, keeping it on the branch target;
    /// on fall-through (null) the reference is dropped.
    BrOnNonNull(BranchTarget),

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

    // --- memory management ---
    MemorySize,
    MemoryGrow,
    MemoryInit(DataIdx),
    DataDrop(DataIdx),
    MemoryCopy,
    MemoryFill,

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
    /// `any.convert_extern`: externref → anyref.
    AnyConvertExtern,
    /// `extern.convert_any`: anyref → externref.
    ExternConvertAny,
    BrOnCast {
        ty: IrHeap,
        nullable: bool,
        target: BranchTarget,
    },
    BrOnCastFail {
        ty: IrHeap,
        nullable: bool,
        target: BranchTarget,
    },
}

/// A function compiled to internal bytecode.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompiledFunc {
    /// The lowered instruction stream.
    pub ops: Box<[Op]>,
    /// Index into the module's type section (gives param/result types).
    pub type_idx: TypeIdx,
    /// Number of parameters (cached from the function type).
    pub n_params: u32,
    /// Number of results (cached from the function type; used on return).
    pub n_results: u32,
    /// Declared local types (params excluded); used to default-initialize the
    /// frame's locals to the correctly-typed zero `Val` at call time.
    pub local_types: Box<[IrVal]>,
    /// Peak operand-stack depth above the locals (for stack pre-reservation).
    pub max_operands: u32,
}
