//! Keys two interpreter cfgs on this crate's optimization level (profiles set `debug-assertions`
//! independently of it):
//! - `submilli_unoptimized` (`opt-level = 0`): keep the giant per-op `step` out of line (see
//!   `exec::step`).
//! - `submilli_inline_hot` (`opt-level` 2 or 3): force-inline hot per-op helpers into the
//!   dispatch. At opt-level 1, which doesn't share stack slots across inlined code, the forced
//!   inlines grow the dispatch frame — live under every host re-entry, so charged against
//!   `max_wasm_stack` per re-entry. (`s`/`z` would be fine; they're left out to keep code small.)
//!
//! Caveat: the dispatch loop is generic over the store's data type, so it is compiled in the
//! embedder's crate at *its* opt level; a mixed profile (this crate optimized, the embedder at 0)
//! still inlines into an unoptimized loop. See PERF-NOTES §25.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(submilli_unoptimized)");
    println!("cargo::rustc-check-cfg=cfg(submilli_inline_hot)");
    println!("cargo::rerun-if-changed=build.rs");
    let level = std::env::var("OPT_LEVEL").unwrap_or_default();
    if level == "0" {
        println!("cargo::rustc-cfg=submilli_unoptimized");
    }
    if level == "2" || level == "3" {
        println!("cargo::rustc-cfg=submilli_inline_hot");
    }
}
