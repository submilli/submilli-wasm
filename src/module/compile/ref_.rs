//! Inline `visit_*` lowering of reference value ops: `ref.null`, `ref.func`, `ref.is_null`,
//! `ref.as_non_null`. Table access (`table.get` etc.) lives in [`super::table`]. Infallible arms
//! still return `Result<()>` for the uniform visitor delegation.
#![allow(clippy::unnecessary_wraps)]

use super::{conv_heaptype, Translator};
use crate::module::op::Op;
use crate::Result;

/// Inline lowering of reference value ops. `ref.null` resolves its heap type (fallible); the rest
/// are fixed 1:1 maps.
impl Translator<'_> {
    pub(in crate::module::compile) fn visit_ref_null(
        &mut self,
        hty: wasmparser::HeapType,
    ) -> Result<()> {
        if self.reachable {
            let ty = conv_heaptype(self.ctx.kinds, hty)?;
            self.constop(Op::RefNull(ty));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_ref_func(&mut self, function_index: u32) -> Result<()> {
        if self.reachable {
            self.constop(Op::RefFunc(function_index));
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_ref_is_null(&mut self) -> Result<()> {
        if self.reachable {
            self.unop(Op::RefIsNull);
        }
        Ok(())
    }

    pub(in crate::module::compile) fn visit_ref_as_non_null(&mut self) -> Result<()> {
        if self.reachable {
            self.unop(Op::RefAsNonNull);
        }
        Ok(())
    }
}
