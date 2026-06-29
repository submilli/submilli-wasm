//! Reference value operators: `ref.null`, `ref.func`, `ref.is_null`, `ref.as_non_null`.
//! Pure operand-stack/reference manipulation — table access lives in [`super::table`].

use super::Execution;
use crate::instance::Instance;
use crate::module::op::Op;
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::value::Val;
use crate::{Error, Result};

impl Execution {
    pub(super) fn exec_ref(
        &mut self,
        inner: &StoreInner,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        match op {
            Op::RefNull(heap) => {
                self.push(Val::null_for_heap(heap));
                Ok(())
            }
            Op::RefFunc(f) => {
                // `f` is a wasmparser-validated function index for this instance (#33 carve-out).
                #[allow(clippy::indexing_slicing)]
                let func = inner.instance(instance).funcs[*f as usize];
                self.push(Val::FuncRef(Some(func)));
                Ok(())
            }
            Op::RefIsNull => {
                let r = self.pop();
                self.push(Val::I32(i32::from(r.is_null())));
                Ok(())
            }
            Op::RefAsNonNull => {
                let (r, tag) = self.pop_tagged();
                if r.is_null() {
                    return Err(Trap::NullReference.into());
                }
                self.push_cell(r, tag);
                Ok(())
            }
            _ => Err(Error::msg(format!("not a ref op: {op:?}"))),
        }
    }
}
