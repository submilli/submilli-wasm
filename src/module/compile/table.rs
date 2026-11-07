//! Inline `visit_*` lowering of table ops: the bulk-memory table ops (`table.init/copy`,
//! `elem.drop`) and `table.get/set/size/grow/fill`. Reference *value* ops (`ref.null` etc.) live
//! in [`super::ref_`]. Infallible arms still return `Result<()>` for the uniform visitor delegation.
#![allow(clippy::unnecessary_wraps)]

use super::Translator;
use crate::module::op::Op;
use crate::Result;

/// Inline lowering of table ops (bulk-memory `table.init/copy`, `elem.drop`, and
/// `table.get/set/size/grow/fill`). Each carries its own stack effect.
impl Translator<'_> {
    pub(in crate::module::compile) fn visit_table_init(
        &mut self,
        elem_index: u32,
        table: u32,
    ) -> Result<()> {
        if self.reachable {
            self.consume(3, |s| Op::TableInit {
                elem: elem_index,
                table,
                s,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_copy(
        &mut self,
        dst_table: u32,
        src_table: u32,
    ) -> Result<()> {
        if self.reachable {
            self.consume(3, |s| Op::TableCopy {
                dst_table,
                src_table,
                s,
            });
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_elem_drop(&mut self, elem_index: u32) -> Result<()> {
        if self.reachable {
            self.emit(Op::ElemDrop(elem_index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_get(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.unop(|s| Op::TableGet(table, s));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_set(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.consume(2, |s| Op::TableSet(table, s));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_size(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.constop(|s| Op::TableSize(table, s));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_grow(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.binop(|s| Op::TableGrow(table, s));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_fill(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.consume(3, |s| Op::TableFill(table, s));
        }
        Ok(())
    }
}
