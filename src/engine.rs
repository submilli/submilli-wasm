//! `Engine` — shared, thread-safe compilation/runtime root; holds the epoch counter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};

use crate::canon::{AggKind, CanonicalTypeId, GroupId, ModuleType, TypeRegistry};
use crate::config::{Collector, Config};
use crate::value::{FieldType, Finality, ValType};
use crate::Result;

/// A compiled-code and runtime environment, shared across `Store`s and threads.
///
/// Cheap to clone (reference-counted handle), `Send + Sync` — mirrors `wasmtime::Engine`.
#[derive(Clone, Debug)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

/// Default wasm stack budget (bytes) when `Config::max_wasm_stack` is unset,
/// matching wasmtime's 512 KiB default.
const DEFAULT_MAX_WASM_STACK: usize = 512 * 1024;

#[derive(Debug)]
struct EngineInner {
    epoch: AtomicU64,
    max_wasm_stack: usize,
    consume_fuel: bool,
    epoch_interruption: bool,
    is_async: bool,
    /// Selected garbage collector. Recorded now; all variants behave allocate-only
    /// (null collector) until a tracing collector lands.
    collector: Collector,
    /// Engine-wide GC-pressure threshold (bytes); also sizes each store's heap ceiling
    /// for now (the engine-wide aggregate axis lands with the tracing collector).
    gc_memory_threshold: Option<usize>,
    /// Engine-wide canonical type registry (cross-module GC type identity). Locked only by
    /// `Module::new`/`Drop` to register/release rec groups — runtime compares the baked
    /// canonical ids without touching it.
    types: RwLock<TypeRegistry>,
}

impl Engine {
    /// Creates a new `Engine` from the given configuration.
    pub fn new(config: &Config) -> Result<Engine> {
        Ok(Engine {
            inner: Arc::new(EngineInner {
                epoch: AtomicU64::new(0),
                max_wasm_stack: config
                    .max_wasm_stack_bytes()
                    .unwrap_or(DEFAULT_MAX_WASM_STACK),
                consume_fuel: config.consume_fuel_enabled(),
                epoch_interruption: config.epoch_interruption_enabled(),
                is_async: config.async_support_enabled(),
                collector: config.collector_kind(),
                gc_memory_threshold: config.gc_memory_threshold_bytes(),
                types: RwLock::new(TypeRegistry::default()),
            }),
        })
    }

    /// Interns a module's rec groups into the engine registry, returning the per-module-type
    /// canonical ids and the registered group ids (held by the `Module` for release on drop).
    pub(crate) fn intern_types(
        &self,
        types: &[ModuleType],
    ) -> (Vec<CanonicalTypeId>, Vec<GroupId>) {
        self.inner
            .types
            .write()
            .expect("type registry poisoned")
            .intern_module(types)
    }

    /// Releases a module's registered group ids (decrement refcounts; reclaim at zero).
    pub(crate) fn release_types(&self, group_ids: &[GroupId]) {
        self.inner
            .types
            .write()
            .expect("type registry poisoned")
            .release(group_ids);
    }

    /// Whether canonical type `sub` is a declared subtype of `sup`.
    pub(crate) fn is_subtype(&self, sub: CanonicalTypeId, sup: CanonicalTypeId) -> bool {
        self.inner
            .types
            .read()
            .expect("type registry poisoned")
            .is_subtype(sub, sup)
    }

    /// The kind (func/struct/array) of a canonical type id, if registered.
    pub(crate) fn type_kind(&self, id: CanonicalTypeId) -> Option<AggKind> {
        self.inner
            .types
            .read()
            .expect("type registry poisoned")
            .kind(id)
    }

    /// Interns a host-built func type and returns its canonical id. (The group is held by the
    /// engine for its lifetime — host-type reclamation is a later collector concern.)
    pub(crate) fn intern_func_type(
        &self,
        params: &[ValType],
        results: &[ValType],
    ) -> CanonicalTypeId {
        self.inner
            .types
            .write()
            .expect("type registry poisoned")
            .intern_func(params, results)
            .0
    }

    pub(crate) fn intern_struct_type(
        &self,
        finality: Finality,
        supertype: Option<CanonicalTypeId>,
        fields: &[FieldType],
    ) -> CanonicalTypeId {
        self.inner
            .types
            .write()
            .expect("type registry poisoned")
            .intern_struct(finality, supertype, fields)
            .0
    }

    pub(crate) fn intern_array_type(
        &self,
        finality: Finality,
        supertype: Option<CanonicalTypeId>,
        field: &FieldType,
    ) -> CanonicalTypeId {
        self.inner
            .types
            .write()
            .expect("type registry poisoned")
            .intern_array(finality, supertype, field)
            .0
    }

    /// The materialized (params, results) of a canonical func type.
    pub(crate) fn func_sig(&self, id: CanonicalTypeId) -> (Vec<ValType>, Vec<ValType>) {
        self.inner
            .types
            .read()
            .expect("type registry poisoned")
            .func_sig(self, id)
    }

    /// The materialized fields of a canonical struct type.
    pub(crate) fn struct_fields(&self, id: CanonicalTypeId) -> Vec<FieldType> {
        self.inner
            .types
            .read()
            .expect("type registry poisoned")
            .struct_fields(self, id)
    }

    /// The materialized element of a canonical array type.
    pub(crate) fn array_field(&self, id: CanonicalTypeId) -> FieldType {
        self.inner
            .types
            .read()
            .expect("type registry poisoned")
            .array_field(self, id)
    }

    /// Whether epoch-based interruption is enabled (`Config::epoch_interruption`).
    pub(crate) fn epoch_interruption(&self) -> bool {
        self.inner.epoch_interruption
    }

    /// The wasm execution-stack byte budget (`Config::max_wasm_stack`).
    pub(crate) fn max_wasm_stack(&self) -> usize {
        self.inner.max_wasm_stack
    }

    /// Whether fuel metering is enabled (`Config::consume_fuel`).
    pub(crate) fn consume_fuel(&self) -> bool {
        self.inner.consume_fuel
    }

    /// Whether async execution is enabled (`Config::async_support`).
    pub(crate) fn is_async(&self) -> bool {
        self.inner.is_async
    }

    /// The selected garbage collector. Recorded now; consulted once a tracing collector
    /// lands — every variant is allocate-only (null collector) until then.
    #[allow(dead_code)]
    pub(crate) fn collector(&self) -> Collector {
        self.inner.collector
    }

    /// The engine-wide GC-pressure threshold in bytes, if configured. Sizes each store's
    /// GC-heap ceiling for now (the engine-wide aggregate axis lands with the collector).
    pub(crate) fn gc_memory_threshold(&self) -> Option<usize> {
        self.inner.gc_memory_threshold
    }

    /// Compiles `bytes` and returns the serialized compiled artifact (as
    /// [`Module::serialize`]), suitable for `unsafe Module::deserialize`. Validates +
    /// compiles once here so deserialize can skip both.
    pub fn precompile_module(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        crate::module::Module::from_binary(self, bytes)?.serialize()
    }

    /// Bumps the epoch counter; pairs with `Store::set_epoch_deadline`.
    pub fn increment_epoch(&self) {
        self.inner.epoch.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns a non-owning handle to this engine (e.g. for an epoch ticker thread).
    pub fn weak(&self) -> EngineWeak {
        EngineWeak {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub(crate) fn current_epoch(&self) -> u64 {
        self.inner.epoch.load(Ordering::Relaxed)
    }
}

impl Default for Engine {
    fn default() -> Engine {
        Engine::new(&Config::default()).expect("default config is always valid")
    }
}

/// A non-owning handle to an [`Engine`], obtained via [`Engine::weak`].
#[derive(Clone, Debug, Default)]
pub struct EngineWeak {
    inner: Weak<EngineInner>,
}

impl EngineWeak {
    /// Upgrades to a strong [`Engine`] handle if the engine is still alive.
    pub fn upgrade(&self) -> Option<Engine> {
        self.inner.upgrade().map(|inner| Engine { inner })
    }
}
