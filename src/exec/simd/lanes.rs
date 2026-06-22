//! `v128` ⇄ typed-lane-array split/join, plus generic lanewise combinators. All `#[inline]` and
//! over fixed-size arrays so LLVM autovectorizes the elementwise loops (the wasmi approach); zero
//! `unsafe` — bytes are moved with `to_le_bytes`/`from_le_bytes`, never transmuted.

/// `b[i*2..]` as a 2-byte array (no bounds-check panic on validated 16-byte input).
#[inline]
fn chunk2(b: &[u8; 16], i: usize) -> [u8; 2] {
    core::array::from_fn(|k| b[i * 2 + k])
}
#[inline]
fn chunk4(b: &[u8; 16], i: usize) -> [u8; 4] {
    core::array::from_fn(|k| b[i * 4 + k])
}
#[inline]
fn chunk8(b: &[u8; 16], i: usize) -> [u8; 8] {
    core::array::from_fn(|k| b[i * 8 + k])
}

// --- split: v128 bits → typed lanes ---
#[inline]
pub(super) fn i8x16(v: u128) -> [i8; 16] {
    v.to_le_bytes().map(|b| b as i8)
}
#[inline]
pub(super) fn u8x16(v: u128) -> [u8; 16] {
    v.to_le_bytes()
}
#[inline]
pub(super) fn i16x8(v: u128) -> [i16; 8] {
    let b = v.to_le_bytes();
    core::array::from_fn(|i| i16::from_le_bytes(chunk2(&b, i)))
}
#[inline]
pub(super) fn u16x8(v: u128) -> [u16; 8] {
    let b = v.to_le_bytes();
    core::array::from_fn(|i| u16::from_le_bytes(chunk2(&b, i)))
}
#[inline]
pub(super) fn i32x4(v: u128) -> [i32; 4] {
    let b = v.to_le_bytes();
    core::array::from_fn(|i| i32::from_le_bytes(chunk4(&b, i)))
}
#[inline]
pub(super) fn u32x4(v: u128) -> [u32; 4] {
    let b = v.to_le_bytes();
    core::array::from_fn(|i| u32::from_le_bytes(chunk4(&b, i)))
}
#[inline]
pub(super) fn i64x2(v: u128) -> [i64; 2] {
    let b = v.to_le_bytes();
    core::array::from_fn(|i| i64::from_le_bytes(chunk8(&b, i)))
}
#[inline]
pub(super) fn u64x2(v: u128) -> [u64; 2] {
    let b = v.to_le_bytes();
    core::array::from_fn(|i| u64::from_le_bytes(chunk8(&b, i)))
}
#[inline]
pub(super) fn f32x4(v: u128) -> [f32; 4] {
    u32x4(v).map(f32::from_bits)
}
#[inline]
pub(super) fn f64x2(v: u128) -> [f64; 2] {
    u64x2(v).map(f64::from_bits)
}

// --- join: typed lanes → v128 bits ---
#[inline]
pub(super) fn from_i8x16(l: [i8; 16]) -> u128 {
    u128::from_le_bytes(l.map(|x| x as u8))
}
#[inline]
pub(super) fn from_u8x16(l: [u8; 16]) -> u128 {
    u128::from_le_bytes(l)
}
#[inline]
fn pack<const N: usize, const W: usize>(lanes: [[u8; W]; N]) -> u128 {
    let mut b = [0u8; 16];
    for i in 0..N {
        for k in 0..W {
            b[i * W + k] = lanes[i][k];
        }
    }
    u128::from_le_bytes(b)
}
#[inline]
pub(super) fn from_i16x8(l: [i16; 8]) -> u128 {
    pack(l.map(i16::to_le_bytes))
}
#[inline]
pub(super) fn from_u16x8(l: [u16; 8]) -> u128 {
    pack(l.map(u16::to_le_bytes))
}
#[inline]
pub(super) fn from_i32x4(l: [i32; 4]) -> u128 {
    pack(l.map(i32::to_le_bytes))
}
#[inline]
pub(super) fn from_u32x4(l: [u32; 4]) -> u128 {
    pack(l.map(u32::to_le_bytes))
}
#[inline]
pub(super) fn from_i64x2(l: [i64; 2]) -> u128 {
    pack(l.map(i64::to_le_bytes))
}
#[inline]
pub(super) fn from_u64x2(l: [u64; 2]) -> u128 {
    pack(l.map(u64::to_le_bytes))
}
#[inline]
pub(super) fn from_f32x4(l: [f32; 4]) -> u128 {
    from_u32x4(l.map(f32::to_bits))
}
#[inline]
pub(super) fn from_f64x2(l: [f64; 2]) -> u128 {
    from_u64x2(l.map(f64::to_bits))
}

// --- generic lanewise combinators ---
#[inline]
pub(super) fn zip<T: Copy, const N: usize>(a: [T; N], b: [T; N], f: impl Fn(T, T) -> T) -> [T; N] {
    core::array::from_fn(|i| f(a[i], b[i]))
}
#[inline]
pub(super) fn map<T: Copy, U, const N: usize>(a: [T; N], f: impl Fn(T) -> U) -> [U; N] {
    a.map(f)
}
