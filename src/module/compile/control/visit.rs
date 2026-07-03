//! Inline `visit_*` lowering for control flow: structural frames (`block`/`loop`/`if`/`else`/`end`/
//! `try_table`) always run to balance the frame stack; branches, calls, and throws are skipped while
//! unreachable (dead-code elision), delegating to the helpers in the sibling `control` modules.
//!
//! Many methods are infallible but must return `Result<()>` for the uniform visitor delegation
//! (their fallible siblings — e.g. `br_on_cast` — return real errors); hence the module-wide allow.
#![allow(clippy::unnecessary_wraps)]

use wasmparser::{BlockType, BrTable, TryTable};

use super::BlockKind;
use crate::module::compile::conv::ref_target;
use crate::module::compile::Translator;
use crate::module::op::Op;
use crate::Result;

impl Translator<'_> {
    // --- structural: always run ---
    pub(in crate::module::compile) fn visit_block(&mut self, blockty: BlockType) -> Result<()> {
        self.push_block(blockty, BlockKind::Block);
        Ok(())
    }

    pub(in crate::module::compile) fn visit_loop(&mut self, blockty: BlockType) -> Result<()> {
        self.push_block(blockty, BlockKind::Loop);
        Ok(())
    }

    pub(in crate::module::compile) fn visit_if(&mut self, blockty: BlockType) -> Result<()> {
        self.push_if(blockty);
        Ok(())
    }

    pub(in crate::module::compile) fn visit_else(&mut self) -> Result<()> {
        self.do_else()?;
        Ok(())
    }

    pub(in crate::module::compile) fn visit_end(&mut self) -> Result<()> {
        self.do_end()?;
        Ok(())
    }

    pub(in crate::module::compile) fn visit_try_table(
        &mut self,
        try_table: TryTable,
    ) -> Result<()> {
        self.push_try_table(&try_table);
        Ok(())
    }

    // --- branches / calls / throws: skipped while unreachable ---
    pub(in crate::module::compile) fn visit_br(&mut self, relative_depth: u32) -> Result<()> {
        if self.reachable {
            self.br(relative_depth)?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_br_if(&mut self, relative_depth: u32) -> Result<()> {
        if self.reachable {
            self.br_if(relative_depth)?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_br_table(
        &mut self,
        targets: BrTable<'_>,
    ) -> Result<()> {
        if self.reachable {
            self.br_table(&targets)?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_return(&mut self) -> Result<()> {
        if self.reachable {
            self.ret()?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_call(&mut self, function_index: u32) -> Result<()> {
        if self.reachable {
            self.call(function_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_call_indirect(
        &mut self,
        type_index: u32,
        table_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.call_indirect(type_index, table_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_call_ref(&mut self, type_index: u32) -> Result<()> {
        if self.reachable {
            self.call_ref(type_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_return_call(
        &mut self,
        function_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.return_call(function_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_return_call_indirect(
        &mut self,
        type_index: u32,
        table_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.return_call_indirect(type_index, table_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_return_call_ref(
        &mut self,
        type_index: u32,
    ) -> Result<()> {
        if self.reachable {
            self.return_call_ref(type_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_br_on_null(
        &mut self,
        relative_depth: u32,
    ) -> Result<()> {
        if self.reachable {
            self.br_on_null(relative_depth)?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_br_on_non_null(
        &mut self,
        relative_depth: u32,
    ) -> Result<()> {
        if self.reachable {
            self.br_on_non_null(relative_depth)?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_br_on_cast(
        &mut self,
        relative_depth: u32,
        _from_ref_type: wasmparser::RefType,
        to_ref_type: wasmparser::RefType,
    ) -> Result<()> {
        if self.reachable {
            let (ty, nullable) = ref_target(self.ctx.kinds, to_ref_type)?;
            self.br_on_cast(relative_depth, ty, nullable, false)?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_br_on_cast_fail(
        &mut self,
        relative_depth: u32,
        _from_ref_type: wasmparser::RefType,
        to_ref_type: wasmparser::RefType,
    ) -> Result<()> {
        if self.reachable {
            let (ty, nullable) = ref_target(self.ctx.kinds, to_ref_type)?;
            self.br_on_cast(relative_depth, ty, nullable, true)?;
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_throw(&mut self, tag_index: u32) -> Result<()> {
        if self.reachable {
            self.throw(tag_index);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_throw_ref(&mut self) -> Result<()> {
        if self.reachable {
            self.throw_ref();
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_unreachable(&mut self) -> Result<()> {
        if self.reachable {
            self.emit(Op::Unreachable);
            self.reachable = false;
        }
        Ok(())
    }
}
