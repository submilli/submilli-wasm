//! Compiled-code storage: one set of module-wide arenas ([`CodeArenas`]) holding every
//! function's op stream and side tables contiguously, a `Copy` per-function record of
//! [`Span`]s into them ([`CompiledFunc`]), and the refcounted runtime handle ([`Code`])
//! the interpreter walks. One allocation per stream for the whole module — no
//! per-function `Box`es, no `Vec` over-reservation slack, and instances share it all
//! through the module's `Arc`.

use std::sync::Arc;

use crate::canon::IrVal;
use crate::module::handler::HandlerSpan;
use crate::module::inner::ModuleInner;
use crate::module::op::{BigMemArg, BranchTarget, Op, TypeIdx};

/// A range into one of the module's code arenas.
#[derive(Copy, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Span {
    pub start: u32,
    pub len: u32,
}

impl Span {
    #[inline]
    pub(crate) fn range(self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

/// Module-wide arenas for compiled code: every defined function's streams, back to back,
/// addressed by the [`Span`]s in its [`CompiledFunc`]. Indices *within* a function
/// (branch `ip`s, `br_table`/`br_on_cast` pool indices, `BigMemArg` pool indices) stay
/// function-relative — the executor always works on the function's sub-slice.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CodeArenas {
    pub ops: Vec<Op>,
    pub local_types: Vec<IrVal>,
    pub handlers: Vec<HandlerSpan>,
    pub br_tables: Vec<BranchTarget>,
    pub big_memargs: Vec<BigMemArg>,
    /// Per-`Op` source wasm byte offsets (#29a); empty unless debug retention is on.
    pub offsets: Vec<u32>,
}

/// A function compiled to internal bytecode: scalar facts plus [`Span`]s into the
/// module's [`CodeArenas`]. Plain `Copy` data — the whole `functions` table is one
/// allocation.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompiledFunc {
    pub ops: Span,
    /// Index into the module's type section (gives param/result types).
    pub type_idx: TypeIdx,
    pub n_params: u32,
    pub n_results: u32,
    /// Declared local types (params excluded); default-zero the frame's locals at call time.
    pub local_types: Span,
    /// Peak operand-stack depth above the locals (for stack pre-reservation).
    pub max_operands: u32,
    /// Exception-handler table (one entry per `try_table`; #28d). Consulted only on throw.
    pub handlers: Span,
    /// Flattened `br_table` target lists + pooled `br_on_cast` edges (function-relative).
    pub br_tables: Span,
    /// Pooled wide memory immediates (see `BIG_MEMARG`); empty for virtually every module.
    pub big_memargs: Span,
    /// Per-`Op` source offsets (#29a); an empty span when debug retention is off.
    pub offsets: Span,
}

/// Runtime handle to one compiled function: the owning module + defined-function index.
/// `Clone` is an `Arc` bump (what each call frame holds); every accessor resolves through
/// the module's arenas.
#[derive(Debug, Clone)]
pub(crate) struct Code {
    pub module: Arc<ModuleInner>,
    /// *Defined*-function index (module func index minus imports).
    pub index: u32,
}

// Slice/field access is by validated construction: spans are produced by the compiler
// against the same arenas they index (#33 carve-out).
#[allow(clippy::indexing_slicing)]
impl Code {
    #[inline]
    fn f(&self) -> &CompiledFunc {
        &self.module.functions[self.index as usize]
    }

    /// A copy of the function record (`CompiledFunc` is `Copy`): frames cache it so hot
    /// per-frame facts don't re-resolve through the `functions` table on every call/return.
    #[inline]
    pub(crate) fn func(&self) -> CompiledFunc {
        *self.f()
    }

    /// The op stream of an already-copied record (the frame-cached fast path).
    #[inline]
    pub(crate) fn ops_of(&self, f: &CompiledFunc) -> &[Op] {
        &self.module.code.ops[f.ops.range()]
    }

    /// The declared locals of an already-copied record (the `push_call` fast path).
    #[inline]
    pub(crate) fn local_types_of(&self, f: &CompiledFunc) -> &[IrVal] {
        &self.module.code.local_types[f.local_types.range()]
    }

    #[inline]
    pub(crate) fn ops(&self) -> &[Op] {
        &self.module.code.ops[self.f().ops.range()]
    }

    #[inline]
    pub(crate) fn local_types(&self) -> &[IrVal] {
        &self.module.code.local_types[self.f().local_types.range()]
    }

    #[inline]
    pub(crate) fn handlers(&self) -> &[HandlerSpan] {
        &self.module.code.handlers[self.f().handlers.range()]
    }

    #[inline]
    pub(crate) fn br_tables(&self) -> &[BranchTarget] {
        &self.module.code.br_tables[self.f().br_tables.range()]
    }

    #[inline]
    pub(crate) fn big_memargs(&self) -> &[BigMemArg] {
        &self.module.code.big_memargs[self.f().big_memargs.range()]
    }

    /// Per-`Op` source offsets, or `None` when debug retention was off at compile.
    #[inline]
    pub(crate) fn offsets(&self) -> Option<&[u32]> {
        let span = self.f().offsets;
        (span.len > 0).then(|| &self.module.code.offsets[span.range()])
    }

    #[inline]
    pub(crate) fn type_idx(&self) -> TypeIdx {
        self.f().type_idx
    }

    #[inline]
    pub(crate) fn n_params(&self) -> u32 {
        self.f().n_params
    }

    #[inline]
    pub(crate) fn n_results(&self) -> u32 {
        self.f().n_results
    }

    #[inline]
    pub(crate) fn max_operands(&self) -> u32 {
        self.f().max_operands
    }
}
