//! `IntoFunc` — converts a host closure into a [`Func`](crate::Func)'s dynamic
//! signature + a type-erased (over arity) host callback.
//!
//! Effectively sealed. Implemented for `Fn(A..) -> R` and the caller-aware
//! `Fn(Caller<'_, T>, A..) -> R`, arities 0..=16. The leading `Caller` is encoded
//! into the `Params` type so the two forms don't overlap (mirrors wasmtime).

use std::sync::Arc;

use crate::engine::Engine;
use crate::func::wasm_ty::{WasmRet, WasmTy};
use crate::func::Caller;
use crate::store::HostFunc;
use crate::value::{FuncType, ValType};

#[cfg(feature = "async")]
use crate::func::wasm_ty::WasmResults;
#[cfg(feature = "async")]
use crate::store::AsyncHostFunc;

/// Conversion of a Rust closure into a host function's signature + callback.
pub trait IntoFunc<T, Params, Results>: Send + Sync + 'static {
    fn into_func(self, engine: &Engine) -> (FuncType, HostFunc<T>);
}

/// Builds the `FuncType` from the argument `valtype`s and the return type.
fn make_ty<R: WasmRet>(engine: &Engine, params: Vec<ValType>) -> FuncType {
    let mut results = Vec::new();
    R::valtypes(&mut results);
    FuncType::new(engine, params, results)
}

/// Builds an [`AsyncHostFunc`] from a typed async closure `Fn(Caller, P) -> Future<R>`.
/// Mirrors [`IntoFunc::into_func`] for the async path (no arity macro: `P` is one tuple).
/// Shared by `Func::wrap_async` and `Linker::func_wrap_async`.
#[cfg(feature = "async")]
pub(crate) fn into_async_func<T, F, P, R>(engine: &Engine, func: F) -> (FuncType, AsyncHostFunc<T>)
where
    F: for<'a> Fn(
            Caller<'a, T>,
            P,
        ) -> std::boxed::Box<dyn std::future::Future<Output = R> + Send + 'a>
        + Send
        + Sync
        + 'static,
    P: WasmResults,
    R: WasmRet + 'static,
    T: Send + 'static,
{
    let mut params = Vec::new();
    P::valtypes(&mut params);
    let ty = make_ty::<R>(engine, params);
    let cb: AsyncHostFunc<T> = Arc::new(move |caller, vals, results| {
        let p = P::from_vals(vals);
        let user_fut = func(caller, p);
        std::boxed::Box::new(async move {
            let r = std::boxed::Box::into_pin(user_fut).await;
            r.into_results(results)
        })
    });
    (ty, cb)
}

macro_rules! impl_into_func {
    // Arity 0.
    (0) => {
        impl<T, F, R> IntoFunc<T, (), R> for F
        where
            F: Fn() -> R + Send + Sync + 'static,
            R: WasmRet,
            T: 'static,
        {
            fn into_func(self, engine: &Engine) -> (FuncType, HostFunc<T>) {
                let ty = make_ty::<R>(engine, Vec::new());
                let cb: HostFunc<T> = Arc::new(move |_caller, _params, results| {
                    self().into_results(results)
                });
                (ty, cb)
            }
        }

        impl<T, F, R> IntoFunc<T, (Caller<'_, T>,), R> for F
        where
            F: Fn(Caller<'_, T>) -> R + Send + Sync + 'static,
            R: WasmRet,
            T: 'static,
        {
            fn into_func(self, engine: &Engine) -> (FuncType, HostFunc<T>) {
                let ty = make_ty::<R>(engine, Vec::new());
                let cb: HostFunc<T> = Arc::new(move |caller, _params, results| {
                    self(caller).into_results(results)
                });
                (ty, cb)
            }
        }
    };
    // Arity 1 uses the bare parameter type (not a 1-tuple).
    (1 $arg:ident) => {
        impl<T, F, $arg, R> IntoFunc<T, $arg, R> for F
        where
            F: Fn($arg) -> R + Send + Sync + 'static,
            $arg: WasmTy,
            R: WasmRet,
            T: 'static,
        {
            fn into_func(self, engine: &Engine) -> (FuncType, HostFunc<T>) {
                let ty = make_ty::<R>(engine, vec![$arg::valtype()]);
                let cb: HostFunc<T> = Arc::new(move |_caller, params, results| {
                    self($arg::from_val(params[0])).into_results(results)
                });
                (ty, cb)
            }
        }

        impl<T, F, $arg, R> IntoFunc<T, (Caller<'_, T>, $arg), R> for F
        where
            F: Fn(Caller<'_, T>, $arg) -> R + Send + Sync + 'static,
            $arg: WasmTy,
            R: WasmRet,
            T: 'static,
        {
            fn into_func(self, engine: &Engine) -> (FuncType, HostFunc<T>) {
                let ty = make_ty::<R>(engine, vec![$arg::valtype()]);
                let cb: HostFunc<T> = Arc::new(move |caller, params, results| {
                    self(caller, $arg::from_val(params[0])).into_results(results)
                });
                (ty, cb)
            }
        }
    };
    ($n:tt $($args:ident)*) => {
        impl<T, F, $($args,)* R> IntoFunc<T, ($($args,)*), R> for F
        where
            F: Fn($($args),*) -> R + Send + Sync + 'static,
            $($args: WasmTy,)*
            R: WasmRet,
            T: 'static,
        {
            #[allow(non_snake_case)]
            fn into_func(self, engine: &Engine) -> (FuncType, HostFunc<T>) {
                let ty = make_ty::<R>(engine, vec![$($args::valtype()),*]);
                let cb: HostFunc<T> = Arc::new(move |_caller, params, results| {
                    let mut it = params.iter().copied();
                    $(let $args = $args::from_val(it.next().expect("arity validated"));)*
                    self($($args),*).into_results(results)
                });
                (ty, cb)
            }
        }

        impl<T, F, $($args,)* R> IntoFunc<T, (Caller<'_, T>, $($args,)*), R> for F
        where
            F: Fn(Caller<'_, T>, $($args),*) -> R + Send + Sync + 'static,
            $($args: WasmTy,)*
            R: WasmRet,
            T: 'static,
        {
            #[allow(non_snake_case)]
            fn into_func(self, engine: &Engine) -> (FuncType, HostFunc<T>) {
                let ty = make_ty::<R>(engine, vec![$($args::valtype()),*]);
                let cb: HostFunc<T> = Arc::new(move |caller, params, results| {
                    let mut it = params.iter().copied();
                    $(let $args = $args::from_val(it.next().expect("arity validated"));)*
                    self(caller, $($args),*).into_results(results)
                });
                (ty, cb)
            }
        }
    };
}
crate::for_each_arity!(impl_into_func);
