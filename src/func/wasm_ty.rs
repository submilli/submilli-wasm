//! Typed-function value conversion traits.
//!
//! These are the bounds an embedder writes (`P: WasmParams`, …) and the internal
//! lower/lift machinery the typed API (`Func::wrap`/`typed`, `TypedFunc::call`)
//! uses to move between Rust values and [`Val`]. Effectively sealed (do not
//! implement them). Impls cover scalar types, the bare single-value form, and
//! tuples (arities 0..=16). The `from_*` conversions assume the signature was
//! already validated (by `Func::typed`/`Func::wrap`), so they trust the variant.

use crate::func::Func;
use crate::value::{Val, ValType, V128};
use crate::Result;

/// A type usable as a single wasm value in the typed API.
pub trait WasmTy: Send + 'static {
    fn valtype() -> ValType;
    fn into_val(self) -> Val;
    fn from_val(v: Val) -> Self;
}

/// A type usable as the parameter list of a [`TypedFunc`](crate::TypedFunc).
pub trait WasmParams: Send + 'static {
    fn valtypes(out: &mut Vec<ValType>);
    fn into_vals(self, out: &mut Vec<Val>);
}

/// A type usable as the result list of a [`TypedFunc`](crate::TypedFunc).
pub trait WasmResults: WasmParams {
    fn from_vals(vals: &[Val]) -> Self;
}

/// A type returnable from a host function passed to `Func::wrap`.
pub trait WasmRet {
    fn valtypes(out: &mut Vec<ValType>);
    /// Writes the results into `out` (pre-sized to the result arity). An `Err`
    /// (e.g. from a `Result<R>` return) surfaces as a trap.
    fn into_results(self, out: &mut [Val]) -> Result<()>;
}

/// The expected `valtypes` of a params/results type, as a `Vec`.
pub(crate) fn valtypes_of<P: WasmParams>() -> Vec<ValType> {
    let mut v = Vec::new();
    P::valtypes(&mut v);
    v
}

macro_rules! impl_wasm_ty {
    ($($t:ty => $vt:expr, $into:expr, $from:expr;)*) => {$(
        impl WasmTy for $t {
            fn valtype() -> ValType { $vt }
            fn into_val(self) -> Val { $into(self) }
            fn from_val(v: Val) -> Self { $from(v) }
        }
    )*};
}
impl_wasm_ty! {
    i32 => ValType::I32, Val::I32, |v: Val| v.unwrap_i32();
    u32 => ValType::I32, (|x: u32| Val::I32(x as i32)), (|v: Val| v.unwrap_i32() as u32);
    i64 => ValType::I64, Val::I64, |v: Val| v.unwrap_i64();
    u64 => ValType::I64, (|x: u64| Val::I64(x as i64)), (|v: Val| v.unwrap_i64() as u64);
    f32 => ValType::F32, (|x: f32| Val::F32(x.to_bits())), (|v: Val| v.unwrap_f32());
    f64 => ValType::F64, (|x: f64| Val::F64(x.to_bits())), (|v: Val| v.unwrap_f64());
    V128 => ValType::V128, Val::V128, |v: Val| v.unwrap_v128();
}

impl WasmTy for Option<Func> {
    fn valtype() -> ValType {
        ValType::Ref(crate::value::RefType::new(
            true,
            crate::value::HeapType::Func,
        ))
    }
    fn into_val(self) -> Val {
        Val::FuncRef(self)
    }
    fn from_val(v: Val) -> Self {
        match v {
            Val::FuncRef(f) => f,
            _ => panic!("expected funcref"),
        }
    }
}

// Single bare value (the arity-1 / single-result form).
impl<T: WasmTy> WasmParams for T {
    fn valtypes(out: &mut Vec<ValType>) {
        out.push(T::valtype());
    }
    fn into_vals(self, out: &mut Vec<Val>) {
        out.push(self.into_val());
    }
}
impl<T: WasmTy> WasmResults for T {
    fn from_vals(vals: &[Val]) -> Self {
        T::from_val(vals[0])
    }
}
impl<T: WasmTy> WasmRet for T {
    fn valtypes(out: &mut Vec<ValType>) {
        out.push(T::valtype());
    }
    fn into_results(self, out: &mut [Val]) -> Result<()> {
        out[0] = self.into_val();
        Ok(())
    }
}

// A host function may return `Result<R>` to signal a trap.
impl<T: WasmRet> WasmRet for Result<T> {
    fn valtypes(out: &mut Vec<ValType>) {
        T::valtypes(out);
    }
    fn into_results(self, out: &mut [Val]) -> Result<()> {
        self?.into_results(out)
    }
}

macro_rules! impl_wasm_tuple {
    ($n:tt $($t:ident)*) => {
        impl<$($t: WasmTy,)*> WasmParams for ($($t,)*) {
            fn valtypes(out: &mut Vec<ValType>) {
                $(out.push($t::valtype());)*
            }
            #[allow(non_snake_case)]
            fn into_vals(self, out: &mut Vec<Val>) {
                let ($($t,)*) = self;
                $(out.push($t.into_val());)*
            }
        }
        impl<$($t: WasmTy,)*> WasmResults for ($($t,)*) {
            #[allow(unused_mut, unused_variables, clippy::unused_unit)]
            fn from_vals(vals: &[Val]) -> Self {
                let mut it = vals.iter().copied();
                ($($t::from_val(it.next().expect("result arity validated")),)*)
            }
        }
        impl<$($t: WasmTy,)*> WasmRet for ($($t,)*) {
            fn valtypes(out: &mut Vec<ValType>) {
                $(out.push($t::valtype());)*
            }
            #[allow(non_snake_case, unused_mut, unused_variables)]
            fn into_results(self, out: &mut [Val]) -> Result<()> {
                let ($($t,)*) = self;
                let mut i = 0;
                $(out[i] = $t.into_val(); i += 1;)*
                let _ = i;
                Ok(())
            }
        }
    };
}
crate::for_each_arity!(impl_wasm_tuple);
