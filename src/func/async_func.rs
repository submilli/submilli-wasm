//! Async host-function constructors and async calls (`--features async`).
//!
//! Split out of `func/mod.rs` to keep that file under the size cap. These are
//! inherent-impl continuations of [`Func`]/[`TypedFunc`]; they reach the parent
//! module's private helpers (`Callee`, `resolve_callee`, `check_args`,
//! `default_results`) as a descendant module.

use std::sync::Arc;

use super::direct_call::{
    call_direct_host, finish_direct_host_call, preflight_direct_call, DirectCallBoundary,
};
use super::wasm_ty::valtypes_of;
use super::{check_call, check_host_call, default_results, validate_results};
use super::{Callee, Caller, Func, TypedFunc};
use super::{WasmParams, WasmResults, WasmRet};
use crate::func::into_async_func;
use crate::store::{AsContextMut, FuncEntity};
use crate::value::{FuncType, Val};
use crate::Result;

async fn call_direct_host_async<S: AsContextMut>(
    store: &mut S,
    host_index: u32,
    params: &[Val],
    ty: &crate::value::FuncType,
) -> Result<Vec<Val>> {
    preflight_direct_call(store.as_context_mut().inner_mut())?;
    let cb = store.as_context_mut().store_mut().async_host_funcs[host_index as usize].clone();
    let mut out = default_results(ty);
    let boundary = DirectCallBoundary::capture(store.as_context().inner());
    let token = store.as_context_mut().inner_mut().begin_async_call_cleanup(
        Some(boundary.roots_mark),
        boundary.pending,
        boundary.pending_generation,
        None,
    );
    let mut cancellation = crate::exec::guard::AsyncCancellation::new(token);
    let guarded = async {
        Box::into_pin(cb(
            Caller::new(store.as_context_mut(), None),
            params,
            &mut out,
        ))
        .await
    };
    let outcome = crate::exec::guard::CatchUnwind(Box::pin(guarded)).await;
    let cleanup = store
        .as_context_mut()
        .inner_mut()
        .complete_async_call_cleanup(cancellation.token());
    cancellation.disarm();
    let result = match outcome {
        Ok(result) => result,
        Err(payload) => {
            boundary.restore_after_panic(store.as_context_mut().inner_mut());
            crate::exec::guard::reraise(payload);
        }
    };
    finish_async_cleanup(store.as_context_mut().inner_mut(), boundary, cleanup)?;
    let result_types: Vec<_> = ty.results().collect();
    finish_direct_host_call(
        store.as_context_mut().inner_mut(),
        boundary,
        out,
        &result_types,
        result,
    )
}

fn finish_async_cleanup(
    inner: &mut crate::store::StoreInner,
    boundary: DirectCallBoundary,
    cleanup: Result<()>,
) -> Result<()> {
    let Err(error) = cleanup else {
        return Ok(());
    };
    inner.gc_roots_truncate(boundary.roots_mark);
    inner.restore_pending_exception(boundary.pending, boundary.pending_generation);
    if inner.has_parked_execution() {
        if let Some(internal) = error.downcast_ref::<crate::error::InternalError>() {
            inner.latch_internal_error(*internal);
        }
    }
    Err(error)
}

impl Func {
    /// Creates an async host function with a dynamic signature. The closure returns a
    /// boxed future the async driver awaits; callable only via the async entry points.
    pub fn new_async<T, F>(mut store: impl AsContextMut<Data = T>, ty: FuncType, func: F) -> Func
    where
        F: for<'a> Fn(
                Caller<'a, T>,
                &'a [Val],
                &'a mut [Val],
            )
                -> std::boxed::Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>
            + Send
            + Sync
            + 'static,
        T: Send + 'static,
    {
        assert!(
            ty.engine().same(store.as_context().engine()),
            "function type belongs to a different engine"
        );
        let mut ctx = store.as_context_mut();
        let host_index = ctx.store_mut().push_async_host_func(Arc::new(func));
        ctx.inner_mut().alloc_func(FuncEntity::HostAsync {
            sig: crate::store::HostSig::new(&ty),
            ty,
            host_index,
        })
    }

    /// Creates an async host function from a typed Rust closure `Fn(Caller, P) -> Future<R>`.
    pub fn wrap_async<T, F, P, R>(mut store: impl AsContextMut<Data = T>, func: F) -> Func
    where
        F: for<'a> Fn(
                Caller<'a, T>,
                P,
            )
                -> std::boxed::Box<dyn std::future::Future<Output = R> + Send + 'a>
            + Send
            + Sync
            + 'static,
        P: WasmResults,
        R: WasmRet + 'static,
        T: Send + 'static,
    {
        let engine = store.as_context().engine().clone();
        let (ty, cb) = into_async_func(&engine, func);
        let mut ctx = store.as_context_mut();
        let host_index = ctx.store_mut().push_async_host_func(cb);
        ctx.inner_mut().alloc_func(FuncEntity::HostAsync {
            sig: crate::store::HostSig::new(&ty),
            ty,
            host_index,
        })
    }

    /// Async sibling of [`call`](Func::call): drives the call as a `Future`. Requires an
    /// async store; awaits async host callees.
    pub async fn call_async(
        &self,
        mut store: impl AsContextMut,
        params: &[Val],
        results: &mut [Val],
    ) -> Result<()> {
        if !store.as_context().engine().is_async() {
            return Err(crate::Error::msg(
                "cannot use `call_async` without `Config::async_support(true)`",
            ));
        }
        // Bind the callee to a local first: if the `store.as_context()` borrow were taken in the
        // `match` scrutinee it would live through the whole match body — including the `.await`s
        // below — making this future hold a shared `&Store` across a suspension point. That would
        // (spuriously) require `T: Sync`. Dropping the borrow here keeps the future `Send` for any
        // `T: Send`, matching wasmtime.
        let callee = self.resolve_callee(store.as_context().inner());
        let out = match callee {
            Callee::Wasm(instance, func_index) => {
                let n_results = results.len();
                let mut ctx = store.as_context_mut();
                call_wasm_async(ctx.store_mut(), instance, func_index, params, n_results).await?
            }
            Callee::Host(host_index) => {
                let ty = self.ty(&store);
                let result_tys = check_host_call(store.as_context().inner(), &ty, params, results)?;
                let out = call_direct_host(&mut store, host_index, params, &ty)?;
                validate_results(store.as_context_mut().inner_mut(), &out, &result_tys)?;
                out
            }
            Callee::HostAsync(host_index) => {
                let ty = self.ty(&store);
                let result_tys = check_host_call(store.as_context().inner(), &ty, params, results)?;
                let out = call_direct_host_async(&mut store, host_index, params, &ty).await?;
                validate_results(store.as_context_mut().inner_mut(), &out, &result_tys)?;
                out
            }
        };
        results.clone_from_slice(&out);
        Ok(())
    }
}

impl<Params, Results> TypedFunc<Params, Results>
where
    Params: WasmParams,
    Results: WasmResults,
{
    /// Async sibling of [`call`](TypedFunc::call). Requires an async store.
    pub async fn call_async(
        &self,
        mut store: impl AsContextMut,
        params: Params,
    ) -> Result<Results> {
        let mut args = Vec::new();
        params.into_vals(&mut args);
        let mut results = vec![Val::I32(0); valtypes_of::<Results>().len()];
        self.func
            .call_async(&mut store, &args, &mut results)
            .await?;
        Ok(Results::from_vals(&results))
    }
}

/// The wasm-callee half of [`Func::call_async`], mirroring the sync `call_wasm`: the signature
/// comes from the module's cache, and the cloned module `Arc` keeps it borrowable across the
/// await.
async fn call_wasm_async<T>(
    store: &mut crate::store::Store<T>,
    instance: crate::instance::Instance,
    func_index: u32,
    params: &[Val],
    n_results: usize,
) -> Result<Vec<Val>> {
    let code = store.inner.instance(instance).module.code(func_index);
    let module = Arc::clone(&code.module);
    let (param_tys, result_tys) = module.func_sig(func_index)?;
    check_call(&store.inner, params, n_results, param_tys, result_tys)?;
    let args = crate::extern_::coerce_args(&mut store.inner, params, param_tys)?;
    // `execute_async` validates each result against its type (and roots references) as it decodes.
    crate::exec::host_async::execute_async(store, instance, func_index, code, &args, result_tys)
        .await
}
