//! Single-pass decoder: wasm operators -> internal `Op` stream.
//!
//! Straight-line operators live in [`numeric`]; structured control flow and the
//! folded sidetable live in [`control`]. The function body is wrapped in an
//! implicit `Block` frame so `return`/branches to the outermost label lower to a
//! branch whose `ip == ops.len()` (the executor returns when `ip >= ops.len()`).

mod control;
mod numeric;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use wasmparser::{BinaryReaderError, FunctionBody, Operator};

use crate::module::op::{CompiledFunc, MemArg, Op};
use crate::value::{FuncType, HeapType, RefType, ValType};
use crate::{Error, Result};

use self::control::{BlockKind, CtrlFrame};

/// Per-module context the translator needs: the type section and the
/// function-index → type-index map (imports + defined functions).
pub(crate) struct CompileCtx<'a> {
    pub types: &'a [FuncType],
    pub func_types: &'a [u32],
}

fn wp_err(e: BinaryReaderError) -> Error {
    Error::msg(e.to_string())
}

/// Maps a `wasmparser` value type to ours (core numeric/vector + funcref/externref).
pub(crate) fn conv_valtype(ty: wasmparser::ValType) -> Result<ValType> {
    Ok(match ty {
        wasmparser::ValType::I32 => ValType::I32,
        wasmparser::ValType::I64 => ValType::I64,
        wasmparser::ValType::F32 => ValType::F32,
        wasmparser::ValType::F64 => ValType::F64,
        wasmparser::ValType::V128 => ValType::V128,
        wasmparser::ValType::Ref(rt) if rt.is_func_ref() => {
            ValType::Ref(RefType::new(rt.is_nullable(), HeapType::Func))
        }
        wasmparser::ValType::Ref(rt) if rt.is_extern_ref() => {
            ValType::Ref(RefType::new(rt.is_nullable(), HeapType::Extern))
        }
        wasmparser::ValType::Ref(_) => {
            return Err(Error::msg(
                "typed/concrete reference types not yet supported",
            ))
        }
    })
}

fn memarg(m: wasmparser::MemArg) -> MemArg {
    MemArg {
        offset: m.offset as u32,
    }
}

/// Translates a single function body into a [`CompiledFunc`]. Assumes the module
/// has already been validated (see `Module::validate`).
pub(crate) fn translate_function(
    ctx: &CompileCtx<'_>,
    type_idx: u32,
    body: &FunctionBody<'_>,
) -> Result<CompiledFunc> {
    let func_ty = &ctx.types[type_idx as usize];
    let n_params = func_ty.params().len() as u32;
    let n_results = func_ty.results().len() as u32;

    let mut local_types: Vec<ValType> = Vec::new();
    for entry in body.get_locals_reader().map_err(wp_err)? {
        let (count, ty) = entry.map_err(wp_err)?;
        let vt = conv_valtype(ty)?;
        for _ in 0..count {
            local_types.push(vt.clone());
        }
    }

    let mut t = Translator::new(ctx);
    t.push_func_frame(n_results);
    for op in body.get_operators_reader().map_err(wp_err)? {
        let op = op.map_err(wp_err)?;
        t.translate(&op)?;
        if t.ctrl.is_empty() {
            break; // function-terminal `end` popped the implicit frame
        }
    }

    Ok(CompiledFunc {
        ops: t.ops.into_boxed_slice(),
        type_idx,
        n_params,
        n_results,
        local_types: local_types.into_boxed_slice(),
        max_operands: t.max_operands,
    })
}

struct Translator<'a> {
    ctx: &'a CompileCtx<'a>,
    ops: Vec<Op>,
    height: u32,
    max_operands: u32,
    ctrl: Vec<CtrlFrame>,
    reachable: bool,
}

impl<'a> Translator<'a> {
    fn new(ctx: &'a CompileCtx<'a>) -> Self {
        Translator {
            ctx,
            ops: Vec::new(),
            height: 0,
            max_operands: 0,
            ctrl: Vec::new(),
            reachable: true,
        }
    }

    fn emit(&mut self, op: Op) {
        self.ops.push(op);
    }

    fn push(&mut self, n: u32) {
        self.height += n;
        self.max_operands = self.max_operands.max(self.height);
    }

    fn pop(&mut self, n: u32) {
        self.height = self.height.saturating_sub(n);
    }

    /// Dispatch one operator. Control constructs always run (to balance the frame
    /// stack); everything else is skipped while unreachable (dead-code elision).
    fn translate(&mut self, op: &Operator<'_>) -> Result<()> {
        use Operator as W;
        match *op {
            W::Block { blockty } => self.push_block(blockty, BlockKind::Block),
            W::Loop { blockty } => self.push_block(blockty, BlockKind::Loop),
            W::If { blockty } => self.push_if(blockty),
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
            W::Unreachable if self.reachable => {
                self.emit(Op::Unreachable);
                self.reachable = false;
            }
            // Skipped while unreachable; otherwise straight-line numeric/mem/var/const.
            W::Br { .. }
            | W::BrIf { .. }
            | W::BrTable { .. }
            | W::Return
            | W::Call { .. }
            | W::CallIndirect { .. }
            | W::Unreachable => {}
            _ if self.reachable => self.straight_line(op)?,
            _ => {}
        }
        Ok(())
    }
}
