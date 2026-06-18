//! Reference value operators: `ref.null`, `ref.func`, `ref.is_null`, `ref.as_non_null`.
//! Pure operand-stack/reference manipulation — table access lives in [`super::table`].

use super::Execution;
use crate::instance::Instance;
use crate::module::op::Op;
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::value::{Val, ValType};
use crate::{Error, Result};

impl Execution {
    pub(super) fn exec_ref(
        &mut self,
        inner: &StoreInner,
        op: &Op,
        instance: Instance,
    ) -> Result<()> {
        match op {
            Op::RefNull(rt) => {
                self.push(Val::default_for(&ValType::Ref(rt.clone())));
                Ok(())
            }
            Op::RefFunc(f) => {
                let func = inner.instance(instance).funcs[*f as usize];
                self.push(Val::FuncRef(Some(func)));
                Ok(())
            }
            Op::RefIsNull => {
                let r = self.pop();
                self.push(Val::I32(i32::from(r.is_null_ref())));
                Ok(())
            }
            Op::RefAsNonNull => {
                let r = self.pop();
                if r.is_null_ref() {
                    return Err(Trap::NullReference.into());
                }
                self.push(r);
                Ok(())
            }
            _ => Err(Error::msg(format!("not a ref op: {op:?}"))),
        }
    }
}
