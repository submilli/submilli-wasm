//! Memory operator translation: loads, stores, and `memory.size/grow/init/copy/
//! fill`/`data.drop`. `super::straight_line` routes only memory ops here.

use wasmparser::Operator;

use super::{memarg, Translator};
use crate::module::op::Op;
use crate::{Error, Result};

impl Translator<'_> {
    pub(super) fn translate_memory(&mut self, op: &Operator<'_>) -> Result<()> {
        use Operator as W;
        match *op {
            // loads (pop addr, push value)
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

            // stores (pop addr + value)
            W::I32Store { memarg: m } => self.store(Op::I32Store(memarg(m))),
            W::I64Store { memarg: m } => self.store(Op::I64Store(memarg(m))),
            W::F32Store { memarg: m } => self.store(Op::F32Store(memarg(m))),
            W::F64Store { memarg: m } => self.store(Op::F64Store(memarg(m))),
            W::I32Store8 { memarg: m } => self.store(Op::I32Store8(memarg(m))),
            W::I32Store16 { memarg: m } => self.store(Op::I32Store16(memarg(m))),
            W::I64Store8 { memarg: m } => self.store(Op::I64Store8(memarg(m))),
            W::I64Store16 { memarg: m } => self.store(Op::I64Store16(memarg(m))),
            W::I64Store32 { memarg: m } => self.store(Op::I64Store32(memarg(m))),

            // management
            W::MemorySize { mem } => self.constop(Op::MemorySize(mem)),
            W::MemoryGrow { mem } => self.unop(Op::MemoryGrow(mem)),
            W::MemoryInit { data_index, mem } => {
                self.pop(3);
                self.emit(Op::MemoryInit(data_index, mem));
            }
            W::DataDrop { data_index } => self.emit(Op::DataDrop(data_index)),
            W::MemoryCopy { dst_mem, src_mem } => {
                self.pop(3);
                self.emit(Op::MemoryCopy(dst_mem, src_mem));
            }
            W::MemoryFill { mem } => {
                self.pop(3);
                self.emit(Op::MemoryFill(mem));
            }
            ref other => return Err(Error::msg(format!("not a memory op: {other:?}"))),
        }
        Ok(())
    }
}
