//! The embedder-facing exception error. An uncaught guest exception (or a host `throw`) surfaces
//! from `Func::call` as a [`ThrownException`] carried on the `Error`; the exception object itself is
//! recovered from the store via `take_pending_exception`. Matches wasmtime's `ThrownException`
//! (a unit error type — the payload lives on the store, not the error).

/// Error type for an uncaught WebAssembly exception. Recover via `err.is::<ThrownException>()` /
/// `err.downcast_ref::<ThrownException>()`, then `store.take_pending_exception()` for the `exnref`.
#[derive(Debug)]
pub struct ThrownException;

impl core::fmt::Display for ThrownException {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("wasm exception thrown")
    }
}

impl std::error::Error for ThrownException {}
