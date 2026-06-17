//! Straight-line operator translation: constants, parametric, variable, memory,
//! and the full numeric/comparison/conversion set. Called only while reachable.

use wasmparser::Operator;

use super::{memarg, Translator};
use crate::module::op::Op;
use crate::{Error, Result};

impl Translator<'_> {
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

    #[allow(clippy::too_many_lines)] // flat opcode dispatch; arms are one-liners
    pub(super) fn straight_line(&mut self, op: &Operator<'_>) -> Result<()> {
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

            // --- memory loads (pop addr, push value) ---
            W::I32Load { memarg: m } => self.unop(Op::I32Load(memarg(m))),
            W::I64Load { memarg: m } => self.unop(Op::I64Load(memarg(m))),
            W::F32Load { memarg: m } => self.unop(Op::F32Load(memarg(m))),
            W::F64Load { memarg: m } => self.unop(Op::F64Load(memarg(m))),
            W::I32Load8S { memarg: m } => self.unop(Op::I32Load8S(memarg(m))),
            W::I32Load8U { memarg: m } => self.unop(Op::I32Load8U(memarg(m))),
            W::I32Load16S { memarg: m } => self.unop(Op::I32Load16S(memarg(m))),
            W::I32Load16U { memarg: m } => self.unop(Op::I32Load16U(memarg(m))),
            W::I64Load8S { memarg: m } => self.unop(Op::I64Load8S(memarg(m))),
            W::I64Load8U { memarg: m } => self.unop(Op::I64Load8U(memarg(m))),
            W::I64Load16S { memarg: m } => self.unop(Op::I64Load16S(memarg(m))),
            W::I64Load16U { memarg: m } => self.unop(Op::I64Load16U(memarg(m))),
            W::I64Load32S { memarg: m } => self.unop(Op::I64Load32S(memarg(m))),
            W::I64Load32U { memarg: m } => self.unop(Op::I64Load32U(memarg(m))),

            // --- memory stores (pop addr + value) ---
            W::I32Store { memarg: m } => self.store(Op::I32Store(memarg(m))),
            W::I64Store { memarg: m } => self.store(Op::I64Store(memarg(m))),
            W::F32Store { memarg: m } => self.store(Op::F32Store(memarg(m))),
            W::F64Store { memarg: m } => self.store(Op::F64Store(memarg(m))),
            W::I32Store8 { memarg: m } => self.store(Op::I32Store8(memarg(m))),
            W::I32Store16 { memarg: m } => self.store(Op::I32Store16(memarg(m))),
            W::I64Store8 { memarg: m } => self.store(Op::I64Store8(memarg(m))),
            W::I64Store16 { memarg: m } => self.store(Op::I64Store16(memarg(m))),
            W::I64Store32 { memarg: m } => self.store(Op::I64Store32(memarg(m))),

            // --- memory management ---
            W::MemorySize { .. } => self.constop(Op::MemorySize),
            W::MemoryGrow { .. } => self.unop(Op::MemoryGrow),
            W::MemoryInit { data_index, .. } => {
                self.pop(3);
                self.emit(Op::MemoryInit(data_index));
            }
            W::DataDrop { data_index } => self.emit(Op::DataDrop(data_index)),
            W::MemoryCopy { .. } => {
                self.pop(3);
                self.emit(Op::MemoryCopy);
            }
            W::MemoryFill { .. } => {
                self.pop(3);
                self.emit(Op::MemoryFill);
            }

            // --- table management (bulk-memory subset) ---
            W::TableInit { elem_index, table } => {
                self.pop(3);
                self.emit(Op::TableInit {
                    elem: elem_index,
                    table,
                });
            }
            W::TableCopy {
                dst_table,
                src_table,
            } => {
                self.pop(3);
                self.emit(Op::TableCopy {
                    dst_table,
                    src_table,
                });
            }
            W::ElemDrop { elem_index } => self.emit(Op::ElemDrop(elem_index)),

            // --- i32 comparisons / numeric ---
            W::I32Eqz => self.unop(Op::I32Eqz),
            W::I32Eq => self.binop(Op::I32Eq),
            W::I32Ne => self.binop(Op::I32Ne),
            W::I32LtS => self.binop(Op::I32LtS),
            W::I32LtU => self.binop(Op::I32LtU),
            W::I32GtS => self.binop(Op::I32GtS),
            W::I32GtU => self.binop(Op::I32GtU),
            W::I32LeS => self.binop(Op::I32LeS),
            W::I32LeU => self.binop(Op::I32LeU),
            W::I32GeS => self.binop(Op::I32GeS),
            W::I32GeU => self.binop(Op::I32GeU),
            W::I32Clz => self.unop(Op::I32Clz),
            W::I32Ctz => self.unop(Op::I32Ctz),
            W::I32Popcnt => self.unop(Op::I32Popcnt),
            W::I32Add => self.binop(Op::I32Add),
            W::I32Sub => self.binop(Op::I32Sub),
            W::I32Mul => self.binop(Op::I32Mul),
            W::I32DivS => self.binop(Op::I32DivS),
            W::I32DivU => self.binop(Op::I32DivU),
            W::I32RemS => self.binop(Op::I32RemS),
            W::I32RemU => self.binop(Op::I32RemU),
            W::I32And => self.binop(Op::I32And),
            W::I32Or => self.binop(Op::I32Or),
            W::I32Xor => self.binop(Op::I32Xor),
            W::I32Shl => self.binop(Op::I32Shl),
            W::I32ShrS => self.binop(Op::I32ShrS),
            W::I32ShrU => self.binop(Op::I32ShrU),
            W::I32Rotl => self.binop(Op::I32Rotl),
            W::I32Rotr => self.binop(Op::I32Rotr),

            // --- i64 comparisons / numeric ---
            W::I64Eqz => self.unop(Op::I64Eqz),
            W::I64Eq => self.binop(Op::I64Eq),
            W::I64Ne => self.binop(Op::I64Ne),
            W::I64LtS => self.binop(Op::I64LtS),
            W::I64LtU => self.binop(Op::I64LtU),
            W::I64GtS => self.binop(Op::I64GtS),
            W::I64GtU => self.binop(Op::I64GtU),
            W::I64LeS => self.binop(Op::I64LeS),
            W::I64LeU => self.binop(Op::I64LeU),
            W::I64GeS => self.binop(Op::I64GeS),
            W::I64GeU => self.binop(Op::I64GeU),
            W::I64Clz => self.unop(Op::I64Clz),
            W::I64Ctz => self.unop(Op::I64Ctz),
            W::I64Popcnt => self.unop(Op::I64Popcnt),
            W::I64Add => self.binop(Op::I64Add),
            W::I64Sub => self.binop(Op::I64Sub),
            W::I64Mul => self.binop(Op::I64Mul),
            W::I64DivS => self.binop(Op::I64DivS),
            W::I64DivU => self.binop(Op::I64DivU),
            W::I64RemS => self.binop(Op::I64RemS),
            W::I64RemU => self.binop(Op::I64RemU),
            W::I64And => self.binop(Op::I64And),
            W::I64Or => self.binop(Op::I64Or),
            W::I64Xor => self.binop(Op::I64Xor),
            W::I64Shl => self.binop(Op::I64Shl),
            W::I64ShrS => self.binop(Op::I64ShrS),
            W::I64ShrU => self.binop(Op::I64ShrU),
            W::I64Rotl => self.binop(Op::I64Rotl),
            W::I64Rotr => self.binop(Op::I64Rotr),

            // --- f32 ---
            W::F32Eq => self.binop(Op::F32Eq),
            W::F32Ne => self.binop(Op::F32Ne),
            W::F32Lt => self.binop(Op::F32Lt),
            W::F32Gt => self.binop(Op::F32Gt),
            W::F32Le => self.binop(Op::F32Le),
            W::F32Ge => self.binop(Op::F32Ge),
            W::F32Abs => self.unop(Op::F32Abs),
            W::F32Neg => self.unop(Op::F32Neg),
            W::F32Ceil => self.unop(Op::F32Ceil),
            W::F32Floor => self.unop(Op::F32Floor),
            W::F32Trunc => self.unop(Op::F32Trunc),
            W::F32Nearest => self.unop(Op::F32Nearest),
            W::F32Sqrt => self.unop(Op::F32Sqrt),
            W::F32Add => self.binop(Op::F32Add),
            W::F32Sub => self.binop(Op::F32Sub),
            W::F32Mul => self.binop(Op::F32Mul),
            W::F32Div => self.binop(Op::F32Div),
            W::F32Min => self.binop(Op::F32Min),
            W::F32Max => self.binop(Op::F32Max),
            W::F32Copysign => self.binop(Op::F32Copysign),

            // --- f64 ---
            W::F64Eq => self.binop(Op::F64Eq),
            W::F64Ne => self.binop(Op::F64Ne),
            W::F64Lt => self.binop(Op::F64Lt),
            W::F64Gt => self.binop(Op::F64Gt),
            W::F64Le => self.binop(Op::F64Le),
            W::F64Ge => self.binop(Op::F64Ge),
            W::F64Abs => self.unop(Op::F64Abs),
            W::F64Neg => self.unop(Op::F64Neg),
            W::F64Ceil => self.unop(Op::F64Ceil),
            W::F64Floor => self.unop(Op::F64Floor),
            W::F64Trunc => self.unop(Op::F64Trunc),
            W::F64Nearest => self.unop(Op::F64Nearest),
            W::F64Sqrt => self.unop(Op::F64Sqrt),
            W::F64Add => self.binop(Op::F64Add),
            W::F64Sub => self.binop(Op::F64Sub),
            W::F64Mul => self.binop(Op::F64Mul),
            W::F64Div => self.binop(Op::F64Div),
            W::F64Min => self.binop(Op::F64Min),
            W::F64Max => self.binop(Op::F64Max),
            W::F64Copysign => self.binop(Op::F64Copysign),

            // --- conversions (pop 1 / push 1) ---
            W::I32WrapI64 => self.unop(Op::I32WrapI64),
            W::I32TruncF32S => self.unop(Op::I32TruncF32S),
            W::I32TruncF32U => self.unop(Op::I32TruncF32U),
            W::I32TruncF64S => self.unop(Op::I32TruncF64S),
            W::I32TruncF64U => self.unop(Op::I32TruncF64U),
            W::I64ExtendI32S => self.unop(Op::I64ExtendI32S),
            W::I64ExtendI32U => self.unop(Op::I64ExtendI32U),
            W::I64TruncF32S => self.unop(Op::I64TruncF32S),
            W::I64TruncF32U => self.unop(Op::I64TruncF32U),
            W::I64TruncF64S => self.unop(Op::I64TruncF64S),
            W::I64TruncF64U => self.unop(Op::I64TruncF64U),
            W::F32ConvertI32S => self.unop(Op::F32ConvertI32S),
            W::F32ConvertI32U => self.unop(Op::F32ConvertI32U),
            W::F32ConvertI64S => self.unop(Op::F32ConvertI64S),
            W::F32ConvertI64U => self.unop(Op::F32ConvertI64U),
            W::F32DemoteF64 => self.unop(Op::F32DemoteF64),
            W::F64ConvertI32S => self.unop(Op::F64ConvertI32S),
            W::F64ConvertI32U => self.unop(Op::F64ConvertI32U),
            W::F64ConvertI64S => self.unop(Op::F64ConvertI64S),
            W::F64ConvertI64U => self.unop(Op::F64ConvertI64U),
            W::F64PromoteF32 => self.unop(Op::F64PromoteF32),
            W::I32ReinterpretF32 => self.unop(Op::I32ReinterpretF32),
            W::I64ReinterpretF64 => self.unop(Op::I64ReinterpretF64),
            W::F32ReinterpretI32 => self.unop(Op::F32ReinterpretI32),
            W::F64ReinterpretI64 => self.unop(Op::F64ReinterpretI64),

            // --- sign-extension ops ---
            W::I32Extend8S => self.unop(Op::I32Extend8S),
            W::I32Extend16S => self.unop(Op::I32Extend16S),
            W::I64Extend8S => self.unop(Op::I64Extend8S),
            W::I64Extend16S => self.unop(Op::I64Extend16S),
            W::I64Extend32S => self.unop(Op::I64Extend32S),

            // --- saturating float-to-int ---
            W::I32TruncSatF32S => self.unop(Op::I32TruncSatF32S),
            W::I32TruncSatF32U => self.unop(Op::I32TruncSatF32U),
            W::I32TruncSatF64S => self.unop(Op::I32TruncSatF64S),
            W::I32TruncSatF64U => self.unop(Op::I32TruncSatF64U),
            W::I64TruncSatF32S => self.unop(Op::I64TruncSatF32S),
            W::I64TruncSatF32U => self.unop(Op::I64TruncSatF32U),
            W::I64TruncSatF64S => self.unop(Op::I64TruncSatF64S),
            W::I64TruncSatF64U => self.unop(Op::I64TruncSatF64U),

            W::Nop => self.emit(Op::Nop),

            // ref/table/gc/simd and anything else: later phases.
            ref other => {
                return Err(Error::msg(format!(
                    "operator not yet supported in this phase: {other:?}"
                )))
            }
        }
        Ok(())
    }
}
