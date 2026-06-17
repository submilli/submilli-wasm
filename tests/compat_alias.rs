//! Proves our public API accepts real `wasmtime` embedder code: the shared
//! example compiles with `submilli_wasm` aliased as `wasmtime`. Compile-only.

#![allow(dead_code, unused_variables, unused_imports)]
#![allow(clippy::all, clippy::pedantic)]

use submilli_wasm as wasmtime;

include!("common/embedder_example.rs");
