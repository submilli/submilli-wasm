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
            self.pop(3);
            self.emit(Op::TableInit {
                elem: elem_index,
                table,
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
            self.pop(3);
            self.emit(Op::TableCopy {
                dst_table,
                src_table,
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
            self.unop(Op::TableGet(table));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_set(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.pop(2);
            self.emit(Op::TableSet(table));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_size(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.constop(Op::TableSize(table));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_grow(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.pop(2);
            self.push(1);
            self.emit(Op::TableGrow(table));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_table_fill(&mut self, table: u32) -> Result<()> {
        if self.reachable {
            self.pop(3);
            self.emit(Op::TableFill(table));
        }
        Ok(())
    }
}
