//! Numeric conversions: the boundary-sensitive float↔int ops.
//!
//! Trapping truncation checks NaN and range explicitly; saturating truncation
//! relies on Rust's `as` cast (which saturates: NaN→0, out-of-range→clamp).
//! Range bounds use exact powers of two (representable in both f32 and f64).

// int→float conversions lose precision by definition; the explicit `>= lo && < hi`
// bound reads clearer here than `Range::contains`.
#![allow(clippy::cast_precision_loss, clippy::manual_range_contains)]

use crate::trap::Trap;
use crate::{Error, Result};

fn bad_conv() -> Error {
    Trap::BadConversionToInteger.into()
}

fn overflow() -> Error {
    Trap::IntegerOverflow.into()
}

pub(super) fn i32_wrap_i64(x: i64) -> i32 {
    x as i32
}

pub(super) fn i64_extend_i32_s(x: i32) -> i64 {
    i64::from(x)
}

pub(super) fn i64_extend_i32_u(x: i32) -> i64 {
    i64::from(x as u32)
}

// --- trapping truncation ---

pub(super) fn i32_trunc_f32_s(x: f32) -> Result<i32> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= -2_147_483_648.0 && t < 2_147_483_648.0 {
        Ok(t as i32)
    } else {
        Err(overflow())
    }
}

pub(super) fn i32_trunc_f32_u(x: f32) -> Result<i32> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= 0.0 && t < 4_294_967_296.0 {
        Ok(t as u32 as i32)
    } else {
        Err(overflow())
    }
}

pub(super) fn i32_trunc_f64_s(x: f64) -> Result<i32> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= -2_147_483_648.0 && t < 2_147_483_648.0 {
        Ok(t as i32)
    } else {
        Err(overflow())
    }
}

pub(super) fn i32_trunc_f64_u(x: f64) -> Result<i32> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= 0.0 && t < 4_294_967_296.0 {
        Ok(t as u32 as i32)
    } else {
        Err(overflow())
    }
}

pub(super) fn i64_trunc_f32_s(x: f32) -> Result<i64> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= -9_223_372_036_854_775_808.0 && t < 9_223_372_036_854_775_808.0 {
        Ok(t as i64)
    } else {
        Err(overflow())
    }
}

pub(super) fn i64_trunc_f32_u(x: f32) -> Result<i64> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= 0.0 && t < 18_446_744_073_709_551_616.0 {
        Ok(t as u64 as i64)
    } else {
        Err(overflow())
    }
}

pub(super) fn i64_trunc_f64_s(x: f64) -> Result<i64> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= -9_223_372_036_854_775_808.0 && t < 9_223_372_036_854_775_808.0 {
        Ok(t as i64)
    } else {
        Err(overflow())
    }
}

pub(super) fn i64_trunc_f64_u(x: f64) -> Result<i64> {
    if x.is_nan() {
        return Err(bad_conv());
    }
    let t = x.trunc();
    if t >= 0.0 && t < 18_446_744_073_709_551_616.0 {
        Ok(t as u64 as i64)
    } else {
        Err(overflow())
    }
}

// --- saturating truncation (Rust `as` saturates) ---

pub(super) fn i32_trunc_sat_f32_s(x: f32) -> i32 {
    x as i32
}
pub(super) fn i32_trunc_sat_f32_u(x: f32) -> i32 {
    x as u32 as i32
}
pub(super) fn i32_trunc_sat_f64_s(x: f64) -> i32 {
    x as i32
}
pub(super) fn i32_trunc_sat_f64_u(x: f64) -> i32 {
    x as u32 as i32
}
pub(super) fn i64_trunc_sat_f32_s(x: f32) -> i64 {
    x as i64
}
pub(super) fn i64_trunc_sat_f32_u(x: f32) -> i64 {
    x as u64 as i64
}
pub(super) fn i64_trunc_sat_f64_s(x: f64) -> i64 {
    x as i64
}
pub(super) fn i64_trunc_sat_f64_u(x: f64) -> i64 {
    x as u64 as i64
}

// --- int→float conversions and demote/promote ---

pub(super) fn f32_convert_i32_s(x: i32) -> f32 {
    x as f32
}
pub(super) fn f32_convert_i32_u(x: i32) -> f32 {
    (x as u32) as f32
}
pub(super) fn f32_convert_i64_s(x: i64) -> f32 {
    x as f32
}
pub(super) fn f32_convert_i64_u(x: i64) -> f32 {
    (x as u64) as f32
}
pub(super) fn f64_convert_i32_s(x: i32) -> f64 {
    f64::from(x)
}
pub(super) fn f64_convert_i32_u(x: i32) -> f64 {
    f64::from(x as u32)
}
pub(super) fn f64_convert_i64_s(x: i64) -> f64 {
    x as f64
}
pub(super) fn f64_convert_i64_u(x: i64) -> f64 {
    (x as u64) as f64
}
pub(super) fn f32_demote_f64(x: f64) -> f32 {
    x as f32
}
pub(super) fn f64_promote_f32(x: f32) -> f64 {
    f64::from(x)
}
