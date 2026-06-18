//! `call_indirect`: table-element lookup, signature check, then a (wasm or host) call.

use super::{resolve, CallReq, Execution, ResolvedCall, StepOutcome};
use crate::instance::Instance;
use crate::store::{FuncEntity, StoreInner};
use crate::trap::Trap;
use crate::value::{FuncType, Ref};
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

        let expected = &inner.instance(instance).module.inner().types[type_idx as usize];
        match resolve(inner, f) {
            ResolvedCall::Wasm(def_inst, code) => {
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
            ResolvedCall::Host(func) => {
                if expected != host_ty(inner, func) {
                    return Err(Trap::BadSignature.into());
                }
                Ok(StepOutcome::DoHostCall {
                    func,
                    instance,
                    return_ip,
                })
            }
        }
    }
}

/// The dynamic signature of a host function handle.
fn host_ty(inner: &StoreInner, f: crate::func::Func) -> &FuncType {
    match inner.func(f) {
        FuncEntity::Host { ty, .. } => ty,
        FuncEntity::Wasm { .. } => unreachable!("resolve returned Host"),
    }
}
