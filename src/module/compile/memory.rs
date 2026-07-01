//! Inline `visit_*` lowering of memory ops: loads, stores, and `memory.size/grow/init/copy/
//! fill`/`data.drop`. Infallible arms still return `Result<()>` for the uniform visitor delegation.
#![allow(clippy::unnecessary_wraps)]

use super::Translator;
use crate::module::op::Op;
use crate::Result;

/// Inline lowering of memory ops. Loads/stores are 1:1 memarg maps (via [`lower_memarg`]); the
/// management ops carry their own stack effect and are written out.
impl Translator<'_> {
    super::visit::lower_memarg! {
            visit_i32_load => unop I32Load;
            visit_i64_load => unop I64Load;
            visit_f32_load => unop F32Load;
            visit_f64_load => unop F64Load;
            visit_i32_load8_s => unop I32Load8S;
            visit_i32_load8_u => unop I32Load8U;
            visit_i32_load16_s => unop I32Load16S;
            visit_i32_load16_u => unop I32Load16U;
            visit_i64_load8_s => unop I64Load8S;
            visit_i64_load8_u => unop I64Load8U;
            visit_i64_load16_s => unop I64Load16S;
            visit_i64_load16_u => unop I64Load16U;
            visit_i64_load32_s => unop I64Load32S;
            visit_i64_load32_u => unop I64Load32U;
            visit_i32_store => store I32Store;
            visit_i64_store => store I64Store;
            visit_f32_store => store F32Store;
            visit_f64_store => store F64Store;
            visit_i32_store8 => store I32Store8;
            visit_i32_store16 => store I32Store16;
            visit_i64_store8 => store I64Store8;
            visit_i64_store16 => store I64Store16;
            visit_i64_store32 => store I64Store32;
    }

    pub(in crate::module::compile) fn visit_memory_size(&mut self, mem: u32) -> Result<()> {
        if self.reachable {
            self.constop(Op::MemorySize(mem));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_memory_grow(&mut self, mem: u32) -> Result<()> {
        if self.reachable {
            self.unop(Op::MemoryGrow(mem));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_memory_init(
        &mut self,
        data_index: u32,
        mem: u32,
    ) -> Result<()> {
        if self.reachable {
            self.pop(3);
            self.emit(Op::MemoryInit(data_index, mem));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_data_drop(&mut self, data_index: u32) -> Result<()> {
        if self.reachable {
            self.emit(Op::DataDrop(data_index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_memory_copy(
        &mut self,
        dst_mem: u32,
        src_mem: u32,
    ) -> Result<()> {
        if self.reachable {
            self.pop(3);
            self.emit(Op::MemoryCopy(dst_mem, src_mem));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_memory_fill(&mut self, mem: u32) -> Result<()> {
        if self.reachable {
            self.pop(3);
            self.emit(Op::MemoryFill(mem));
        }
        Ok(())
    }
}
