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
mod imm;
mod memory;
mod numeric;
mod provider;
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
use crate::error::InternalError;
use crate::module::code::{CodeArenas, Span};
use crate::module::op::{
    BigMemArg, BranchTarget, CmpKind, CompiledFunc, LoadArg, Op, Slot, StoreArg, BIG_MEMARG,
};
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
    /// Module-wide cap on run-length-expanded locals (`ModuleLimits::max_expanded_locals`).
    pub max_expanded_locals: usize,
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
    stack: Vec<Slot>,
    local_heights: Vec<u32>,
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
    let locals_start = expand_locals(ctx, code, body)?;
    let n_locals = u32::try_from(code.local_types.len().saturating_sub(locals_start as usize))
        .ok()
        .and_then(|n| n.checked_add(n_params))
        .ok_or(InternalError::Compiler("local count overflow"))?;

    // The function's streams append to the module-wide arenas (pre-reserved once in
    // `compile_bodies` — each `Op` is written exactly once, no regrowth, no per-function
    // allocation). `ctrl` is handed the recycled buffer (empty, capacity preserved).
    let mut t = Translator::new(ctx, code, retain_offsets, scratch);
    t.n_locals = n_locals;
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

    if let Some(error) = t.error {
        return Err(error.into());
    }

    t.recycle(scratch);
    // Results return from the operand base, so the frame always has room for them — even when
    // the body's end is dead code (which pushes nothing).
    let max_operands = t.max_operands.max(n_results);
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

/// Expands the body's run-length local declarations into the module-wide `local_types` arena,
/// returning the function's start index. `wasmparser` caps locals *per function* (50 000) but
/// not per module, so a few megabytes of trivial functions could otherwise request gigabytes
/// here; the aggregate is checked against the configured cap before each group is reserved, and
/// the reservation itself is fallible so a genuine allocation failure errors instead of aborting.
fn expand_locals(
    ctx: &CompileCtx<'_>,
    code: &mut CodeArenas,
    body: &FunctionBody<'_>,
) -> Result<u32> {
    // `Span` indexes are `u32`, so the effective cap can never exceed what a span can address.
    let cap = ctx.max_expanded_locals.min(u32::MAX as usize);
    let start = code.local_types.len();
    for entry in body.get_locals_reader().map_err(wp_err)? {
        let (count, ty) = entry.map_err(wp_err)?;
        let vt = conv_valtype(ctx.kinds, ty)?;
        let count = count as usize;
        let total = code
            .local_types
            .len()
            .checked_add(count)
            .filter(|&n| n <= cap)
            .ok_or_else(|| {
                Error::msg(format!(
                    "module declares more than {cap} locals in total (configured limit)"
                ))
            })?;
        code.local_types
            .try_reserve(count)
            .map_err(|_| Error::msg("out of memory expanding module locals"))?;
        code.local_types.resize(total, vt);
    }
    Ok(start as u32)
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
    /// Params + declared locals: the frame slot where operand-stack position 0 lives.
    n_locals: u32,
    height: u32,
    /// The slot each operand-stack entry's value lives in (`len == height`; see `provider`).
    stack: Vec<Slot>,
    /// The heights of the entries reading a local's slot, ascending (see `provider`).
    local_heights: Vec<u32>,
    max_operands: u32,
    ctrl: Vec<CtrlFrame>,
    reachable: bool,
    /// Byte offset of the operator currently being translated; recorded per emitted `Op` into
    /// the offsets arena (when retained) so a frame's `ip` maps back to source via DWARF (#29a).
    cur_offset: u32,
    /// `Some((kind, lhs, rhs, dst))` while the last emitted op is a fusable i32 comparison that a
    /// following `br_if`/`if` testing its result (`dst`) may collapse into one branch op
    /// ([`fused_branch`]). Set by the helpers emitting comparisons; cleared by every other emit
    /// and at every control boundary where a branch label could land between the pair.
    fusable_cmp: Option<(CmpKind, Slot, CmpRhs, Slot)>,
    /// `Some((dst, k))` while the last emitted op is `i32.const k` into `dst`, which a following
    /// binop may fold into an immediate form (see `imm`). Same lifetime as `fusable_cmp`.
    pending_const: Option<(Slot, i32)>,
    /// `Some((ip, dst))` while the last emitted op computes into a free destination `dst`
    /// ([`Op::dst_mut`]) that a following `local.set`/`local.tee` of that result may retarget
    /// at the local. Same lifetime as `fusable_cmp`.
    fusable_dst: Option<(u32, Slot)>,
    /// First internal height-accounting failure. The validator should make this unreachable, but
    /// latching it keeps the lowering helpers small and prevents a malformed internal stream from
    /// being published as successfully compiled code.
    error: Option<InternalError>,
}

impl<'a> Translator<'a> {
    fn new(
        ctx: &'a CompileCtx<'a>,
        code: &'a mut CodeArenas,
        retain_offsets: bool,
        scratch: &mut Scratch,
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
            n_locals: 0,
            height: 0,
            stack: std::mem::take(&mut scratch.stack),
            local_heights: std::mem::take(&mut scratch.local_heights),
            max_operands: 0,
            ctrl: std::mem::take(&mut scratch.ctrl),
            reachable: true,
            cur_offset: 0,
            fusable_cmp: None,
            fusable_dst: None,
            pending_const: None,
            error: None,
        }
    }

    /// Returns the per-function buffers to the scratch so the next body reuses their capacity.
    fn recycle(&mut self, scratch: &mut Scratch) {
        self.stack.clear();
        self.local_heights.clear();
        scratch.ctrl = std::mem::take(&mut self.ctrl);
        scratch.stack = std::mem::take(&mut self.stack);
        scratch.local_heights = std::mem::take(&mut self.local_heights);
    }

    /// The function-relative ip the *next* emitted op will get.
    fn next_ip(&self) -> u32 {
        self.code.ops.len() as u32 - self.base.ops
    }

    /// The already-emitted op at function-relative ip `idx` (branch patching / fusion).
    fn op_mut(&mut self, idx: u32) -> &mut Op {
        &mut self.code.ops[(self.base.ops + idx) as usize]
    }

    /// [`op_mut`](Self::op_mut), checked (`local.set` fusion's retarget).
    fn op_get_mut(&mut self, idx: u32) -> Option<&mut Op> {
        let at = self.base.ops.checked_add(idx)?;
        self.code.ops.get_mut(at as usize)
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

    /// The inline offset of a memory-0 memarg below [`BIG_MEMARG`]; `None` ⇒ it must be pooled.
    fn inline_offset(m: wasmparser::MemArg) -> Option<u32> {
        u32::try_from(m.offset)
            .ok()
            .filter(|&offset| m.memory == 0 && offset < BIG_MEMARG)
    }

    /// A load: `dst` ← memory at the address operand (see [`LoadArg`] for the pooled form).
    fn load(&mut self, m: wasmparser::MemArg, op: impl FnOnce(LoadArg) -> Op) {
        if let Some(offset) = Self::inline_offset(m) {
            let addr = self.operand(0);
            let dst = self.height_slot(1);
            self.produce(1, dst);
            self.emit_result(op(LoadArg { dst, addr, offset }), dst);
        } else {
            let dst = self.slot_below(1);
            let addr = self.pool_memarg(m);
            self.pop(1);
            self.push(1);
            self.emit(op(LoadArg {
                dst,
                addr,
                offset: BIG_MEMARG,
            }));
        }
    }

    /// A store of the top operand to the address below it (see [`StoreArg`] for the pooled form).
    fn store(&mut self, m: wasmparser::MemArg, op: impl FnOnce(StoreArg) -> Op) {
        let arg = if let Some(offset) = Self::inline_offset(m) {
            let (addr, val) = (self.operand(1), self.operand(0));
            StoreArg { addr, val, offset }
        } else {
            let addr = self.slot_below(2);
            let val = self.pool_memarg(m);
            StoreArg {
                addr,
                val,
                offset: BIG_MEMARG,
            }
        };
        self.pop(2);
        self.emit(op(arg));
    }

    /// Pools a memarg in the function's `big_memargs`, returning its function-relative index.
    fn pool_memarg(&mut self, m: wasmparser::MemArg) -> u32 {
        let idx = u32::try_from(self.code.big_memargs.len())
            .ok()
            .and_then(|len| len.checked_sub(self.base.big_memargs));
        self.code.big_memargs.push(BigMemArg {
            memory: m.memory,
            offset: m.offset,
        });
        idx.unwrap_or_else(|| {
            self.error
                .get_or_insert(InternalError::Compiler("memarg pool overflow"));
            0
        })
    }

    fn emit(&mut self, op: Op) {
        if self.retain_offsets {
            self.code.offsets.push(self.cur_offset);
        }
        self.clear_fusion();
        self.code.ops.push(op);
    }

    /// Emits an op computing into the free destination `dst` ([`Op::dst_mut`]): a following
    /// `local.set` of the result may retarget it.
    fn emit_result(&mut self, op: Op, dst: Slot) {
        let ip = self.next_ip();
        self.emit(op);
        self.fusable_dst = Some((ip, dst));
    }

    /// push 1, computed into a free destination (constants, `global.get`).
    fn fresh(&mut self, op: impl FnOnce(Slot) -> Op) {
        let dst = self.height_slot(0);
        self.produce(0, dst);
        self.emit_result(op(dst), dst);
    }

    /// A control boundary (or a stack change that emits nothing, like `drop`): no pending
    /// fusion may cross it.
    fn clear_fusion(&mut self) {
        self.fusable_cmp = None;
        self.fusable_dst = None;
        self.pending_const = None;
    }

    /// Removes the last emitted op if it is `i32.const k` into `dst` (an immediate form takes its
    /// place, at the same ip), returning its source offset; `None` if it is not that constant.
    fn unemit_const(&mut self, dst: Slot, k: i32) -> Option<u32> {
        let is_const =
            matches!(self.code.ops.last(), Some(&Op::I32Const(d, v)) if d == dst && v == k);
        if !is_const || self.next_ip() == 0 {
            return None;
        }
        self.code.ops.pop();
        let offset = if self.retain_offsets {
            self.code.offsets.pop()
        } else {
            None
        };
        Some(offset.unwrap_or(self.cur_offset))
    }

    fn height_below(&mut self, n: u32) -> u32 {
        if let Some(height) = self.height.checked_sub(n) {
            height
        } else {
            self.error
                .get_or_insert(InternalError::Compiler("control operand height underflow"));
            self.height
        }
    }

    /// The frame slot of the operand `n` entries below the stack top — where an op consuming the
    /// top `n` operands *in their own slots* finds its first one (and writes its result). Those
    /// operands are materialized there first.
    fn slot_below(&mut self, n: u32) -> Slot {
        self.materialize(n);
        self.height_slot(n)
    }

    /// The frame slot of stack height `height - n` — a result's destination — without
    /// materializing anything.
    fn height_slot(&mut self, n: u32) -> Slot {
        let height = self.height_below(n);
        self.slot_of(height)
    }

    /// pop 2, push 1 (binary numeric / comparison): three-address `(dst, lhs, rhs)`.
    fn binop(&mut self, op: impl FnOnce(Slot, Slot, Slot) -> Op) {
        let (lhs, rhs) = (self.operand(1), self.operand(0));
        let dst = self.height_slot(2);
        self.produce(2, dst);
        let op = op(dst, lhs, rhs);
        let cmp = fusable_cmp_of(&op);
        self.emit_result(op, dst);
        self.fusable_cmp = cmp;
    }

    /// pop 1, push 1 (unary numeric / conversion / test): two-address `(dst, src)`.
    fn unop(&mut self, op: impl FnOnce(Slot, Slot) -> Op) {
        let src = self.operand(0);
        let dst = self.height_slot(1);
        self.produce(1, dst);
        let op = op(dst, src);
        let cmp = fusable_cmp_of(&op); // `i32.eqz`
        self.emit_result(op, dst);
        self.fusable_cmp = cmp;
    }

    /// pop `n`, push 1, in place: the result overwrites the first operand's slot (the cold
    /// reference/table/`memory.grow` ops).
    fn in_place(&mut self, n: u32, op: impl FnOnce(Slot) -> Op) {
        let s = self.slot_below(n);
        self.pop(n);
        self.push(1);
        self.emit(op(s));
    }

    /// A *stack-window* op consuming `n_in` operands and pushing `n_out`, addressed by the stack
    /// top before it (`top`, one past its last operand): its handler pops/pushes relative to it.
    fn window(&mut self, n_in: u32, n_out: u32, op: impl FnOnce(Slot) -> Op) {
        self.materialize(n_in);
        let top = self.height_slot(0);
        self.pop(n_in);
        self.push(n_out);
        self.emit(op(top));
    }

    fn window_unop(&mut self, op: impl FnOnce(Slot) -> Op) {
        self.window(1, 1, op);
    }

    fn window_binop(&mut self, op: impl FnOnce(Slot) -> Op) {
        self.window(2, 1, op);
    }

    /// pop `n`, push 0, slot-addressed at the first popped operand (bulk memory/table ops).
    fn consume(&mut self, n: u32, op: impl FnOnce(Slot) -> Op) {
        let s = self.slot_below(n);
        self.pop(n);
        self.emit(op(s));
    }

    /// push 1 (constant / get), slot-addressed: the result lands at the new top.
    fn constop(&mut self, op: impl FnOnce(Slot) -> Op) {
        let s = self.slot_below(0);
        self.push(1);
        self.emit(op(s));
    }

    /// pop 1, push 0 (`global.set`), reading the value from wherever it lives.
    fn sink(&mut self, op: impl FnOnce(Slot) -> Op) {
        let s = self.operand(0);
        self.pop(1);
        self.emit(op(s));
    }

    /// A SIMD op as a stack window (`n_in` operands, `n_out` results; see `Op::Simd`).
    #[cfg(feature = "simd")]
    fn simd(&mut self, n_in: u32, n_out: u32, op: crate::module::op_simd::SimdOp) {
        self.window(n_in, n_out, |top| Op::Simd(op, top));
    }

    #[cfg(feature = "simd")]
    fn simd_unop(&mut self, op: crate::module::op_simd::SimdOp) {
        self.simd(1, 1, op);
    }

    #[cfg(feature = "simd")]
    fn simd_binop(&mut self, op: crate::module::op_simd::SimdOp) {
        self.simd(2, 1, op);
    }

    #[cfg(feature = "simd")]
    fn simd_ternary(&mut self, op: crate::module::op_simd::SimdOp) {
        self.simd(3, 1, op);
    }

    #[cfg(feature = "simd")]
    fn simd_store(&mut self, op: crate::module::op_simd::SimdOp) {
        self.simd(2, 0, op);
    }
}

/// A fusable comparison's right operand.
#[derive(Copy, Clone, Debug)]
pub(super) enum CmpRhs {
    Slot(Slot),
    Imm(i32),
}

/// `(kind, lhs, rhs, dst)` if `op` is an i32 comparison a following branch may fuse with:
/// a relop, its immediate form, or `i32.eqz` (`== 0`).
fn fusable_cmp_of(op: &Op) -> Option<(CmpKind, Slot, CmpRhs, Slot)> {
    if let Op::I32CmpImm {
        kind,
        dst,
        lhs,
        imm,
    } = *op
    {
        return Some((kind, lhs, CmpRhs::Imm(imm), dst));
    }
    if let Op::I32Eqz(dst, src) = *op {
        return Some((CmpKind::Eq, src, CmpRhs::Imm(0), dst));
    }
    let (kind, a, b, d) = match *op {
        Op::I32Eq(d, a, b) => (CmpKind::Eq, a, b, d),
        Op::I32Ne(d, a, b) => (CmpKind::Ne, a, b, d),
        Op::I32LtS(d, a, b) => (CmpKind::LtS, a, b, d),
        Op::I32LtU(d, a, b) => (CmpKind::LtU, a, b, d),
        Op::I32GtS(d, a, b) => (CmpKind::GtS, a, b, d),
        Op::I32GtU(d, a, b) => (CmpKind::GtU, a, b, d),
        Op::I32LeS(d, a, b) => (CmpKind::LeS, a, b, d),
        Op::I32LeU(d, a, b) => (CmpKind::LeU, a, b, d),
        Op::I32GeS(d, a, b) => (CmpKind::GeS, a, b, d),
        Op::I32GeU(d, a, b) => (CmpKind::GeU, a, b, d),
        _ => return None,
    };
    Some((kind, a, CmpRhs::Slot(b), d))
}

/// The single branch a comparison and the `br_if` (`negate == false`) or `if` (`negate == true`)
/// testing it fuse into: against zero, a plain conditional branch on `lhs` itself.
pub(super) fn fused_branch(
    kind: CmpKind,
    negate: bool,
    target: BranchTarget,
    lhs: Slot,
    rhs: CmpRhs,
) -> Op {
    match (kind, rhs) {
        (CmpKind::Eq, CmpRhs::Imm(0)) if negate => Op::BrIf(target, lhs),
        (CmpKind::Eq, CmpRhs::Imm(0)) => Op::BrIfNot(target, lhs),
        (CmpKind::Ne, CmpRhs::Imm(0)) if negate => Op::BrIfNot(target, lhs),
        (CmpKind::Ne, CmpRhs::Imm(0)) => Op::BrIf(target, lhs),
        (_, CmpRhs::Slot(rhs)) => Op::BrIfCmp {
            kind,
            negate,
            ip: target.ip,
            lhs,
            rhs,
        },
        (_, CmpRhs::Imm(imm)) => Op::BrIfCmpImm {
            kind,
            negate,
            ip: target.ip,
            lhs,
            imm,
        },
    }
}
