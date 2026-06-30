#![no_main]
//! #35 target (c): a wasm-smith-generated module run on submilli and wasmtime must agree — same
//! return values (NaN-canonical) and the same trap-vs-return category; a divergence is a bug.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    submilli_wasm_fuzz::differential(data);
});
