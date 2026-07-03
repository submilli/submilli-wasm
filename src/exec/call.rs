//! `call_indirect`: table-element lookup, signature check, then a (wasm or host) call.

use super::{CallReq, Execution, ResolvedCall, StepOutcome};
use crate::canon::{CanonicalTypeId, RefKind};
use crate::func::Func;
use crate::instance::Instance;
use crate::store::{FuncEntity, StoreInner};
use crate::trap::Trap;
use crate::value::{FuncType, Ref};
use crate::Result;

/// Disposition of a call site: a normal nested call (carrying the caller's resume ip) or a tail
/// call (#39) that replaces the current frame.
#[derive(Clone, Copy)]
pub(super) enum CallKind {
    Nested(u32),
    Tail,
}

/// Resolves a function handle to a wasm body (defining instance + compiled code) or a
/// host func. Imported functions resolve transparently — the handle already points at
/// the defining instance's `FuncEntity`.
#[inline]
pub(super) fn resolve(inner: &StoreInner, f: Func) -> ResolvedCall {
    match inner.func(f) {
        FuncEntity::Wasm {
            instance,
            func_index,
        } => {
            let def_inst = *instance;
            let module = inner.instance(def_inst).module.clone();
            ResolvedCall::Wasm(def_inst, *func_index, module.code(*func_index))
        }
        FuncEntity::Host { .. } => ResolvedCall::Host(f),
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { .. } => ResolvedCall::HostAsync(f),
    }
}

impl Execution {
    /// `call_indirect` / `return_call_indirect` (`tail`, #39): table lookup, signature check, then
    /// the (wasm or host) call outcome.
    #[inline]
    pub(super) fn do_call_indirect(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        type_idx: u32,
        table: u32,
        kind: CallKind,
    ) -> Result<StepOutcome> {
        // `table` is a wasmparser-validated table index for this instance (#33 carve-out).
        #[allow(clippy::indexing_slicing)]
        let handle = inner.instance(instance).tables[table as usize];
        let idx = self.pop_index(inner.table(handle).ty.is_64());
        let f = match inner.table(handle).get(idx) {
            Some(Ref::Func(Some(f))) => f,
            Some(Ref::Func(None)) => return Err(Trap::IndirectCallToNull.into()),
            Some(_) => return Err(Trap::BadSignature.into()),
            None => return Err(Trap::TableOutOfBounds.into()),
        };

        let expected_module = inner.instance(instance).module.clone();
        // Engine-canonical id of the expected type (cross-module, recursion-safe identity).
        let expected_id = expected_module.inner().canonical_type_id(type_idx);
        let resolved = resolve(inner, f);
        match &resolved {
            // The callee's type must be a subtype of the expected one (funcref subtyping).
            ResolvedCall::Wasm(def_inst, _, code) => {
                let actual_id = inner
                    .instance(*def_inst)
                    .module
                    .inner()
                    .canonical_type_id(code.type_idx());
                if !inner.engine().is_subtype(actual_id, expected_id) {
                    return Err(Trap::BadSignature.into());
                }
            }
            // Host func types are interned too — compare canonical ids uniformly.
            ResolvedCall::Host(func) => host_sig_ok(inner, *func, expected_id)?,
            #[cfg(feature = "async")]
            ResolvedCall::HostAsync(func) => host_sig_ok(inner, *func, expected_id)?,
        }
        Ok(call_outcome(inner, resolved, instance, kind))
    }

    /// `call_ref` / `return_call_ref` (`tail`, #39): pop a funcref operand and dispatch. Null traps;
    /// the signature is statically guaranteed (validation), so there is no runtime type check.
    #[inline]
    pub(super) fn do_call_ref(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        kind: CallKind,
    ) -> Result<StepOutcome> {
        let f = match self.pop_ref(RefKind::Func).to_ref() {
            Ref::Func(Some(f)) => f,
            Ref::Func(None) => return Err(Trap::NullReference.into()),
            _ => return Err(Trap::BadSignature.into()),
        };
        Ok(call_outcome(inner, resolve(inner, f), instance, kind))
    }
}

/// Builds the step outcome from a resolved callee. `tail` (#39) replaces the current frame instead
/// of nesting; for a tail host call it carries `n_params` so the run loop can reposition the args.
#[inline]
pub(super) fn call_outcome(
    inner: &StoreInner,
    resolved: ResolvedCall,
    instance: Instance,
    kind: CallKind,
) -> StepOutcome {
    let n_params = |func| host_ty(inner, func).params().len() as u32;
    match resolved {
        ResolvedCall::Wasm(def_inst, func_index, code) => {
            let req = |return_ip| CallReq {
                return_ip,
                instance: def_inst,
                func_index,
                code: code.clone(),
            };
            match kind {
                CallKind::Tail => StepOutcome::DoTailCall(req(0)),
                CallKind::Nested(return_ip) => StepOutcome::DoCall(req(return_ip)),
            }
        }
        ResolvedCall::Host(func) => match kind {
            CallKind::Tail => StepOutcome::DoTailHostCall {
                func,
                instance,
                n_params: n_params(func),
            },
            CallKind::Nested(return_ip) => StepOutcome::DoHostCall {
                func,
                instance,
                return_ip,
            },
        },
        #[cfg(feature = "async")]
        ResolvedCall::HostAsync(func) => match kind {
            CallKind::Tail => StepOutcome::DoTailHostAsyncCall {
                func,
                instance,
                n_params: n_params(func),
            },
            CallKind::Nested(return_ip) => StepOutcome::DoHostAsyncCall {
                func,
                instance,
                return_ip,
            },
        },
    }
}

/// Traps unless the host callee's (interned) type is a subtype of the `call_indirect` site's
/// expected canonical type id.
fn host_sig_ok(inner: &StoreInner, func: Func, expected_id: CanonicalTypeId) -> Result<()> {
    if inner
        .engine()
        .is_subtype(host_ty(inner, func).canonical_id(), expected_id)
    {
        Ok(())
    } else {
        Err(Trap::BadSignature.into())
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
