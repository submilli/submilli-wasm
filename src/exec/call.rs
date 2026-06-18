//! `call_indirect`: table-element lookup, signature check, then a (wasm or host) call.

use super::{CallReq, Execution, ResolvedCall, StepOutcome};
use crate::func::Func;
use crate::instance::Instance;
use crate::store::{FuncEntity, StoreInner};
use crate::trap::Trap;
use crate::value::{FuncType, Ref};
use crate::Result;

/// Resolves a function handle to a wasm body (defining instance + compiled code) or a
/// host func. Imported functions resolve transparently — the handle already points at
/// the defining instance's `FuncEntity`.
pub(super) fn resolve(inner: &StoreInner, f: Func) -> ResolvedCall {
    match inner.func(f) {
        FuncEntity::Wasm {
            instance,
            func_index,
        } => {
            let def_inst = *instance;
            let module = inner.instance(def_inst).module.clone();
            ResolvedCall::Wasm(def_inst, module.inner().compiled(*func_index))
        }
        FuncEntity::Host { .. } => ResolvedCall::Host(f),
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { .. } => ResolvedCall::HostAsync(f),
    }
}

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
            #[cfg(feature = "async")]
            ResolvedCall::HostAsync(func) => {
                if expected != host_ty(inner, func) {
                    return Err(Trap::BadSignature.into());
                }
                Ok(StepOutcome::DoHostAsyncCall {
                    func,
                    instance,
                    return_ip,
                })
            }
        }
    }

    /// `call_ref`: pop a funcref operand and dispatch to it. Null traps; the signature
    /// is statically guaranteed (validation), so there is no runtime type check.
    pub(super) fn do_call_ref(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        return_ip: u32,
    ) -> Result<StepOutcome> {
        let f = match self.pop().to_ref() {
            Ref::Func(Some(f)) => f,
            Ref::Func(None) => return Err(Trap::NullReference.into()),
            _ => return Err(Trap::BadSignature.into()),
        };
        Ok(match resolve(inner, f) {
            ResolvedCall::Wasm(def_inst, code) => StepOutcome::DoCall(CallReq {
                return_ip,
                instance: def_inst,
                code,
            }),
            ResolvedCall::Host(func) => StepOutcome::DoHostCall {
                func,
                instance,
                return_ip,
            },
            #[cfg(feature = "async")]
            ResolvedCall::HostAsync(func) => StepOutcome::DoHostAsyncCall {
                func,
                instance,
                return_ip,
            },
        })
    }
}

/// The dynamic signature of a host function handle (sync or async).
fn host_ty(inner: &StoreInner, f: crate::func::Func) -> &FuncType {
    match inner.func(f) {
        FuncEntity::Host { ty, .. } => ty,
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { ty, .. } => ty,
        FuncEntity::Wasm { .. } => unreachable!("resolve returned a host func"),
    }
}
