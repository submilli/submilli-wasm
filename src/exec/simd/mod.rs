//! Fixed-width SIMD (`v128`, #37) execution. `Op::Simd(s)` routes here; memory ops need the store,
//! everything else is pure operand-stack compute dispatched through a handler chain (mirroring the
//! gc chain). Lane math is portable scalar code (see [`lanes`]); SIMD never traps except on a
//! memory-op out-of-bounds.

// Cascades to the whole `simd` subtree (#33 carve-out, exec hot-path gate). SIMD indexing is into
// fixed-size lane arrays (`[T; N]`, statically in-bounds — lane indices are validated immediates) or
// the bounds-checked `load_n`/`store_n` memory helpers — never unchecked guest input.
#![allow(clippy::indexing_slicing)]

mod bit;
mod cvt;
mod farith;
mod fcmp;
mod iarith;
mod iarith2;
mod icmp;
mod lane;
mod lanes;
mod mem;
mod relaxed;

use super::Execution;
use crate::instance::Instance;
use crate::module::op_simd::SimdOp;
use crate::store::StoreInner;
use crate::value::{Val, V128};
use crate::Result;

impl Execution {
    pub(super) fn exec_simd(
        &mut self,
        inner: &mut StoreInner,
        code: &crate::module::op::CompiledFunc,
        s: &SimdOp,
        instance: Instance,
    ) -> Result<()> {
        // Memory ops (load/store/lane/splat/extend/zero) consult the store; the rest are pure.
        if self.exec_simd_mem(inner, code, s, instance)? {
            return Ok(());
        }
        self.exec_simd_compute(s)
    }

    /// Pure (non-memory) SIMD ops, dispatched through the category chain.
    fn exec_simd_compute(&mut self, s: &SimdOp) -> Result<()> {
        self.exec_simd_lane(s)
    }

    #[inline]
    fn pop_v128(&mut self) -> u128 {
        self.pop().unwrap_v128().as_u128()
    }

    #[inline]
    fn push_v128(&mut self, v: u128) {
        self.push(Val::V128(V128::from(v)));
    }

    /// Lanewise binary op: split both operands into typed lanes, `zip` with `f`, rejoin.
    fn v_binop<T: Copy, const N: usize>(
        &mut self,
        split: fn(u128) -> [T; N],
        join: fn([T; N]) -> u128,
        f: impl Fn(T, T) -> T,
    ) {
        let b = self.pop_v128();
        let a = self.pop_v128();
        self.push_v128(join(lanes::zip(split(a), split(b), f)));
    }

    /// Lanewise unary op (same lane count). Lane-count-changing ops (extend/narrow) are bespoke.
    fn v_unop<T: Copy, U, const N: usize>(
        &mut self,
        split: fn(u128) -> [T; N],
        join: fn([U; N]) -> u128,
        f: impl Fn(T) -> U,
    ) {
        let a = self.pop_v128();
        self.push_v128(join(lanes::map(split(a), f)));
    }

    /// Lanewise comparison: each lane becomes an all-ones (`0xff…`) or all-zero mask.
    fn v_cmp<T: Copy, const N: usize>(
        &mut self,
        split: fn(u128) -> [T; N],
        f: impl Fn(T, T) -> bool,
    ) {
        let b = self.pop_v128();
        let a = self.pop_v128();
        let (la, lb) = (split(a), split(b));
        let mask: [bool; N] = core::array::from_fn(|i| f(la[i], lb[i]));
        self.push_v128(cmp_mask(mask));
    }

    /// Lanewise shift by a scalar `i32` count, masked to the lane width.
    fn v_shift<T: Copy, const N: usize>(
        &mut self,
        split: fn(u128) -> [T; N],
        join: fn([T; N]) -> u128,
        f: impl Fn(T, u32) -> T,
    ) {
        let count = self.pop_i32() as u32;
        let a = self.pop_v128();
        let s = count % (128 / N as u32);
        self.push_v128(join(lanes::map(split(a), |x| f(x, s))));
    }
}

/// Builds a v128 whose `i`-th lane (of `16/N` bytes) is all-ones when `mask[i]`, else zero.
fn cmp_mask<const N: usize>(mask: [bool; N]) -> u128 {
    let w = 16 / N;
    let mut b = [0u8; 16];
    for i in 0..N {
        if mask[i] {
            for k in 0..w {
                b[i * w + k] = 0xff;
            }
        }
    }
    u128::from_le_bytes(b)
}
