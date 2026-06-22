//! Compiled-artifact codec for `Module::serialize`/`deserialize` and
//! `Engine::precompile_module`.
//!
//! We serialize the *compiled* [`ModuleInner`] (the `Op` stream with the folded
//! sidetable, plus section metadata) — not the original wasm — so deserialize restores
//! it directly, skipping the validate+compile pass (the genuine startup win, the analog
//! of wasmtime skipping codegen). The format is `MAGIC` + a `u32` version + a postcard
//! body. It deliberately mirrors our churning `Op`/sidetable layout, so cross-version
//! artifacts are **rejected** (bump [`ARTIFACT_VERSION`] whenever that layout changes).

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
const ARTIFACT_VERSION: u32 = 11;

/// Encodes a compiled module into the binary artifact format.
pub(crate) fn encode(inner: &ModuleInner) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&ARTIFACT_VERSION.to_le_bytes());
    let body = postcard::to_allocvec(inner)
        .map_err(|e| Error::msg(format!("failed to serialize module: {e}")))?;
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decodes an artifact produced by [`encode`]. Rejects a wrong magic or version.
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
    postcard::from_bytes(body).map_err(|e| Error::msg(format!("failed to deserialize module: {e}")))
}
