//! `Engine` — shared, thread-safe compilation/runtime root; holds the epoch counter.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};

use crate::canon::{AggKind, CanonicalTypeId, GroupId, ModuleType, TypeRegistry};
use crate::config::{CollectorKind, Config};
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

#[allow(clippy::struct_excessive_bools)] // independent engine flags derived from `Config`
#[derive(Debug)]
struct EngineInner {
    epoch: AtomicU64,
    max_wasm_stack: usize,
    consume_fuel: bool,
    epoch_interruption: bool,
    is_async: bool,
    /// The resolved collector strategy (mark-sweep or allocate-only null), decided at
    /// `Engine::new` from `Config::collector`.
    collector: CollectorKind,
    /// Engine-wide GC-pressure threshold (bytes); also sizes each store's heap ceiling.
    gc_memory_threshold: Option<usize>,
    /// Per-store pre-authorized GC budget (bytes): reservation growth within it skips the limiter,
    /// and it caps a single growth step (`Config::gc_heap_reservation`).
    gc_heap_reservation: usize,
    /// Default validation-time module-size ceiling (bytes) for `Module::new` — the untrusted
    /// tier (`Config::max_module_bytes`, #32). Trusted modules override it per-compile.
    max_module_bytes: usize,
    /// Total GC bytes committed (reserved) across all of the engine's stores — updated at
    /// reservation-batch granularity (never per object), so it has no hot-path cost. Drives the
    /// engine-wide GC-pressure axis (§14).
    gc_committed: AtomicUsize,
    /// Per-store GC-request **mailboxes**: each live store registers a flag here (held as a `Weak`,
    /// the store owns the `Arc`). When `gc_committed` crosses `gc_memory_threshold`, the engine
    /// posts to every mailbox; each store reads (and clears) *its own* at a back-edge safe point and
    /// self-collects. A `Store` is `!Sync`, so this is the only way the engine can *request* (never
    /// force) a collection on another thread — and one store servicing its request leaves the others'
    /// untouched. Dead mailboxes are pruned on post.
    gc_requests: Mutex<Vec<Weak<AtomicBool>>>,
    /// Whether to capture backtraces (`Config::wasm_backtrace`); also gates the per-`Op` offset
    /// table + `name`-section retention at parse (#29c).
    wasm_backtrace: bool,
    /// Whether to retain the module's `.debug_*` DWARF for source-level backtraces (#29c).
    retain_dwarf: bool,
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
                collector: config.collector_kind().resolve()?,
                gc_memory_threshold: config.gc_memory_threshold_bytes(),
                gc_heap_reservation: config.gc_heap_reservation_bytes(),
                max_module_bytes: config.max_module_bytes_value(),
                gc_committed: AtomicUsize::new(0),
                gc_requests: Mutex::new(Vec::new()),
                wasm_backtrace: config.wasm_backtrace_enabled(),
                retain_dwarf: config.debug_info_enabled()
                    || (config.wasm_backtrace_enabled() && config.wasm_backtrace_details_enabled()),
                types: RwLock::new(TypeRegistry::default()),
            }),
        })
    }

    /// Reads the engine type registry, **recovering from lock poisoning** (#33): a panic while
    /// some other store held this lock must not cascade a panic into every other tenant on the
    /// engine. Registry operations are short and leave it structurally valid, so the recovered
    /// guard is safe to use.
    fn types_read(&self) -> RwLockReadGuard<'_, TypeRegistry> {
        self.inner
            .types
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes the engine type registry, recovering from lock poisoning — see [`Self::types_read`].
    fn types_write(&self) -> RwLockWriteGuard<'_, TypeRegistry> {
        self.inner
            .types
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Interns a module's rec groups into the engine registry, returning the per-module-type
    /// canonical ids and the registered group ids (held by the `Module` for release on drop).
    pub(crate) fn intern_types(
        &self,
        types: &[ModuleType],
    ) -> (Vec<CanonicalTypeId>, Vec<GroupId>) {
        self.types_write().intern_module(types)
    }

    /// Releases a module's registered group ids (decrement refcounts; reclaim at zero).
    pub(crate) fn release_types(&self, group_ids: &[GroupId]) {
        self.types_write().release(group_ids);
    }

    /// Adds a registration to the group owning `id` (a type handle was cloned / materialized).
    pub(crate) fn incref_type(&self, id: CanonicalTypeId) {
        self.types_write().incref_type(id);
    }

    /// Removes a registration from the group owning `id` (a type handle was dropped).
    pub(crate) fn decref_type(&self, id: CanonicalTypeId) {
        self.types_write().decref_type(id);
    }

    /// Removes one registration from group `g` (a `RecGroup` / `Module` handle was dropped).
    pub(crate) fn release_group(&self, g: GroupId) {
        self.release_types(&[g]);
    }

    /// Adds one registration to group `g` (a `RecGroup` was cloned).
    pub(crate) fn incref_group(&self, g: GroupId) {
        self.types_write().incref_group(g);
    }

    /// The number of live (registered) rec groups — for leak/reclamation tests.
    pub(crate) fn live_group_count(&self) -> usize {
        self.types_read().live_group_count()
    }

    /// Whether canonical type `sub` is a declared subtype of `sup`.
    pub(crate) fn is_subtype(&self, sub: CanonicalTypeId, sup: CanonicalTypeId) -> bool {
        self.types_read().is_subtype(sub, sup)
    }

    /// The kind (func/struct/array) of a canonical type id, if registered.
    pub(crate) fn type_kind(&self, id: CanonicalTypeId) -> Option<AggKind> {
        self.types_read().kind(id)
    }

    /// Interns a host-built func type and returns its canonical id. (The group is held by the
    /// engine for its lifetime — host-type reclamation is a later collector concern.)
    pub(crate) fn intern_func_type(
        &self,
        params: &[ValType],
        results: &[ValType],
    ) -> CanonicalTypeId {
        self.types_write().intern_func(params, results).0
    }

    pub(crate) fn intern_struct_type(
        &self,
        finality: Finality,
        supertype: Option<CanonicalTypeId>,
        fields: &[FieldType],
    ) -> CanonicalTypeId {
        self.types_write()
            .intern_struct(finality, supertype, fields)
            .0
    }

    pub(crate) fn intern_array_type(
        &self,
        finality: Finality,
        supertype: Option<CanonicalTypeId>,
        field: &FieldType,
    ) -> CanonicalTypeId {
        self.types_write()
            .intern_array(finality, supertype, field)
            .0
    }

    /// Interns a host-built rec group (`RecGroupBuilder`), returning the members' canonical ids
    /// and the group id (with one registration, adopted by the resulting `RecGroup`).
    pub(crate) fn intern_host_group(
        &self,
        members: &[ModuleType],
        externals: &[CanonicalTypeId],
    ) -> (Vec<CanonicalTypeId>, GroupId) {
        self.types_write().intern_host_group(members, externals)
    }

    /// The engine's canonical type registry lock (used by the two-phase materializers in `canon`,
    /// which acquire it briefly to clone canonical data, then build handles after releasing it).
    pub(crate) fn types(&self) -> &RwLock<TypeRegistry> {
        &self.inner.types
    }

    /// The materialized (params, results) of a canonical func type.
    pub(crate) fn func_sig(&self, id: CanonicalTypeId) -> (Vec<ValType>, Vec<ValType>) {
        crate::canon::func_sig(self, id)
    }

    /// The materialized fields of a canonical struct type.
    pub(crate) fn struct_fields(&self, id: CanonicalTypeId) -> Vec<FieldType> {
        crate::canon::struct_fields(self, id)
    }

    /// The materialized element of a canonical array type.
    pub(crate) fn array_field(&self, id: CanonicalTypeId) -> FieldType {
        crate::canon::array_field(self, id)
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

    /// Whether backtraces are captured (`Config::wasm_backtrace`); also whether the per-`Op` offset
    /// table + `name` section are retained at parse.
    pub(crate) fn wasm_backtrace_enabled(&self) -> bool {
        self.inner.wasm_backtrace
    }

    /// Whether the module's `.debug_*` DWARF is retained for source-level backtraces.
    pub(crate) fn retain_dwarf(&self) -> bool {
        self.inner.retain_dwarf
    }

    /// The resolved collector strategy (mark-sweep or allocate-only null).
    pub(crate) fn collector(&self) -> CollectorKind {
        self.inner.collector
    }

    /// Whether the engine runs a tracing collector (i.e. not the allocate-only null collector).
    pub(crate) fn is_collecting(&self) -> bool {
        self.inner.collector == CollectorKind::MarkSweep
    }

    /// The engine-wide GC-pressure threshold in bytes, if configured. Sizes each store's GC-heap
    /// ceiling and is the high-water mark for the engine-wide GC-pressure axis.
    pub(crate) fn gc_memory_threshold(&self) -> Option<usize> {
        self.inner.gc_memory_threshold
    }

    /// The per-store pre-authorized GC reservation in bytes (`Config::gc_heap_reservation`).
    pub(crate) fn gc_heap_reservation(&self) -> usize {
        self.inner.gc_heap_reservation
    }

    /// The default validation-time module-size ceiling for `Module::new` (`Config::max_module_bytes`).
    pub(crate) fn max_module_bytes(&self) -> usize {
        self.inner.max_module_bytes
    }

    /// Registers a new store's GC-request mailbox, returning the flag the store owns (the engine
    /// keeps a `Weak`). Posted to under engine-wide GC pressure; read-and-cleared by the store.
    pub(crate) fn register_gc_request(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut mailboxes) = self.inner.gc_requests.lock() {
            mailboxes.push(Arc::downgrade(&flag));
        }
        flag
    }

    /// Records a reservation growth of `delta` GC bytes against the engine-wide committed total. If
    /// it crosses `gc_memory_threshold` (when configured), posts a collection request to every live
    /// store's mailbox (pruning dead ones). Batch-granular, so this is off the hot path.
    pub(crate) fn add_gc_committed(&self, delta: usize) {
        let total = self.inner.gc_committed.fetch_add(delta, Ordering::Relaxed) + delta;
        if self.inner.gc_memory_threshold.is_none_or(|t| total <= t) {
            return;
        }
        if let Ok(mut mailboxes) = self.inner.gc_requests.lock() {
            mailboxes.retain(|w| match w.upgrade() {
                Some(flag) => {
                    flag.store(true, Ordering::Relaxed);
                    true
                }
                None => false, // store dropped — prune its mailbox
            });
        }
    }

    /// Releases `delta` GC bytes from the engine-wide committed total (a store dropped its heap).
    pub(crate) fn sub_gc_committed(&self, delta: usize) {
        self.inner.gc_committed.fetch_sub(delta, Ordering::Relaxed);
    }

    /// Total GC bytes committed (reserved) across the engine's stores (for tests / introspection).
    pub(crate) fn gc_committed_bytes(&self) -> usize {
        self.inner.gc_committed.load(Ordering::Relaxed)
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

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
