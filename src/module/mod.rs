//! `Module` — parse + validate + compile a wasm binary to internal bytecode.

pub(crate) mod compile;
pub(crate) mod inner;
pub(crate) mod op;
pub(crate) mod parse;
mod serialize;

use std::path::Path;
use std::sync::Arc;

use wasmparser::{Validator, WasmFeatures};

use crate::engine::Engine;
use crate::module::inner::ModuleInner;
use crate::value::{ExportType, ExternType, ImportType};
use crate::{Error, Result};

/// The set of WebAssembly proposals enabled during Phase 1 (core + the small
/// proposals our interpreter supports). Reference-types/GC/exceptions/SIMD/threads
/// are intentionally off until their phases (and will be `Config`-driven later).
pub(crate) fn phase1_features() -> WasmFeatures {
    WasmFeatures::MUTABLE_GLOBAL
        | WasmFeatures::SIGN_EXTENSION
        | WasmFeatures::MULTI_VALUE
        | WasmFeatures::BULK_MEMORY
        | WasmFeatures::SATURATING_FLOAT_TO_INT
        | WasmFeatures::FLOATS
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
        let _ = engine;
        Ok(Module(Arc::new(serialize::decode(bytes.as_ref())?)))
    }

    /// Like [`Module::deserialize`] but reads the artifact from a file.
    ///
    /// # Safety
    /// See [`Module::deserialize`].
    #[allow(unsafe_code)] // No unsafe operations; `unsafe` is wasmtime API parity only.
    pub unsafe fn deserialize_file(engine: &Engine, path: impl AsRef<Path>) -> Result<Module> {
        let _ = engine;
        let bytes = std::fs::read(path).map_err(|e| Error::msg(e.to_string()))?;
        Ok(Module(Arc::new(serialize::decode(&bytes)?)))
    }

    /// Validates a module without compiling it.
    pub fn validate(engine: &Engine, binary: &[u8]) -> Result<()> {
        let _ = engine;
        Validator::new_with_features(phase1_features())
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
