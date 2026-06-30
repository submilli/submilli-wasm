#![no_main]
//! #35 target (a): the validator/compiler must never panic on arbitrary bytes — only return `Err`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    submilli_wasm_fuzz::validate(data);
});
