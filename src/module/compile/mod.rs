//! Single-pass decoder: wasm operators -> internal `Op` stream.
//!
//! `straight_line` (below) dispatches each non-control operator by category: core ops inline, then
//! [`numeric`]/[`memory`]/[`table`]. Structured control flow + the folded sidetable live in
//! [`control`]. The body is wrapped in an implicit `Block` frame so `return`/outermost branches
//! lower to a branch with `ip == ops.len()` (the executor returns when `ip >= ops.len()`).

mod control;
mod conv;
mod gc;
mod memory;
mod numeric;
mod ref_;
#[cfg(feature = "simd")]
mod simd;
mod table;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use wasmparser::{BinaryReaderError, FuncValidator, FunctionBody, Operator, ValidatorResources};

use crate::canon::{AggKind, IrVal, ModuleType};
use crate::module::handler::HandlerSpan;
use crate::module::op::{CompiledFunc, Op};
use crate::{Error, Result};

use self::control::{BlockKind, CtrlFrame};
pub(crate) use self::conv::{
    conv_globaltype, conv_heaptype, conv_memtype, conv_reftype_heap, conv_tabletype, conv_valtype,
};
use self::conv::{memarg, ref_target};

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

    let mut t = Translator::new(ctx, retain_offsets);
    t.push_func_frame(n_results);
    let mut reader = body.get_operators_reader().map_err(wp_err)?;
    while !reader.eof() {
        let (op, offset) = reader.read_with_offset().map_err(wp_err)?;
        fv.op(offset, &op).map_err(wp_err)?;
        t.cur_offset = offset as u32;
        t.translate(&op)?;
    }
    reader.finish().map_err(wp_err)?;

    Ok(CompiledFunc {
        ops: t.ops.into_boxed_slice(),
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
    fn new(ctx: &'a CompileCtx<'a>, retain_offsets: bool) -> Self {
        Translator {
            ctx,
            ops: Vec::new(),
            height: 0,
            max_operands: 0,
            ctrl: Vec::new(),
            reachable: true,
            handlers: Vec::new(),
            cur_offset: 0,
            offsets: retain_offsets.then(Vec::new),
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

    /// Translates a straight-line (non-control) operator: core stack ops (constants,
    /// parametric, variable) inline; memory, table/ref, and numeric by category module.
    #[allow(clippy::too_many_lines)] // mostly the flat memory/table routing groups
    fn straight_line(&mut self, op: &Operator<'_>) -> Result<()> {
        use Operator as W;
        match *op {
            // --- constants ---
            W::I32Const { value } => self.constop(Op::I32Const(value)),
            W::I64Const { value } => self.constop(Op::I64Const(value)),
            W::F32Const { value } => self.constop(Op::F32Const(value.bits())),
            W::F64Const { value } => self.constop(Op::F64Const(value.bits())),

            // --- parametric ---
            W::Drop => {
                self.pop(1);
                self.emit(Op::Drop);
            }
            W::Select | W::TypedSelect { .. } => {
                self.pop(3);
                self.push(1);
                self.emit(Op::Select);
            }

            // --- variable ---
            W::LocalGet { local_index } => self.constop(Op::LocalGet(local_index)),
            W::LocalSet { local_index } => {
                self.pop(1);
                self.emit(Op::LocalSet(local_index));
            }
            W::LocalTee { local_index } => self.emit(Op::LocalTee(local_index)), // height-neutral
            W::GlobalGet { global_index } => self.constop(Op::GlobalGet(global_index)),
            W::GlobalSet { global_index } => {
                self.pop(1);
                self.emit(Op::GlobalSet(global_index));
            }

            // --- memory ops → memory module ---
            W::I32Load { .. }
            | W::I64Load { .. }
            | W::F32Load { .. }
            | W::F64Load { .. }
            | W::I32Load8S { .. }
            | W::I32Load8U { .. }
            | W::I32Load16S { .. }
            | W::I32Load16U { .. }
            | W::I64Load8S { .. }
            | W::I64Load8U { .. }
            | W::I64Load16S { .. }
            | W::I64Load16U { .. }
            | W::I64Load32S { .. }
            | W::I64Load32U { .. }
            | W::I32Store { .. }
            | W::I64Store { .. }
            | W::F32Store { .. }
            | W::F64Store { .. }
            | W::I32Store8 { .. }
            | W::I32Store16 { .. }
            | W::I64Store8 { .. }
            | W::I64Store16 { .. }
            | W::I64Store32 { .. }
            | W::MemorySize { .. }
            | W::MemoryGrow { .. }
            | W::MemoryInit { .. }
            | W::DataDrop { .. }
            | W::MemoryCopy { .. }
            | W::MemoryFill { .. } => self.translate_memory(op)?,

            // --- reference value ops → ref module ---
            W::RefNull { .. } | W::RefFunc { .. } | W::RefIsNull | W::RefAsNonNull => {
                self.translate_ref(op)?;
            }

            // --- table ops → table module ---
            W::TableInit { .. }
            | W::TableCopy { .. }
            | W::ElemDrop { .. }
            | W::TableGet { .. }
            | W::TableSet { .. }
            | W::TableSize { .. }
            | W::TableGrow { .. }
            | W::TableFill { .. } => {
                self.translate_table(op)?;
            }

            // --- GC aggregates (struct/array/i31) → gc module ---
            W::StructNew { .. }
            | W::StructNewDefault { .. }
            | W::StructGet { .. }
            | W::StructGetS { .. }
            | W::StructGetU { .. }
            | W::StructSet { .. }
            | W::ArrayNew { .. }
            | W::ArrayNewDefault { .. }
            | W::ArrayNewFixed { .. }
            | W::ArrayNewData { .. }
            | W::ArrayNewElem { .. }
            | W::ArrayGet { .. }
            | W::ArrayGetS { .. }
            | W::ArrayGetU { .. }
            | W::ArraySet { .. }
            | W::ArrayLen
            | W::ArrayFill { .. }
            | W::ArrayCopy { .. }
            | W::ArrayInitData { .. }
            | W::ArrayInitElem { .. }
            | W::RefI31
            | W::I31GetS
            | W::I31GetU
            | W::RefTestNonNull { .. }
            | W::RefTestNullable { .. }
            | W::RefCastNonNull { .. }
            | W::RefCastNullable { .. }
            | W::RefEq
            | W::AnyConvertExtern
            | W::ExternConvertAny => self.translate_gc(op)?,

            // --- SIMD (v128) → simd module (feature-gated, #37) ---
            #[cfg(feature = "simd")]
            _ if self.translate_simd(op) => {}

            // --- numeric / comparison / conversion / sign-ext / saturating / nop ---
            _ => self.translate_numeric(op)?,
        }
        Ok(())
    }

    /// Dispatch one operator. Control constructs always run (to balance the frame
    /// stack); everything else is skipped while unreachable (dead-code elision).
    #[allow(clippy::too_many_lines)] // flat control-flow dispatch
    fn translate(&mut self, op: &Operator<'_>) -> Result<()> {
        use Operator as W;
        match *op {
            W::Block { blockty } => self.push_block(blockty, BlockKind::Block),
            W::Loop { blockty } => self.push_block(blockty, BlockKind::Loop),
            W::If { blockty } => self.push_if(blockty),
            W::TryTable { ref try_table } => self.push_try_table(try_table),
            W::Else => self.do_else(),
            W::End => self.do_end(),
            W::Br { relative_depth } if self.reachable => self.br(relative_depth),
            W::BrIf { relative_depth } if self.reachable => self.br_if(relative_depth),
            W::BrTable { ref targets } if self.reachable => self.br_table(targets)?,
            W::Return if self.reachable => self.ret(),
            W::Call { function_index } if self.reachable => self.call(function_index),
            W::CallIndirect {
                type_index,
                table_index,
            } if self.reachable => self.call_indirect(type_index, table_index),
            W::CallRef { type_index } if self.reachable => self.call_ref(type_index),
            W::ReturnCall { function_index } if self.reachable => self.return_call(function_index),
            W::ReturnCallIndirect {
                type_index,
                table_index,
            } if self.reachable => self.return_call_indirect(type_index, table_index),
            W::ReturnCallRef { type_index } if self.reachable => self.return_call_ref(type_index),
            W::BrOnNull { relative_depth } if self.reachable => self.br_on_null(relative_depth),
            W::BrOnNonNull { relative_depth } if self.reachable => {
                self.br_on_non_null(relative_depth);
            }
            W::BrOnCast {
                relative_depth,
                to_ref_type,
                ..
            } if self.reachable => {
                let (ty, nullable) = ref_target(self.ctx.kinds, to_ref_type)?;
                self.br_on_cast(relative_depth, ty, nullable, false);
            }
            W::BrOnCastFail {
                relative_depth,
                to_ref_type,
                ..
            } if self.reachable => {
                let (ty, nullable) = ref_target(self.ctx.kinds, to_ref_type)?;
                self.br_on_cast(relative_depth, ty, nullable, true);
            }
            W::Unreachable if self.reachable => {
                self.emit(Op::Unreachable);
                self.reachable = false;
            }
            W::Throw { tag_index } if self.reachable => self.throw(tag_index),
            W::ThrowRef if self.reachable => self.throw_ref(),
            // Skipped while unreachable; otherwise straight-line numeric/mem/var/const.
            W::Br { .. }
            | W::BrIf { .. }
            | W::BrTable { .. }
            | W::Return
            | W::Call { .. }
            | W::CallIndirect { .. }
            | W::CallRef { .. }
            | W::ReturnCall { .. }
            | W::ReturnCallIndirect { .. }
            | W::ReturnCallRef { .. }
            | W::BrOnNull { .. }
            | W::BrOnNonNull { .. }
            | W::BrOnCast { .. }
            | W::BrOnCastFail { .. }
            | W::Throw { .. }
            | W::ThrowRef
            | W::Unreachable => {}
            _ if self.reachable => self.straight_line(op)?,
            _ => {}
        }
        Ok(())
    }
}
