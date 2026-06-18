//! Table operator translation: the bulk-memory table ops (`table.init/copy`, `elem.drop`)
//! and `table.get/set/size/grow/fill`. Reference *value* ops (`ref.null` etc.) live in
//! [`super::ref_`]. `super::straight_line` routes only these ops here.

use wasmparser::Operator;

use super::Translator;
use crate::module::op::Op;
use crate::{Error, Result};

impl Translator<'_> {
    pub(super) fn translate_table(&mut self, op: &Operator<'_>) -> Result<()> {
        use Operator as W;
        match *op {
            // bulk-memory table ops
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

            // table get/set/size/grow/fill
            W::TableGet { table } => self.unop(Op::TableGet(table)),
            W::TableSet { table } => {
                self.pop(2);
                self.emit(Op::TableSet(table));
            }
            W::TableSize { table } => self.constop(Op::TableSize(table)),
            W::TableGrow { table } => {
                self.pop(2);
                self.push(1);
                self.emit(Op::TableGrow(table));
            }
            W::TableFill { table } => {
                self.pop(3);
                self.emit(Op::TableFill(table));
            }
            ref other => return Err(Error::msg(format!("not a table/ref op: {other:?}"))),
        }
        Ok(())
    }
}
