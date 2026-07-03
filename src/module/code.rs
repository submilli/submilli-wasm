//! `CompiledFunc` — a function lowered to the internal [`Op`](super::op::Op) bytecode, plus the
//! per-function side-tables (locals, handlers, `br_table` targets, debug offsets) the executor and
//! backtrace paths read. Re-exported from [`super::op`] so existing `module::op::CompiledFunc`
//! paths stay valid.

use crate::canon::IrVal;
use crate::module::op::{BigMemArg, BranchTarget, Op, TypeIdx};

/// A function compiled to internal bytecode.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompiledFunc {
    /// Lowered instruction stream — a pre-sized `Vec` moved straight in (each `Op` written once).
    pub ops: Vec<Op>,
    /// Index into the module's type section (gives param/result types).
    pub type_idx: TypeIdx,
    /// Number of parameters (cached from the function type).
    pub n_params: u32,
    /// Number of results (cached from the function type; used on return).
    pub n_results: u32,
    /// Declared local types (params excluded); default-zero the frame's locals at call time.
    pub local_types: Box<[IrVal]>,
    /// Peak operand-stack depth above the locals (for stack pre-reservation).
    pub max_operands: u32,
    /// Exception-handler table (one entry per `try_table`; #28d). Consulted only on throw.
    pub handlers: Box<[crate::module::handler::HandlerSpan]>,
    /// Flattened `br_table` target lists (see [`BrTableRange`](super::op::BrTableRange)); indexed by
    /// `Op::BrTable`. Empty when the function has no `br_table`.
    pub br_tables: Box<[BranchTarget]>,
    /// Pooled wide memory immediates (64-bit offsets, or the literal offset `u32::MAX`) that
    /// don't fit `MemArg`'s inline `u32`; indexed via the [`BIG_MEMARG`](super::op::BIG_MEMARG)
    /// sentinel. Empty for virtually every real module.
    pub big_memargs: Box<[BigMemArg]>,
    /// Per-`Op` source wasm byte offset for backtraces (#29a); `None` without debug retention.
    pub offsets: Option<Box<[u32]>>,
}
