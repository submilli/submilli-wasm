//! Async host-function constructors and async calls (`--features async`).
//!
//! Split out of `func/mod.rs` to keep that file under the size cap. These are
//! inherent-impl continuations of [`Func`]/[`TypedFunc`]; they reach the parent
//! module's private helpers (`Callee`, `resolve_callee`, `check_args`,
//! `default_results`) as a descendant module.

use std::sync::Arc;

use super::wasm_ty::valtypes_of;
use super::{check_args, default_results, Callee, Caller, Func, TypedFunc};
use super::{WasmParams, WasmResults, WasmRet};
use crate::func::into_async_func;
use crate::store::{AsContextMut, FuncEntity};
use crate::value::{FuncType, Val};
use crate::Result;

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
        let mut ctx = store.as_context_mut();
        let host_index = ctx.store_mut().push_async_host_func(Arc::new(func));
        ctx.inner_mut()
            .alloc_func(FuncEntity::HostAsync { ty, host_index })
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
        ctx.inner_mut()
            .alloc_func(FuncEntity::HostAsync { ty, host_index })
    }

    /// Async sibling of [`call`](Func::call): drives the call as a `Future`. Requires an
    /// async store; awaits async host callees.
    #[allow(clippy::too_many_lines)] // three-arm callee dispatch; arms are short
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
        let ty = self.ty(&store);
        check_args(params, &ty)?;
        if results.len() != ty.results().len() {
            return Err(crate::Error::msg("wrong number of results"));
        }
        // Bind the callee to a local first: if the `store.as_context()` borrow were taken in the
        // `match` scrutinee it would live through the whole match body — including the `.await`s
        // below — making this future hold a shared `&Store` across a suspension point. That would
        // (spuriously) require `T: Sync`. Dropping the borrow here keeps the future `Send` for any
        // `T: Send`, matching wasmtime.
        let callee = self.resolve_callee(store.as_context().inner());
        let out = match callee {
            Callee::Wasm(instance, func_index) => {
                let code = store
                    .as_context()
                    .inner()
                    .instance(instance)
                    .module
                    .inner()
                    .compiled(func_index);
                let result_tys: Vec<crate::value::ValType> = ty.results().collect();
                let args = crate::extern_::coerce_args(
                    &mut store.as_context_mut().store_mut().inner,
                    params,
                    &ty,
                )?;
                crate::exec::host::execute_async(
                    store.as_context_mut().store_mut(),
                    instance,
                    func_index,
                    code,
                    args,
                    &result_tys,
                )
                .await?
            }
            Callee::Host(host_index) => {
                let cb = store.as_context_mut().store_mut().host_funcs[host_index as usize].clone();
                let mut out = default_results(&ty);
                cb(Caller::new(store.as_context_mut(), None), params, &mut out)?;
                out
            }
            Callee::HostAsync(host_index) => {
                let cb = store.as_context_mut().store_mut().async_host_funcs[host_index as usize]
                    .clone();
                let mut out = default_results(&ty);
                let fut = cb(Caller::new(store.as_context_mut(), None), params, &mut out);
                std::boxed::Box::into_pin(fut).await?;
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
