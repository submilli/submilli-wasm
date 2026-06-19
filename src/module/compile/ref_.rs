//! Reference value operator translation: `ref.null`, `ref.func`, `ref.is_null`,
//! `ref.as_non_null`. Table access (`table.get` etc.) lives in [`super::table`].

use wasmparser::Operator;

use super::{conv_heaptype, Translator};
use crate::module::op::Op;
use crate::{Error, Result};

impl Translator<'_> {
    pub(super) fn translate_ref(&mut self, op: &Operator<'_>) -> Result<()> {
        use Operator as W;
        match *op {
            W::RefNull { hty } => {
                self.constop(Op::RefNull(conv_heaptype(self.ctx.kinds, hty)?));
            }
            W::RefFunc { function_index } => self.constop(Op::RefFunc(function_index)),
            W::RefIsNull => self.unop(Op::RefIsNull),
            W::RefAsNonNull => self.unop(Op::RefAsNonNull), // height-neutral; null traps
            ref other => return Err(Error::msg(format!("not a ref op: {other:?}"))),
        }
        Ok(())
    }
}
