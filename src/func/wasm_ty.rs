//! Marker traits for typed-function parameters/results.
//!
//! These are the bounds an embedder writes (`P: WasmParams`, …). They are
//! effectively sealed (do not implement them); the internal machinery used by
//! the interpreter lives elsewhere. Impls cover scalar types, the bare
//! single-value form, and tuples (arities 0..=16).

use crate::func::Func;
use crate::value::V128;

/// A type usable as a single wasm value in the typed API.
pub trait WasmTy: Send + 'static {}

/// A type usable as the parameter list of a [`TypedFunc`](crate::TypedFunc).
pub trait WasmParams: Send + 'static {}

/// A type usable as the result list of a [`TypedFunc`](crate::TypedFunc).
pub trait WasmResults: WasmParams {}

/// A type returnable from a host function passed to `Func::wrap`.
pub trait WasmRet {}

macro_rules! impl_wasm_ty {
    ($($t:ty),* $(,)?) => {$(
        impl WasmTy for $t {}
    )*};
}
impl_wasm_ty!(i32, u32, i64, u64, f32, f64, V128, Option<Func>);

// Single bare value (the arity-1 / single-result form).
impl<T: WasmTy> WasmParams for T {}
impl<T: WasmTy> WasmResults for T {}
impl<T: WasmTy> WasmRet for T {}

// A host function may return `Result<R>` to signal a trap.
impl<T: WasmRet> WasmRet for crate::Result<T> {}

macro_rules! impl_wasm_tuple {
    ($n:tt $($t:ident)*) => {
        impl<$($t: WasmTy,)*> WasmParams for ($($t,)*) {}
        impl<$($t: WasmTy,)*> WasmResults for ($($t,)*) {}
        impl<$($t: WasmTy,)*> WasmRet for ($($t,)*) {}
    };
}
crate::for_each_arity!(impl_wasm_tuple);
