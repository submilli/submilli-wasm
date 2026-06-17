//! Proves the shared example is valid `wasmtime` code (so `compat_alias` is a
//! real equivalence, not a tautology): the same example compiles against the
//! pinned real `wasmtime`. Compile-only.

#![allow(dead_code, unused_variables, unused_imports)]
#![allow(clippy::all, clippy::pedantic)]

include!("common/embedder_example.rs");
