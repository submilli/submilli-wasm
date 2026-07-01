//! Single-pass decoder: wasm operators -> internal `Op` stream.
//!
//! `straight_line` (below) dispatches each non-control operator by category: core ops inline, then
//! [`numeric`]/[`memory`]/[`table`]. Structured control flow + the folded sidetable live in
//! [`control`]. The body is wrapped in an implicit `Block` frame so `return`/outermost branches
//! lower to a branch with `ip == ops.len()` (the executor returns when `ip >= ops.len()`).

mod control;
mod conv;
mod core;
mod gc;
mod memory;
mod numeric;
mod ref_;
mod table;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
mod visit;
#[cfg(feature = "simd")]
mod visit_simd;

use wasmparser::{BinaryReaderError, FuncValidator, FunctionBody, ValidatorResources};

use crate::canon::{AggKind, IrVal, ModuleType};
use crate::module::handler::HandlerSpan;
use crate::module::op::{CompiledFunc, Op};
use crate::{Error, Result};

use self::control::CtrlFrame;
pub(crate) use self::conv::{
    conv_globaltype, conv_heaptype, conv_memtype, conv_reftype_heap, conv_tabletype, conv_valtype,
};
use self::visit::ValidateThenLower;

/// Per-module context the translator needs: the type table (for func signatures), the per-type
/// kind table (for concrete references), and the function/tag index → type-index maps.
pub(crate) struct CompileCtx<'a> {
    pub types: &'a [ModuleType],
    pub kinds: &'a [AggKind],
    pub func_types: &'a [u32],
    /// Tag-index → type-index (imported then defined), for `try_table` catch-payload arity.
    pub tag_types: &'a [u32],
}

fn wp_err(e: BinaryReaderError) -> Error {
    Error::msg(e.to_string())
}

/// Validates and translates a single function body into a [`CompiledFunc`] in one pass: each
/// operator is validated (via `fv`) and lowered to internal bytecode as it is decoded, so the
/// body's bytes are read exactly once. Module-level validation has already run in `parse_module`.
pub(crate) fn translate_function(
    ctx: &CompileCtx<'_>,
    type_idx: u32,
    body: &FunctionBody<'_>,
    fv: &mut FuncValidator<ValidatorResources>,
    retain_offsets: bool,
) -> Result<CompiledFunc> {
    let (params, results) = ctx.types[type_idx as usize].func_sig();
    let n_params = params.len() as u32;
    let n_results = results.len() as u32;

    // Validate the locals declarations (count/type limits) with accurate byte offsets, then
    // re-read them into our own `IrVal` types — the locals header is tiny, so the second read
    // is negligible (the operator body, which dominates, is still walked exactly once below).
    fv.read_locals(&mut body.get_binary_reader())
        .map_err(wp_err)?;
    let mut local_types: Vec<IrVal> = Vec::new();
    for entry in body.get_locals_reader().map_err(wp_err)? {
        let (count, ty) = entry.map_err(wp_err)?;
        let vt = conv_valtype(ctx.kinds, ty)?;
        for _ in 0..count {
            local_types.push(vt.clone());
        }
    }

    // Pre-size the op buffer from the body byte length so each `Op` is written exactly once (no
    // regrowth copies). Op count is always < body bytes (every op is ≥1 byte), so this never
    // under-reserves for real code; a modest over-reserve is freed when the `Vec` moves into the
    // `CompiledFunc`.
    let op_hint = body.range().len();
    let mut t = Translator::with_capacity(ctx, retain_offsets, op_hint);
    t.push_func_frame(n_results);
    let mut reader = body.get_operators_reader().map_err(wp_err)?;
    let mut vl = ValidateThenLower {
        validator: fv,
        translator: &mut t,
        offset: 0,
    };
    while !reader.eof() {
        vl.offset = reader.original_position();
        // Outer `?`: decode error. Inner `?`: validation/lowering error.
        reader.visit_operator(&mut vl).map_err(wp_err)??;
    }
    reader.finish().map_err(wp_err)?;

    Ok(CompiledFunc {
        ops: t.ops,
        type_idx,
        n_params,
        n_results,
        local_types: local_types.into_boxed_slice(),
        max_operands: t.max_operands,
        handlers: t.handlers.into_boxed_slice(),
        offsets: t.offsets.map(Vec::into_boxed_slice),
    })
}

struct Translator<'a> {
    ctx: &'a CompileCtx<'a>,
    ops: Vec<Op>,
    height: u32,
    max_operands: u32,
    ctrl: Vec<CtrlFrame>,
    reachable: bool,
    handlers: Vec<HandlerSpan>,
    /// Byte offset of the operator currently being translated; recorded per emitted `Op` into
    /// `offsets` (when present) so a frame's `ip` can be mapped back to source via DWARF (#29a).
    cur_offset: u32,
    /// Parallel to `ops`: one source offset per `Op`. `None` when debug retention is off.
    offsets: Option<Vec<u32>>,
}

impl<'a> Translator<'a> {
    fn with_capacity(ctx: &'a CompileCtx<'a>, retain_offsets: bool, op_hint: usize) -> Self {
        Translator {
            ctx,
            ops: Vec::with_capacity(op_hint),
            height: 0,
            max_operands: 0,
            ctrl: Vec::new(),
            reachable: true,
            handlers: Vec::new(),
            cur_offset: 0,
            offsets: retain_offsets.then(|| Vec::with_capacity(op_hint)),
        }
    }

    fn emit(&mut self, op: Op) {
        if let Some(offsets) = &mut self.offsets {
            offsets.push(self.cur_offset);
        }
        self.ops.push(op);
    }

    fn push(&mut self, n: u32) {
        self.height += n;
        self.max_operands = self.max_operands.max(self.height);
    }

    fn pop(&mut self, n: u32) {
        self.height = self.height.saturating_sub(n);
    }

    /// pop 2, push 1 (binary numeric / comparison).
    fn binop(&mut self, op: Op) {
        self.pop(2);
        self.push(1);
        self.emit(op);
    }

    /// pop 1, push 1 (unary numeric / conversion / test / load).
    fn unop(&mut self, op: Op) {
        self.pop(1);
        self.push(1);
        self.emit(op);
    }

    /// pop 2, push 0 (store).
    fn store(&mut self, op: Op) {
        self.pop(2);
        self.emit(op);
    }

    /// push 1 (constant / size / get).
    fn constop(&mut self, op: Op) {
        self.push(1);
        self.emit(op);
    }

    /// pop 3, push 1 (`v128.bitselect`).
    #[cfg(feature = "simd")]
    fn ternary(&mut self, op: Op) {
        self.pop(3);
        self.push(1);
        self.emit(op);
    }
}
