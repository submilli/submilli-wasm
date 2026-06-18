//! Single-pass decoder: wasm operators -> internal `Op` stream.
//!
//! `straight_line` (below) dispatches each non-control operator by category: core
//! ops (constants/parametric/variable) inline, then [`numeric`]/[`memory`]/[`table`].
//! Structured control flow and the folded sidetable live in [`control`]. The function
//! body is wrapped in an implicit `Block` frame so `return`/branches to the outermost
//! label lower to a branch whose `ip == ops.len()` (the executor returns when
//! `ip >= ops.len()`).

mod control;
mod memory;
mod numeric;
mod ref_;
mod table;
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

/// Maps a `wasmparser` value type to ours (core numeric/vector + funcref/externref,
/// including typed/non-nullable references from function-references, #26d). `types` is the
/// module's (so-far-converted) type section, used to resolve concrete func-type references.
pub(crate) fn conv_valtype(types: &[FuncType], ty: wasmparser::ValType) -> Result<ValType> {
    Ok(match ty {
        wasmparser::ValType::I32 => ValType::I32,
        wasmparser::ValType::I64 => ValType::I64,
        wasmparser::ValType::F32 => ValType::F32,
        wasmparser::ValType::F64 => ValType::F64,
        wasmparser::ValType::V128 => ValType::V128,
        wasmparser::ValType::Ref(rt) => ValType::Ref(RefType::new(
            rt.is_nullable(),
            conv_heaptype(types, rt.heap_type())?,
        )),
    })
}

/// Converts a wasmparser heap type to ours: the reference-types abstract set (`func`/`extern`
/// and their `nofunc`/`noextern` bottoms) plus, for function-references, concrete func types.
/// GC is off, so a concrete type is always a function type — resolved to its structural
/// signature for identity. A concrete reference we can't yet resolve (a forward/recursive
/// type index) collapses to abstract `func`; full rec-group canonicalization is #27c.
pub(crate) fn conv_heaptype(types: &[FuncType], hty: wasmparser::HeapType) -> Result<HeapType> {
    use wasmparser::{AbstractHeapType as A, HeapType as H};
    Ok(match hty {
        H::Abstract {
            shared: false,
            ty: A::Func | A::NoFunc,
        } => HeapType::Func,
        H::Abstract {
            shared: false,
            ty: A::Extern | A::NoExtern,
        } => HeapType::Extern,
        H::Concrete(idx) | H::Exact(idx) => match idx.as_module_index() {
            Some(i) if (i as usize) < types.len() => {
                HeapType::ConcreteFunc(types[i as usize].clone())
            }
            _ => HeapType::Func,
        },
        H::Abstract { .. } => return Err(Error::msg("unsupported heap type (GC)")),
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
        let vt = conv_valtype(ctx.types, ty)?;
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

            // --- numeric / comparison / conversion / sign-ext / saturating / nop ---
            _ => self.translate_numeric(op)?,
        }
        Ok(())
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
            W::CallRef { type_index } if self.reachable => self.call_ref(type_index),
            W::BrOnNull { relative_depth } if self.reachable => self.br_on_null(relative_depth),
            W::BrOnNonNull { relative_depth } if self.reachable => {
                self.br_on_non_null(relative_depth);
            }
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
            | W::CallRef { .. }
            | W::BrOnNull { .. }
            | W::BrOnNonNull { .. }
            | W::Unreachable => {}
            _ if self.reachable => self.straight_line(op)?,
            _ => {}
        }
        Ok(())
    }
}
