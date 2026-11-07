//! Stack plumbing for the numeric conversion ops (pop one operand, push the converted result).
//! The ops themselves dispatch flat in [`step`](super::step); the conversion functions live in
//! [`super::convert`], arithmetic helpers in [`super::arith`].

use super::Execution;
use crate::Result;

impl Execution {
    pub(super) fn trunc_to_i32(&mut self, f: impl Fn(f32) -> Result<i32>) -> Result<()> {
        let x = self.pop_f32()?;
        self.push_i32(f(x)?);
        Ok(())
    }
    pub(super) fn trunc64_to_i32(&mut self, f: impl Fn(f64) -> Result<i32>) -> Result<()> {
        let x = self.pop_f64()?;
        self.push_i32(f(x)?);
        Ok(())
    }
    pub(super) fn trunc_to_i64(&mut self, f: impl Fn(f32) -> Result<i64>) -> Result<()> {
        let x = self.pop_f32()?;
        self.push_i64(f(x)?);
        Ok(())
    }
    pub(super) fn trunc64_to_i64(&mut self, f: impl Fn(f64) -> Result<i64>) -> Result<()> {
        let x = self.pop_f64()?;
        self.push_i64(f(x)?);
        Ok(())
    }
    pub(super) fn map_f32_i32(&mut self, f: impl Fn(f32) -> i32) -> Result<()> {
        let x = self.pop_f32()?;
        self.push_i32(f(x));
        Ok(())
    }
    pub(super) fn map_f64_i32(&mut self, f: impl Fn(f64) -> i32) -> Result<()> {
        let x = self.pop_f64()?;
        self.push_i32(f(x));
        Ok(())
    }
    pub(super) fn map_f32_i64(&mut self, f: impl Fn(f32) -> i64) -> Result<()> {
        let x = self.pop_f32()?;
        self.push_i64(f(x));
        Ok(())
    }
    pub(super) fn map_f64_i64(&mut self, f: impl Fn(f64) -> i64) -> Result<()> {
        let x = self.pop_f64()?;
        self.push_i64(f(x));
        Ok(())
    }
    pub(super) fn map_i32_f32(&mut self, f: impl Fn(i32) -> f32) -> Result<()> {
        let x = self.pop_i32()?;
        self.push_f32(f(x));
        Ok(())
    }
    pub(super) fn map_i64_f32(&mut self, f: impl Fn(i64) -> f32) -> Result<()> {
        let x = self.pop_i64()?;
        self.push_f32(f(x));
        Ok(())
    }
    pub(super) fn map_f64_f32(&mut self, f: impl Fn(f64) -> f32) -> Result<()> {
        let x = self.pop_f64()?;
        self.push_f32(f(x));
        Ok(())
    }
    pub(super) fn map_i32_f64(&mut self, f: impl Fn(i32) -> f64) -> Result<()> {
        let x = self.pop_i32()?;
        self.push_f64(f(x));
        Ok(())
    }
    pub(super) fn map_i64_f64(&mut self, f: impl Fn(i64) -> f64) -> Result<()> {
        let x = self.pop_i64()?;
        self.push_f64(f(x));
        Ok(())
    }
    pub(super) fn map_f32_f64(&mut self, f: impl Fn(f32) -> f64) -> Result<()> {
        let x = self.pop_f32()?;
        self.push_f64(f(x));
        Ok(())
    }
}
