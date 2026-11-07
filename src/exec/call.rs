//! `call_indirect`: table-element lookup, signature check, then a (wasm or host) call.

use super::outcome::Callee;
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
pub(super) fn resolve(inner: &StoreInner, f: Func) -> Result<ResolvedCall> {
    Ok(match inner.func(f) {
        FuncEntity::Wasm {
            instance,
            func_index,
        } => {
            let module = &inner.instance(*instance).module;
            let (index, func) =
                module
                    .defined(*func_index)
                    .ok_or(crate::error::InternalError::FrameStack(
                        "call target is not a defined function",
                    ))?;
            ResolvedCall::Wasm(Callee {
                instance: *instance,
                func_index: *func_index,
                index,
                key: module.key(),
                n_params: func.n_params,
                type_idx: func.type_idx,
            })
        }
        FuncEntity::Host { .. } => ResolvedCall::Host(f),
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { .. } => ResolvedCall::HostAsync(f),
    })
}

impl Execution {
    /// `call_indirect` / `return_call_indirect` (`tail`, #39): table lookup, signature check, then
    /// the (wasm or host) call outcome.
    #[inline]
    pub(super) fn do_call_indirect(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        (type_idx, table): (u32, u32),
        kind: CallKind,
        at: usize,
    ) -> Result<StepOutcome> {
        // `table` is a wasmparser-validated table index for this instance (#33 carve-out).
        #[allow(clippy::indexing_slicing)]
        let handle = inner.instance(instance).tables[table as usize];
        let idx = self.index_at(at, inner.table(handle).ty.is_64())?;
        let f = match inner.table(handle).get(idx) {
            Some(Ref::Func(Some(f))) => f,
            Some(Ref::Func(None)) => return Err(Trap::IndirectCallToNull.into()),
            Some(_) => return Err(Trap::BadSignature.into()),
            None => return Err(Trap::TableOutOfBounds.into()),
        };

        let expected_module = inner.instance(instance).module.clone();
        // Engine-canonical id of the expected type (cross-module, recursion-safe identity).
        let expected_id = expected_module.inner().canonical_type_id(type_idx);
        let resolved = resolve(inner, f)?;
        match &resolved {
            // The callee's type must be a subtype of the expected one (funcref subtyping).
            ResolvedCall::Wasm(callee) => {
                let actual_id = inner
                    .instance(callee.instance)
                    .module
                    .inner()
                    .canonical_type_id(callee.type_idx);
                if !inner.engine().is_subtype(actual_id, expected_id) {
                    return Err(Trap::BadSignature.into());
                }
            }
            // Host func types are interned too — compare canonical ids uniformly.
            ResolvedCall::Host(func) => host_sig_ok(inner, *func, expected_id)?,
            #[cfg(feature = "async")]
            ResolvedCall::HostAsync(func) => host_sig_ok(inner, *func, expected_id)?,
        }
        let args_at = args_below(inner, &resolved, at)?;
        Ok(call_outcome(inner, resolved, instance, kind, args_at))
    }

    /// `call_ref` / `return_call_ref` (`tail`, #39): pop a funcref operand and dispatch. Null traps;
    /// the signature is statically guaranteed (validation), so there is no runtime type check.
    #[inline]
    pub(super) fn do_call_ref(
        &mut self,
        inner: &StoreInner,
        instance: Instance,
        kind: CallKind,
        at: usize,
    ) -> Result<StepOutcome> {
        let f = match self.take_ref(at, RefKind::Func)?.to_ref() {
            Ref::Func(Some(f)) => f,
            Ref::Func(None) => return Err(Trap::NullReference.into()),
            _ => return Err(Trap::BadSignature.into()),
        };
        let resolved = resolve(inner, f)?;
        let args_at = args_below(inner, &resolved, at)?;
        Ok(call_outcome(inner, resolved, instance, kind, args_at))
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
    args_at: usize,
) -> StepOutcome {
    match resolved {
        ResolvedCall::Wasm(callee) => {
            let return_ip = match kind {
                CallKind::Tail => 0,
                CallKind::Nested(return_ip) => return_ip,
            };
            let req = CallReq {
                return_ip,
                args_at,
                callee,
            };
            match kind {
                CallKind::Tail => StepOutcome::TailCall(req),
                CallKind::Nested(_) => StepOutcome::Call(req),
            }
        }
        ResolvedCall::Host(func) => match kind {
            CallKind::Tail => StepOutcome::TailHostCall {
                func,
                instance,
                args_at,
                n_params: host_n_params(inner, func),
            },
            CallKind::Nested(return_ip) => StepOutcome::HostCall {
                func,
                instance,
                top: args_at.saturating_add(host_n_params(inner, func)),
                return_ip,
            },
        },
        #[cfg(feature = "async")]
        ResolvedCall::HostAsync(func) => match kind {
            CallKind::Tail => StepOutcome::TailHostAsyncCall {
                func,
                instance,
                args_at,
                n_params: host_n_params(inner, func),
            },
            CallKind::Nested(return_ip) => StepOutcome::HostAsyncCall {
                func,
                instance,
                top: args_at.saturating_add(host_n_params(inner, func)),
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

/// The first argument's absolute operand index for a callee whose table-index/funcref operand
/// sits at `operand_at` (the arguments sit just below it).
fn args_below(inner: &StoreInner, resolved: &ResolvedCall, operand_at: usize) -> Result<usize> {
    let n_params = match resolved {
        ResolvedCall::Wasm(callee) => callee.n_params as usize,
        ResolvedCall::Host(f) => host_n_params(inner, *f),
        #[cfg(feature = "async")]
        ResolvedCall::HostAsync(f) => host_n_params(inner, *f),
    };
    operand_at.checked_sub(n_params).ok_or_else(|| {
        crate::error::InternalError::OperandStack("call arguments below the frame").into()
    })
}

/// A host callee's parameter count, from its cached signature.
fn host_n_params(inner: &StoreInner, f: Func) -> usize {
    match inner.func(f) {
        FuncEntity::Host { sig, .. } => sig.params.len(),
        #[cfg(feature = "async")]
        FuncEntity::HostAsync { sig, .. } => sig.params.len(),
        FuncEntity::Wasm { .. } => 0,
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
