//! The runtime value enum (`Val`) and reference value enum (`Ref`).
//!
//! `Val` is both the public, `wasmtime`-compatible value type and the value the
//! interpreter operates on (there is no separate internal value type).

use crate::canon::{AggKind, IrHeap, IrVal};
use crate::func::Func;
use crate::value::gc_ref::{AnyRef, ExnRef, ExternRef, Rooted};
use crate::value::types::{HeapType, ValType};

/// A 128-bit vector value.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct V128(u128);

impl V128 {
    pub fn as_u128(self) -> u128 {
        self.0
    }
}

impl From<u128> for V128 {
    fn from(value: u128) -> Self {
        V128(value)
    }
}

impl From<V128> for u128 {
    fn from(value: V128) -> Self {
        value.0
    }
}

/// A WebAssembly value. `F32`/`F64` hold raw IEEE-754 bits, matching `wasmtime::Val`.
#[derive(Copy, Clone, Debug)]
pub enum Val {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    V128(V128),
    FuncRef(Option<Func>),
    ExternRef(Option<Rooted<ExternRef>>),
    AnyRef(Option<Rooted<AnyRef>>),
    ExnRef(Option<Rooted<ExnRef>>),
}

impl Val {
    /// A null `funcref` (`(ref null func)`).
    pub fn null_func_ref() -> Val {
        Val::FuncRef(None)
    }

    /// A null `externref` (`(ref null extern)`).
    pub fn null_extern_ref() -> Val {
        Val::ExternRef(None)
    }

    /// A null `anyref` (`(ref null any)`).
    pub fn null_any_ref() -> Val {
        Val::AnyRef(None)
    }

    /// A null `exnref` (`(ref null exn)`).
    pub fn null_exn_ref() -> Val {
        Val::ExnRef(None)
    }

    /// Returns the `i32` if this is one, else `None`.
    pub fn i32(&self) -> Option<i32> {
        match self {
            Val::I32(x) => Some(*x),
            _ => None,
        }
    }

    /// Returns the `i32`, panicking if this is not one.
    pub fn unwrap_i32(&self) -> i32 {
        self.i32().expect("expected i32")
    }

    /// Returns the `i64` if this is one, else `None`.
    pub fn i64(&self) -> Option<i64> {
        match self {
            Val::I64(x) => Some(*x),
            _ => None,
        }
    }

    /// Returns the `i64`, panicking if this is not one.
    pub fn unwrap_i64(&self) -> i64 {
        self.i64().expect("expected i64")
    }

    /// Returns the `f32` (decoded from raw bits) if this is one, else `None`.
    pub fn f32(&self) -> Option<f32> {
        match self {
            Val::F32(bits) => Some(f32::from_bits(*bits)),
            _ => None,
        }
    }

    /// Returns the `f32`, panicking if this is not one.
    pub fn unwrap_f32(&self) -> f32 {
        self.f32().expect("expected f32")
    }

    /// Returns the `f64` (decoded from raw bits) if this is one, else `None`.
    pub fn f64(&self) -> Option<f64> {
        match self {
            Val::F64(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }

    /// Returns the `f64`, panicking if this is not one.
    pub fn unwrap_f64(&self) -> f64 {
        self.f64().expect("expected f64")
    }

    /// Returns the `v128` if this is one, else `None`.
    pub fn v128(&self) -> Option<V128> {
        match self {
            Val::V128(x) => Some(*x),
            _ => None,
        }
    }

    /// Returns the `v128`, panicking if this is not one.
    pub fn unwrap_v128(&self) -> V128 {
        self.v128().expect("expected v128")
    }

    /// Whether this is a *null* reference (any ref kind). Non-refs return false.
    pub(crate) fn is_null_ref(&self) -> bool {
        matches!(
            self,
            Val::FuncRef(None) | Val::ExternRef(None) | Val::AnyRef(None) | Val::ExnRef(None)
        )
    }

    /// Lifts a table/element [`Ref`] to the corresponding `Val`.
    pub(crate) fn from_ref(r: Ref) -> Val {
        match r {
            Ref::Func(f) => Val::FuncRef(f),
            Ref::Extern(e) => Val::ExternRef(e),
            Ref::Any(a) => Val::AnyRef(a),
            Ref::Exn(x) => Val::ExnRef(x),
        }
    }

    /// Lowers a reference `Val` to a [`Ref`]. The operand is a reference by validation.
    pub(crate) fn to_ref(self) -> Ref {
        match self {
            Val::FuncRef(f) => Ref::Func(f),
            Val::ExternRef(e) => Ref::Extern(e),
            Val::AnyRef(a) => Ref::Any(a),
            Val::ExnRef(x) => Ref::Exn(x),
            _ => unreachable!("operand validated as a reference"),
        }
    }

    /// The correctly-typed zero value for an IR local type, used to default-initialize locals.
    ///
    /// Reference types default to null. A non-nullable reference local (function-references)
    /// is not defaultable in wasm, but the validator's local-init tracking guarantees it is
    /// set before any read — so the null placeholder returned here is never observed.
    pub(crate) fn default_for(ty: &IrVal) -> Val {
        match ty {
            IrVal::I32 => Val::I32(0),
            IrVal::I64 => Val::I64(0),
            IrVal::F32 => Val::F32(0),
            IrVal::F64 => Val::F64(0),
            IrVal::V128 => Val::V128(V128::from(0)),
            IrVal::Ref { heap, .. } => Val::null_for_heap(heap),
        }
    }

    /// The null reference value for an IR heap type (by hierarchy).
    pub(crate) fn null_for_heap(heap: &IrHeap) -> Val {
        match heap {
            IrHeap::Func | IrHeap::NoFunc | IrHeap::Concrete(_, AggKind::Func) => {
                Val::FuncRef(None)
            }
            IrHeap::Extern | IrHeap::NoExtern => Val::ExternRef(None),
            IrHeap::Exn | IrHeap::NoExn => Val::ExnRef(None),
            _ => Val::AnyRef(None),
        }
    }

    /// The default zero value for a public (boundary) value type — used to pre-initialize host
    /// call result slots before the host writes them.
    pub(crate) fn default_for_valtype(ty: &ValType) -> Val {
        match ty {
            ValType::I32 => Val::I32(0),
            ValType::I64 => Val::I64(0),
            ValType::F32 => Val::F32(0),
            ValType::F64 => Val::F64(0),
            ValType::V128 => Val::V128(V128::from(0)),
            ValType::Ref(rt) => match rt.heap_type() {
                HeapType::Func | HeapType::NoFunc | HeapType::ConcreteFunc(_) => Val::FuncRef(None),
                HeapType::Extern | HeapType::NoExtern => Val::ExternRef(None),
                HeapType::Exn | HeapType::NoExn => Val::ExnRef(None),
                _ => Val::AnyRef(None),
            },
        }
    }
}

/// A reference value (table element / `ref.*`).
#[derive(Clone, Debug)]
pub enum Ref {
    Func(Option<Func>),
    Extern(Option<Rooted<ExternRef>>),
    Any(Option<Rooted<AnyRef>>),
    Exn(Option<Rooted<ExnRef>>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_bits_round_trip_preserves_nan() {
        let nan_bits = 0x7fc0_1234_u32;
        let v = Val::F32(nan_bits);
        assert!(v.unwrap_f32().is_nan());
        assert_eq!(v.unwrap_f32().to_bits(), nan_bits);
    }

    #[test]
    fn float_accessors_decode_bits() {
        assert_eq!(
            Val::F32(1.5_f32.to_bits()).unwrap_f32().to_bits(),
            1.5_f32.to_bits()
        );
        assert_eq!(
            Val::F64(2.5_f64.to_bits()).unwrap_f64().to_bits(),
            2.5_f64.to_bits()
        );
        assert_eq!(Val::I32(-7).unwrap_i32(), -7);
        assert_eq!(Val::I64(9).unwrap_i64(), 9);
        assert!(Val::I32(0).f64().is_none());
    }

    #[test]
    fn default_for_numeric_types() {
        use crate::canon::IrVal;
        assert_eq!(Val::default_for(&IrVal::I32).unwrap_i32(), 0);
        assert_eq!(Val::default_for(&IrVal::I64).unwrap_i64(), 0);
        assert_eq!(Val::default_for(&IrVal::F32).unwrap_f32().to_bits(), 0);
        assert_eq!(Val::default_for(&IrVal::F64).unwrap_f64().to_bits(), 0);
    }
}
