//! Stack plumbing for the numeric conversion ops (pop one operand, push the converted result).
//! The ops themselves dispatch flat in [`step`](super::step); the conversion functions live in
//! [`super::convert`], arithmetic helpers in [`super::arith`].

use super::cell::Cell;
use super::stack::Un;
use super::Execution;
use crate::Result;

impl Execution {
    pub(super) fn trunc_to_i32(&mut self, at: Un, f: impl Fn(f32) -> Result<i32>) -> Result<()> {
        self.unop_try_at(at, |c| Ok(Cell::from_i32(f(c.unwrap_f32())?)))
    }
    pub(super) fn trunc64_to_i32(&mut self, at: Un, f: impl Fn(f64) -> Result<i32>) -> Result<()> {
        self.unop_try_at(at, |c| Ok(Cell::from_i32(f(c.unwrap_f64())?)))
    }
    pub(super) fn trunc_to_i64(&mut self, at: Un, f: impl Fn(f32) -> Result<i64>) -> Result<()> {
        self.unop_try_at(at, |c| Ok(Cell::from_i64(f(c.unwrap_f32())?)))
    }
    pub(super) fn trunc64_to_i64(&mut self, at: Un, f: impl Fn(f64) -> Result<i64>) -> Result<()> {
        self.unop_try_at(at, |c| Ok(Cell::from_i64(f(c.unwrap_f64())?)))
    }
    pub(super) fn map_i32_i64(&mut self, at: Un, f: impl Fn(i32) -> i64) -> Result<()> {
        self.unop_at(at, |c| Cell::from_i64(f(c.unwrap_i32())))
    }
    pub(super) fn map_i64_i32(&mut self, at: Un, f: impl Fn(i64) -> i32) -> Result<()> {
        self.unop_at(at, |c| Cell::from_i32(f(c.unwrap_i64())))
    }
    pub(super) fn map_f32_i32(&mut self, at: Un, f: impl Fn(f32) -> i32) -> Result<()> {
        self.unop_at(at, |c| Cell::from_i32(f(c.unwrap_f32())))
    }
    pub(super) fn map_f64_i32(&mut self, at: Un, f: impl Fn(f64) -> i32) -> Result<()> {
        self.unop_at(at, |c| Cell::from_i32(f(c.unwrap_f64())))
    }
    pub(super) fn map_f32_i64(&mut self, at: Un, f: impl Fn(f32) -> i64) -> Result<()> {
        self.unop_at(at, |c| Cell::from_i64(f(c.unwrap_f32())))
    }
    pub(super) fn map_f64_i64(&mut self, at: Un, f: impl Fn(f64) -> i64) -> Result<()> {
        self.unop_at(at, |c| Cell::from_i64(f(c.unwrap_f64())))
    }
    pub(super) fn map_i32_f32(&mut self, at: Un, f: impl Fn(i32) -> f32) -> Result<()> {
        self.unop_at(at, |c| Cell::from_f32(f(c.unwrap_i32())))
    }
    pub(super) fn map_i64_f32(&mut self, at: Un, f: impl Fn(i64) -> f32) -> Result<()> {
        self.unop_at(at, |c| Cell::from_f32(f(c.unwrap_i64())))
    }
    pub(super) fn map_f64_f32(&mut self, at: Un, f: impl Fn(f64) -> f32) -> Result<()> {
        self.unop_at(at, |c| Cell::from_f32(f(c.unwrap_f64())))
    }
    pub(super) fn map_i32_f64(&mut self, at: Un, f: impl Fn(i32) -> f64) -> Result<()> {
        self.unop_at(at, |c| Cell::from_f64(f(c.unwrap_i32())))
    }
    pub(super) fn map_i64_f64(&mut self, at: Un, f: impl Fn(i64) -> f64) -> Result<()> {
        self.unop_at(at, |c| Cell::from_f64(f(c.unwrap_i64())))
    }
    pub(super) fn map_f32_f64(&mut self, at: Un, f: impl Fn(f32) -> f64) -> Result<()> {
        self.unop_at(at, |c| Cell::from_f64(f(c.unwrap_f32())))
    }
}
