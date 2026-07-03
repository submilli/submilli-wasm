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
use crate::module::code::{CodeArenas, Span};
use crate::module::op::{BigMemArg, BranchTarget, CmpKind, CompiledFunc, MemArg, Op, BIG_MEMARG};
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

/// Reusable per-function translation buffers, recycled across every body in a module compile so a
/// function's `ctrl`/`local_types` reuse the prior function's capacity instead of allocating (and
/// regrowing from empty) each time — mirrors how wasmi recycles its translator allocations.
#[derive(Default)]
pub(crate) struct Scratch {
    ctrl: Vec<CtrlFrame>,
    local_types: Vec<IrVal>,
}

/// Validates and translates a single function body into a [`CompiledFunc`] in one pass: each
/// operator is validated (via `fv`) and lowered to internal bytecode as it is decoded, so the
/// body's bytes are read exactly once. Module-level validation has already run in `parse_module`.
#[allow(clippy::too_many_arguments)] // the compile pipeline's one fan-in point
pub(crate) fn translate_function(
    ctx: &CompileCtx<'_>,
    code: &mut CodeArenas,
    type_idx: u32,
    body: &FunctionBody<'_>,
    fv: &mut FuncValidator<ValidatorResources>,
    retain_offsets: bool,
    scratch: &mut Scratch,
) -> Result<CompiledFunc> {
    let (params, results) = ctx.types[type_idx as usize].func_sig();
    let n_params = params.len() as u32;
    let n_results = results.len() as u32;

    // Validate the locals declarations (count/type limits) with accurate byte offsets, then
    // re-read them into our own `IrVal` types — the locals header is tiny, so the second read
    // is negligible (the operator body, which dominates, is still walked exactly once below).
    fv.read_locals(&mut body.get_binary_reader())
        .map_err(wp_err)?;
    let locals_start = code.local_types.len() as u32;
    for entry in body.get_locals_reader().map_err(wp_err)? {
        let (count, ty) = entry.map_err(wp_err)?;
        let vt = conv_valtype(ctx.kinds, ty)?;
        for _ in 0..count {
            code.local_types.push(vt.clone());
        }
    }

    // The function's streams append to the module-wide arenas (pre-reserved once in
    // `compile_bodies` — each `Op` is written exactly once, no regrowth, no per-function
    // allocation). `ctrl` is handed the recycled buffer (empty, capacity preserved).
    let mut t = Translator::new(ctx, code, retain_offsets, std::mem::take(&mut scratch.ctrl));
    t.push_func_frame(n_results);
    // Drive `BinaryReader::visit_operator` directly (the same pattern `FuncValidator::validate`
    // uses): `ValidateThenLower` doubles as the reader's `FrameStack`, so no `OperatorsReader` —
    // and none of its duplicate per-op control-stack bookkeeping — sits between decode and us.
    let mut reader = body.get_binary_reader_for_operators().map_err(wp_err)?;
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
    reader.finish_expression(&vl).map_err(wp_err)?;

    // Return the (now-empty) `ctrl` buffer to the scratch so the next body reuses its capacity.
    scratch.ctrl = std::mem::take(&mut t.ctrl);
    let max_operands = t.max_operands;
    let base = t.base;
    let span = |start: u32, end: usize| Span {
        start,
        len: end as u32 - start,
    };
    Ok(CompiledFunc {
        ops: span(base.ops, code.ops.len()),
        type_idx,
        n_params,
        n_results,
        local_types: span(locals_start, code.local_types.len()),
        max_operands,
        handlers: span(base.handlers, code.handlers.len()),
        br_tables: span(base.br_tables, code.br_tables.len()),
        big_memargs: span(base.big_memargs, code.big_memargs.len()),
        offsets: span(base.offsets, code.offsets.len()),
    })
}

/// The arena lengths at function entry — the function's span starts. Every in-function index
/// (branch `ip`s, pool indices) is relative to these.
#[derive(Copy, Clone)]
struct Bases {
    ops: u32,
    br_tables: u32,
    big_memargs: u32,
    handlers: u32,
    offsets: u32,
}

struct Translator<'a> {
    ctx: &'a CompileCtx<'a>,
    /// The module-wide arenas this function appends to (no per-function buffers at all).
    code: &'a mut CodeArenas,
    base: Bases,
    retain_offsets: bool,
    height: u32,
    max_operands: u32,
    ctrl: Vec<CtrlFrame>,
    reachable: bool,
    /// Byte offset of the operator currently being translated; recorded per emitted `Op` into
    /// the offsets arena (when retained) so a frame's `ip` maps back to source via DWARF (#29a).
    cur_offset: u32,
    /// `Some(kind)` while the last emitted op is a fusable i32 relop that an immediately
    /// following `br_if`/`br_if_not` may collapse into an [`Op::BrIfCmp`]. Set by
    /// [`Translator::emit`] per op; cleared at every control boundary where a branch label
    /// could land between the pair (see `control`).
    fusable_cmp: Option<CmpKind>,
}

impl<'a> Translator<'a> {
    fn new(
        ctx: &'a CompileCtx<'a>,
        code: &'a mut CodeArenas,
        retain_offsets: bool,
        ctrl: Vec<CtrlFrame>,
    ) -> Self {
        let base = Bases {
            ops: code.ops.len() as u32,
            br_tables: code.br_tables.len() as u32,
            big_memargs: code.big_memargs.len() as u32,
            handlers: code.handlers.len() as u32,
            offsets: code.offsets.len() as u32,
        };
        Translator {
            ctx,
            code,
            base,
            retain_offsets,
            height: 0,
            max_operands: 0,
            ctrl,
            reachable: true,
            cur_offset: 0,
            fusable_cmp: None,
        }
    }

    /// The function-relative ip the *next* emitted op will get.
    fn next_ip(&self) -> u32 {
        self.code.ops.len() as u32 - self.base.ops
    }

    /// The already-emitted op at function-relative ip `idx` (branch patching / fusion).
    fn op_mut(&mut self, idx: u32) -> &mut Op {
        &mut self.code.ops[(self.base.ops + idx) as usize]
    }

    fn op_ref(&self, idx: u32) -> &Op {
        &self.code.ops[(self.base.ops + idx) as usize]
    }

    /// Appends a branch edge to the shared pool, returning its function-relative index.
    fn push_edge(&mut self, t: BranchTarget) -> u32 {
        let rel = self.code.br_tables.len() as u32 - self.base.br_tables;
        self.code.br_tables.push(t);
        rel
    }

    fn edge_mut(&mut self, rel: u32) -> &mut BranchTarget {
        &mut self.code.br_tables[(self.base.br_tables + rel) as usize]
    }

    /// Converts a wasmparser memarg to the compact form, demoting a wide offset (memory64, or
    /// the literal `u32::MAX`) to the function's pool behind the [`BIG_MEMARG`] sentinel.
    fn memarg(&mut self, m: wasmparser::MemArg) -> MemArg {
        if m.offset < u64::from(BIG_MEMARG) {
            MemArg {
                memory: m.memory,
                offset: m.offset as u32,
            }
        } else {
            let idx = self.code.big_memargs.len() as u32 - self.base.big_memargs;
            self.code.big_memargs.push(BigMemArg {
                memory: m.memory,
                offset: m.offset,
            });
            MemArg {
                memory: idx,
                offset: BIG_MEMARG,
            }
        }
    }

    fn emit(&mut self, op: Op) {
        if self.retain_offsets {
            self.code.offsets.push(self.cur_offset);
        }
        self.fusable_cmp = fusable_cmp_of(&op);
        self.code.ops.push(op);
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

/// The comparison kind if `op` is a fusable i32 relop (the [`Op::BrIfCmp`] candidates).
fn fusable_cmp_of(op: &Op) -> Option<CmpKind> {
    Some(match op {
        Op::I32Eq => CmpKind::Eq,
        Op::I32Ne => CmpKind::Ne,
        Op::I32LtS => CmpKind::LtS,
        Op::I32LtU => CmpKind::LtU,
        Op::I32GtS => CmpKind::GtS,
        Op::I32GtU => CmpKind::GtU,
        Op::I32LeS => CmpKind::LeS,
        Op::I32LeU => CmpKind::LeU,
        Op::I32GeS => CmpKind::GeS,
        Op::I32GeU => CmpKind::GeU,
        _ => return None,
    })
}
