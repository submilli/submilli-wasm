//! `Module` — parse + validate + compile a wasm binary to internal bytecode.

pub(crate) mod compile;
pub(crate) mod debug;
pub(crate) mod handler;
pub(crate) mod inner;
pub(crate) mod op;
#[cfg(feature = "simd")]
pub(crate) mod op_simd;
pub(crate) mod parse;
mod serialize;
mod typesec;

use std::path::Path;
use std::sync::Arc;

use wasmparser::{Validator, WasmFeatures};

use crate::engine::Engine;
use crate::module::inner::ModuleInner;
use crate::value::{ExportType, ExternType, ImportType};
use crate::{Error, Result};

/// The set of WebAssembly proposals our interpreter currently enables. GC/exceptions/
/// SIMD/threads are intentionally off until implemented (and will be `Config`-driven later).
pub(crate) fn enabled_features() -> WasmFeatures {
    WasmFeatures::MUTABLE_GLOBAL
        | WasmFeatures::SIGN_EXTENSION
        | WasmFeatures::MULTI_VALUE
        | WasmFeatures::BULK_MEMORY
        | WasmFeatures::SATURATING_FLOAT_TO_INT
        | WasmFeatures::FLOATS
        | WasmFeatures::REFERENCE_TYPES
        // `externref` (the `extern` heap type) is gated behind GC_TYPES in wasmparser
        // even though it's a reference-types value.
        | WasmFeatures::GC_TYPES
        // Typed/non-nullable refs, `call_ref`, `ref.as_non_null`, `br_on_null`/`br_on_non_null`.
        | WasmFeatures::FUNCTION_REFERENCES
        // Tail calls: `return_call`/`return_call_indirect`/`return_call_ref` (#39).
        | WasmFeatures::TAIL_CALL
        // Full GC: struct/array type definitions, rec groups, sub/final. The aggregate
        // instructions + casts are deferred — validated modules using them skip in the harness.
        | WasmFeatures::GC
        // Exception handling (`exnref` + `try_table`). Tags + their imports/exports are decoded
        // here (#28a); the `throw`/`throw_ref`/`try_table` instructions are deferred — validated
        // modules using them skip in the harness until #28c–#28e. Legacy `try/catch/delegate`
        // (`LEGACY_EXCEPTIONS`) stays off.
        | WasmFeatures::EXCEPTIONS
        // Const-expr `i32`/`i64` `add`/`sub`/`mul` + `global.get` of prior immutable globals (#40).
        | WasmFeatures::EXTENDED_CONST
        // More than one memory per module; every memory op carries an explicit index (#41).
        | WasmFeatures::MULTI_MEMORY
        // 64-bit memories and tables (`i64` index type); one flag covers both (#42).
        | WasmFeatures::MEMORY64
        // Fixed-width SIMD (`v128`), gated behind the `simd` Cargo feature (#37).
        | simd_features()
}

#[cfg(feature = "simd")]
fn simd_features() -> WasmFeatures {
    // Relaxed SIMD (#38) builds on v128, so it rides the same `simd` feature.
    WasmFeatures::SIMD | WasmFeatures::RELAXED_SIMD
}

#[cfg(not(feature = "simd"))]
fn simd_features() -> WasmFeatures {
    WasmFeatures::empty()
}

/// A compiled, reusable WebAssembly module. Shareable across stores of the same
/// engine; cloning is a cheap `Arc` bump.
#[derive(Clone, Debug)]
pub struct Module(Arc<ModuleInner>);

impl Module {
    /// Parses, validates, and compiles a module from a `.wasm` binary.
    ///
    /// (Unlike `wasmtime` with its `wat` feature, we accept binary only; convert
    /// `.wat` with the `wat` crate first.)
    pub fn new(engine: &Engine, bytes: impl AsRef<[u8]>) -> Result<Module> {
        Module::from_binary(engine, bytes.as_ref())
    }

    /// Like [`Module::new`] but accepts binary `.wasm` only.
    pub fn from_binary(engine: &Engine, binary: &[u8]) -> Result<Module> {
        Module::validate(engine, binary)?;
        let inner = parse::parse_module(engine, binary)?;
        Ok(Module(Arc::new(inner)))
    }

    /// Reads and compiles a module from a file path.
    pub fn from_file(engine: &Engine, file: impl AsRef<Path>) -> Result<Module> {
        let bytes = std::fs::read(file).map_err(|e| Error::msg(e.to_string()))?;
        Module::from_binary(engine, &bytes)
    }

    /// Serializes this compiled module to the binary artifact format consumed by
    /// [`Module::deserialize`]. The artifact is the *compiled* form, not the original
    /// wasm — restoring it skips validation + compilation.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        serialize::encode(&self.0)
    }

    /// Restores a [`Module`] from an artifact produced by [`Module::serialize`] /
    /// [`Engine::precompile_module`], **without** re-validating or recompiling.
    ///
    /// # Safety
    /// Matches `wasmtime::Module::deserialize`: the artifact must be a trusted blob
    /// produced by a matching version of this crate (it is not guest-reachable input).
    /// A corrupted artifact cannot cause undefined behavior here — the interpreter runs
    /// in safe Rust with bounds-checked dispatch, so the worst case is a trap rather than
    /// memory unsafety — but cross-version/garbage blobs are rejected up front.
    #[allow(unsafe_code)] // No unsafe operations; `unsafe` is wasmtime API parity only.
    pub unsafe fn deserialize(engine: &Engine, bytes: impl AsRef<[u8]>) -> Result<Module> {
        let mut inner = serialize::decode(bytes.as_ref())?;
        // Canonical type ids are engine-specific — re-intern the (module-relative) artifact
        // against the target engine.
        inner.intern(engine);
        Ok(Module(Arc::new(inner)))
    }

    /// Like [`Module::deserialize`] but reads the artifact from a file.
    ///
    /// # Safety
    /// See [`Module::deserialize`].
    #[allow(unsafe_code)] // No unsafe operations; `unsafe` is wasmtime API parity only.
    pub unsafe fn deserialize_file(engine: &Engine, path: impl AsRef<Path>) -> Result<Module> {
        let bytes = std::fs::read(path).map_err(|e| Error::msg(e.to_string()))?;
        let mut inner = serialize::decode(&bytes)?;
        inner.intern(engine);
        Ok(Module(Arc::new(inner)))
    }

    /// Validates a module without compiling it.
    pub fn validate(engine: &Engine, binary: &[u8]) -> Result<()> {
        let _ = engine;
        Validator::new_with_features(enabled_features())
            .validate_all(binary)
            .map(|_| ())
            .map_err(|e| Error::msg(e.to_string()))
    }

    pub fn imports(&self) -> impl ExactSizeIterator<Item = ImportType<'_>> + '_ {
        self.0.imports.iter().map(|imp| {
            ImportType::new(&imp.module, &imp.name, self.0.import_extern_type(&imp.kind))
        })
    }

    pub fn exports(&self) -> impl ExactSizeIterator<Item = ExportType<'_>> + '_ {
        self.0
            .exports
            .iter()
            .map(|e| ExportType::new(&e.name, self.0.export_extern_type(e.kind)))
    }

    pub fn get_export(&self, name: &str) -> Option<ExternType> {
        self.0
            .exports
            .iter()
            .find(|e| e.name == name)
            .map(|e| self.0.export_extern_type(e.kind))
    }

    /// Internal access to the compiled representation (for instantiation/exec).
    pub(crate) fn inner(&self) -> &ModuleInner {
        &self.0
    }
}
