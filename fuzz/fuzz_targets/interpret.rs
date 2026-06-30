#![no_main]
//! #35 target (b): a wasm-smith-generated valid module must instantiate and run (fuel-bounded)
//! without panicking or hanging — any trap/return is fine.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    submilli_wasm_fuzz::interpret(data);
});
