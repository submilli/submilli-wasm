//! Compiled-artifact codec for `Module::serialize`/`deserialize` and
//! `Engine::precompile_module`.
//!
//! We serialize the *compiled* [`ModuleInner`] (the `Op` stream with the folded
//! sidetable, plus section metadata) — not the original wasm — so deserialize restores
//! it directly, skipping the validate+compile pass (the genuine startup win, the analog
//! of wasmtime skipping codegen). The format is `MAGIC` + a `u32` version + a `u64` body
//! checksum + a postcard body. It deliberately mirrors our churning `Op`/sidetable layout, so
//! cross-version artifacts are **rejected** (bump [`ARTIFACT_VERSION`] whenever that layout
//! changes), and so are corrupted ones (the checksum: truncation, bit rot, a partial write).

use crate::module::inner::ModuleInner;
use crate::{Error, Result};

/// Identifies a submilli compiled artifact. (`subm` + format tag.)
const MAGIC: &[u8; 8] = b"submwc01";
/// Artifact format version — bump on any `Op`/`ModuleInner`/value-type layout change.
/// v5: retain DWARF/`name` debug info + per-`Op` offsets (#29a) so backtraces survive round-trip.
/// v6: extended-const arithmetic `ConstOp`s (#40).
/// v7: per-op memory index (`MemArg.memory` + management-op indices) for multi-memory (#41).
/// v8: tail-call `Op`s (`ReturnCall`/`ReturnCallIndirect`/`ReturnCallRef`, #39).
/// v9: memory64/table64 — `MemArg.offset` widened to u64, `IrTableType` gains `table64` + u64 limits (#42).
/// v10: fixed-width SIMD — `Op::Simd(SimdOp)` + `ConstOp::V128` (#37, `simd` feature).
/// v11: relaxed SIMD — 20 `SimdOp::*Relaxed*` variants (#38).
/// v12: `CompiledFunc.ops` stored as `Vec<Op>` (write-once, pre-sized) instead of `Box<[Op]>`.
/// v13: `v128.const`/`i8x16.shuffle` immediates pooled in `CodeArenas::v128s` (16-byte `Op`).
/// v14: slot-addressed `Op`s (static-height frame slots, step 2a) + pooled `br_on_cast` heaps.
/// v15: three-address numeric/load/store `Op`s, `Copy`, two-slot `BrIfCmp` (step 2b).
/// v16: i32 immediate-operand `Op`s and `BrIfCmpImm` (step 2c).
/// v17: `Op::CallInternal` (direct calls within a module).
/// v18: body checksum in the header.
const ARTIFACT_VERSION: u32 = 18;

/// Encodes a compiled module into the binary artifact format.
pub(crate) fn encode(inner: &ModuleInner) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&ARTIFACT_VERSION.to_le_bytes());
    let body = postcard::to_allocvec(inner)
        .map_err(|e| Error::msg(format!("failed to serialize module: {e}")))?;
    out.extend_from_slice(&checksum(&body).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decodes an artifact produced by [`encode`]. Rejects a wrong magic, version, or checksum.
pub(crate) fn decode(bytes: &[u8]) -> Result<ModuleInner> {
    let rest = bytes
        .strip_prefix(MAGIC)
        .ok_or_else(|| Error::msg("not a submilli serialized module"))?;
    let (ver, body) = rest
        .split_first_chunk::<4>()
        .ok_or_else(|| Error::msg("truncated serialized module header"))?;
    if u32::from_le_bytes(*ver) != ARTIFACT_VERSION {
        return Err(Error::msg(
            "incompatible serialized module (version mismatch)",
        ));
    }
    let (sum, body) = body
        .split_first_chunk::<8>()
        .ok_or_else(|| Error::msg("truncated serialized module header"))?;
    if u64::from_le_bytes(*sum) != checksum(body) {
        return Err(Error::msg(
            "corrupted serialized module (checksum mismatch)",
        ));
    }
    postcard::from_bytes(body).map_err(|e| Error::msg(format!("failed to deserialize module: {e}")))
}

/// A 64-bit integrity hash of the artifact body, word at a time. Every step is a bijection of the
/// running state for a fixed word (rotate, xor, multiply by an odd constant), so any change
/// confined to one word always changes the result; mixing the length in up front makes even a
/// truncation of trailing zero bytes change it (any other change misses with ~2^-64 odds). Catches
/// accidental corruption — not deliberate tampering: the artifact is trusted input (see
/// `Module::deserialize`).
fn checksum(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mix = |h: u64, word: u64| (h.rotate_left(23) ^ word).wrapping_mul(K);
    let (words, rest) = bytes.as_chunks::<8>();
    let mut h = mix(0, bytes.len() as u64);
    for word in words {
        h = mix(h, u64::from_le_bytes(*word));
    }
    let mut tail = [0u8; 8];
    for (t, &b) in tail.iter_mut().zip(rest) {
        *t = b;
    }
    h = mix(h, u64::from_le_bytes(tail));
    h ^ (h >> 29)
}
