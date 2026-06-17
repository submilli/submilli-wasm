//! `call_indirect`: table-element lookup, signature check, then a tail call.

use super::{resolve_func, CallReq, Execution, StepOutcome};
use crate::instance::Instance;
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::value::Ref;
use crate::Result;

impl Execution {
    pub(super) fn do_call_indirect(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        type_idx: u32,
        table: u32,
        return_ip: u32,
    ) -> Result<StepOutcome> {
        let idx = u64::from(self.pop_i32() as u32);
        let handle = inner.instance(instance).tables[table as usize];
        let f = match inner.table(handle).get(idx) {
            Some(Ref::Func(Some(f))) => f,
            Some(Ref::Func(None)) => return Err(Trap::IndirectCallToNull.into()),
            Some(_) => return Err(Trap::BadSignature.into()),
            None => return Err(Trap::TableOutOfBounds.into()),
        };

        let (def_inst, code) = resolve_func(inner, f);
        let expected = &inner.instance(instance).module.inner().types[type_idx as usize];
        let actual = &inner.instance(def_inst).module.inner().types[code.type_idx as usize];
        if expected != actual {
            return Err(Trap::BadSignature.into());
        }

        Ok(StepOutcome::DoCall(CallReq {
            return_ip,
            instance: def_inst,
            code,
        }))
    }
}
