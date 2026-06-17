//! `IntoFunc` — converts a host closure into a [`Func`](crate::Func).
//!
//! Effectively sealed. Implemented for `Fn(A..) -> R` and the caller-aware
//! `Fn(Caller<'_, T>, A..) -> R`, arities 0..=16. The leading `Caller` is encoded
//! into the `Params` type so the two forms don't overlap (mirrors wasmtime).

use crate::func::wasm_ty::{WasmRet, WasmTy};
use crate::func::Caller;

/// Conversion of a Rust closure into a host [`Func`](crate::Func).
pub trait IntoFunc<T, Params, Results>: Send + Sync + 'static {}

macro_rules! impl_into_func {
    // Arity 1 uses the bare parameter type (not a 1-tuple).
    (1 $arg:ident) => {
        impl<T, F, $arg, R> IntoFunc<T, $arg, R> for F
        where
            F: Fn($arg) -> R + Send + Sync + 'static,
            $arg: WasmTy,
            R: WasmRet,
            T: 'static,
        {
        }

        impl<T, F, $arg, R> IntoFunc<T, (Caller<'_, T>, $arg), R> for F
        where
            F: Fn(Caller<'_, T>, $arg) -> R + Send + Sync + 'static,
            $arg: WasmTy,
            R: WasmRet,
            T: 'static,
        {
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
        }

        impl<T, F, $($args,)* R> IntoFunc<T, (Caller<'_, T>, $($args,)*), R> for F
        where
            F: Fn(Caller<'_, T>, $($args),*) -> R + Send + Sync + 'static,
            $($args: WasmTy,)*
            R: WasmRet,
            T: 'static,
        {
        }
    };
}
crate::for_each_arity!(impl_into_func);
