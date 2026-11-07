//! Reference value operators: `ref.null`, `ref.func`, `ref.is_null`, `ref.as_non_null`.
//! Pure operand-stack/reference manipulation — table access lives in [`super::table`].

use super::cell::Cell;
use super::Execution;
use crate::instance::Instance;
use crate::module::op::{Op, Slot};
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
        base: u32,
    ) -> Result<()> {
        let at = |s: Slot| super::step::slot(base, s);
        match op {
            Op::RefNull(heap, s) => self.set_val(at(*s), Val::null_for_heap(heap)),
            Op::RefFunc(f, s) => {
                // `f` is a wasmparser-validated function index for this instance (#33 carve-out).
                #[allow(clippy::indexing_slicing)]
                let func = inner.instance(instance).funcs[*f as usize];
                self.set_val(at(*s), Val::FuncRef(Some(func)))
            }
            Op::RefIsNull(s) => {
                let at = at(*s);
                let is_null = self.ref_at(at)?.0.is_null();
                self.set_scalar(at, Cell::from_i32(i32::from(is_null)))
            }
            Op::RefAsNonNull(s) => {
                let at = at(*s);
                if self.ref_at(at)?.0.is_null() {
                    return Err(Trap::NullReference.into());
                }
                Ok(()) // non-null: stays in place
            }
            _ => Err(Error::msg(format!("not a ref op: {op:?}"))),
        }
    }
}
