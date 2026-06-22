//! Arithmetic helper methods (pop operands → compute → push) plus the float
//! NaN-canonicalization and `min`/`max`/`div` helpers shared by the dispatch.

// `min`/`max` compare for exact equality to resolve the ±0 case, per the spec.
#![allow(clippy::float_cmp)]

use super::Execution;
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

const CANON_F32: u32 = 0x7fc0_0000;
const CANON_F64: u64 = 0x7ff8_0000_0000_0000;

pub(super) fn canon_f32(r: f32, had_nan: bool) -> f32 {
    if r.is_nan() && !had_nan {
        f32::from_bits(CANON_F32)
    } else {
        r
    }
}

pub(super) fn canon_f64(r: f64, had_nan: bool) -> f64 {
    if r.is_nan() && !had_nan {
        f64::from_bits(CANON_F64)
    } else {
        r
    }
}

pub(super) fn f32_min(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::from_bits(CANON_F32)
    } else if a == b {
        f32::from_bits(a.to_bits() | b.to_bits()) // ±0: prefer -0
    } else if a < b {
        a
    } else {
        b
    }
}

pub(super) fn f32_max(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::from_bits(CANON_F32)
    } else if a == b {
        f32::from_bits(a.to_bits() & b.to_bits()) // ±0: prefer +0
    } else if a > b {
        a
    } else {
        b
    }
}

pub(super) fn f64_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::from_bits(CANON_F64)
    } else if a == b {
        f64::from_bits(a.to_bits() | b.to_bits())
    } else if a < b {
        a
    } else {
        b
    }
}

pub(super) fn f64_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::from_bits(CANON_F64)
    } else if a == b {
        f64::from_bits(a.to_bits() & b.to_bits())
    } else if a > b {
        a
    } else {
        b
    }
}

pub(super) fn nz<T: Default + PartialEq>(b: T) -> Result<()> {
    if b == T::default() {
        Err(Trap::IntegerDivisionByZero.into())
    } else {
        Ok(())
    }
}

pub(super) fn div_s_i32(a: i32, b: i32) -> Result<i32> {
    if b == 0 {
        Err(Trap::IntegerDivisionByZero.into())
    } else if a == i32::MIN && b == -1 {
        Err(Trap::IntegerOverflow.into())
    } else {
        Ok(a / b)
    }
}

pub(super) fn div_s_i64(a: i64, b: i64) -> Result<i64> {
    if b == 0 {
        Err(Trap::IntegerDivisionByZero.into())
    } else if a == i64::MIN && b == -1 {
        Err(Trap::IntegerOverflow.into())
    } else {
        Ok(a / b)
    }
}

impl Execution {
    pub(super) fn i32_binop(&mut self, f: impl Fn(i32, i32) -> i32) {
        let b = self.pop().unwrap_i32();
        let a = self.pop().unwrap_i32();
        self.push(Val::I32(f(a, b)));
    }

    pub(super) fn i32_unop(&mut self, f: impl Fn(i32) -> i32) {
        let a = self.pop().unwrap_i32();
        self.push(Val::I32(f(a)));
    }

    pub(super) fn i32_relop(&mut self, f: impl Fn(i32, i32) -> bool) {
        let b = self.pop().unwrap_i32();
        let a = self.pop().unwrap_i32();
        self.push(Val::I32(i32::from(f(a, b))));
    }

    pub(super) fn u32_binop(&mut self, f: impl Fn(u32, u32) -> u32) {
        let b = self.pop().unwrap_i32() as u32;
        let a = self.pop().unwrap_i32() as u32;
        self.push(Val::I32(f(a, b) as i32));
    }

    pub(super) fn u32_relop(&mut self, f: impl Fn(u32, u32) -> bool) {
        let b = self.pop().unwrap_i32() as u32;
        let a = self.pop().unwrap_i32() as u32;
        self.push(Val::I32(i32::from(f(a, b))));
    }

    pub(super) fn i32_try_binop(&mut self, f: impl Fn(i32, i32) -> Result<i32>) -> Result<()> {
        let b = self.pop().unwrap_i32();
        let a = self.pop().unwrap_i32();
        let r = f(a, b)?;
        self.push(Val::I32(r));
        Ok(())
    }

    pub(super) fn u32_try_binop(&mut self, f: impl Fn(u32, u32) -> Result<u32>) -> Result<()> {
        let b = self.pop().unwrap_i32() as u32;
        let a = self.pop().unwrap_i32() as u32;
        let r = f(a, b)?;
        self.push(Val::I32(r as i32));
        Ok(())
    }

    pub(super) fn i64_binop(&mut self, f: impl Fn(i64, i64) -> i64) {
        let b = self.pop().unwrap_i64();
        let a = self.pop().unwrap_i64();
        self.push(Val::I64(f(a, b)));
    }

    pub(super) fn i64_unop(&mut self, f: impl Fn(i64) -> i64) {
        let a = self.pop().unwrap_i64();
        self.push(Val::I64(f(a)));
    }

    pub(super) fn i64_relop(&mut self, f: impl Fn(i64, i64) -> bool) {
        let b = self.pop().unwrap_i64();
        let a = self.pop().unwrap_i64();
        self.push(Val::I32(i32::from(f(a, b))));
    }

    pub(super) fn u64_binop(&mut self, f: impl Fn(u64, u64) -> u64) {
        let b = self.pop().unwrap_i64() as u64;
        let a = self.pop().unwrap_i64() as u64;
        self.push(Val::I64(f(a, b) as i64));
    }

    pub(super) fn u64_relop(&mut self, f: impl Fn(u64, u64) -> bool) {
        let b = self.pop().unwrap_i64() as u64;
        let a = self.pop().unwrap_i64() as u64;
        self.push(Val::I32(i32::from(f(a, b))));
    }

    pub(super) fn i64_try_binop(&mut self, f: impl Fn(i64, i64) -> Result<i64>) -> Result<()> {
        let b = self.pop().unwrap_i64();
        let a = self.pop().unwrap_i64();
        let r = f(a, b)?;
        self.push(Val::I64(r));
        Ok(())
    }

    pub(super) fn u64_try_binop(&mut self, f: impl Fn(u64, u64) -> Result<u64>) -> Result<()> {
        let b = self.pop().unwrap_i64() as u64;
        let a = self.pop().unwrap_i64() as u64;
        let r = f(a, b)?;
        self.push(Val::I64(r as i64));
        Ok(())
    }

    pub(super) fn f32_arith(&mut self, f: impl Fn(f32, f32) -> f32) {
        let b = self.pop().unwrap_f32();
        let a = self.pop().unwrap_f32();
        let r = canon_f32(f(a, b), a.is_nan() || b.is_nan());
        self.push(Val::F32(r.to_bits()));
    }

    pub(super) fn f32_binop(&mut self, f: impl Fn(f32, f32) -> f32) {
        let b = self.pop().unwrap_f32();
        let a = self.pop().unwrap_f32();
        self.push(Val::F32(f(a, b).to_bits()));
    }

    pub(super) fn f32_unop(&mut self, f: impl Fn(f32) -> f32) {
        let a = self.pop().unwrap_f32();
        self.push(Val::F32(f(a).to_bits()));
    }

    pub(super) fn f32_unop_canon(&mut self, f: impl Fn(f32) -> f32) {
        let a = self.pop().unwrap_f32();
        self.push(Val::F32(canon_f32(f(a), a.is_nan()).to_bits()));
    }

    pub(super) fn f32_relop(&mut self, f: impl Fn(f32, f32) -> bool) {
        let b = self.pop().unwrap_f32();
        let a = self.pop().unwrap_f32();
        self.push(Val::I32(i32::from(f(a, b))));
    }

    pub(super) fn f64_arith(&mut self, f: impl Fn(f64, f64) -> f64) {
        let b = self.pop().unwrap_f64();
        let a = self.pop().unwrap_f64();
        let r = canon_f64(f(a, b), a.is_nan() || b.is_nan());
        self.push(Val::F64(r.to_bits()));
    }

    pub(super) fn f64_binop(&mut self, f: impl Fn(f64, f64) -> f64) {
        let b = self.pop().unwrap_f64();
        let a = self.pop().unwrap_f64();
        self.push(Val::F64(f(a, b).to_bits()));
    }

    pub(super) fn f64_unop(&mut self, f: impl Fn(f64) -> f64) {
        let a = self.pop().unwrap_f64();
        self.push(Val::F64(f(a).to_bits()));
    }

    pub(super) fn f64_unop_canon(&mut self, f: impl Fn(f64) -> f64) {
        let a = self.pop().unwrap_f64();
        self.push(Val::F64(canon_f64(f(a), a.is_nan()).to_bits()));
    }

    pub(super) fn f64_relop(&mut self, f: impl Fn(f64, f64) -> bool) {
        let b = self.pop().unwrap_f64();
        let a = self.pop().unwrap_f64();
        self.push(Val::I32(i32::from(f(a, b))));
    }
}
