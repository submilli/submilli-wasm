//! `Config` — engine/runtime configuration (wasmtime-compatible builder).

/// Global configuration for an [`crate::Engine`]. Builder methods return `&mut Self`.
#[derive(Clone, Debug, Default)]
pub struct Config {
    consume_fuel: bool,
    epoch_interruption: bool,
    max_wasm_stack: Option<usize>,
    collector: Collector,
    gc_memory_threshold: Option<usize>,
    async_support: bool,
}

impl Config {
    /// Creates a configuration with default settings.
    pub fn new() -> Self {
        Config::default()
    }

    /// Enables fuel consumption (metering). Off by default.
    pub fn consume_fuel(&mut self, enable: bool) -> &mut Self {
        self.consume_fuel = enable;
        self
    }

    /// Whether fuel metering is enabled.
    pub(crate) fn consume_fuel_enabled(&self) -> bool {
        self.consume_fuel
    }

    /// Enables epoch-based interruption. Off by default.
    pub fn epoch_interruption(&mut self, enable: bool) -> &mut Self {
        self.epoch_interruption = enable;
        self
    }

    /// Whether epoch-based interruption is enabled.
    pub(crate) fn epoch_interruption_enabled(&self) -> bool {
        self.epoch_interruption
    }

    /// Sets the maximum wasm operand-stack size, in bytes.
    pub fn max_wasm_stack(&mut self, size: usize) -> &mut Self {
        self.max_wasm_stack = Some(size);
        self
    }

    /// The configured max wasm stack size in bytes, if set.
    pub(crate) fn max_wasm_stack_bytes(&self) -> Option<usize> {
        self.max_wasm_stack
    }

    pub fn wasm_multi_value(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn wasm_tail_call(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn wasm_bulk_memory(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn wasm_reference_types(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn wasm_function_references(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn wasm_gc(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn wasm_exceptions(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn memory_reservation(&mut self, bytes: u64) -> &mut Self {
        self
    }

    pub fn memory_reservation_for_growth(&mut self, bytes: u64) -> &mut Self {
        self
    }

    pub fn memory_may_move(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn memory_init_cow(&mut self, enable: bool) -> &mut Self {
        self
    }

    pub fn memory_guard_size(&mut self, bytes: u64) -> &mut Self {
        self
    }

    pub fn gc_heap_reservation(&mut self, bytes: u64) -> &mut Self {
        self
    }

    pub fn gc_heap_guard_size(&mut self, bytes: u64) -> &mut Self {
        self
    }

    pub fn gc_heap_reservation_for_growth(&mut self, bytes: u64) -> &mut Self {
        self
    }

    pub fn gc_heap_may_move(&mut self, enable: bool) -> &mut Self {
        self
    }

    /// Accepted for wasmtime parity; this interpreter has no Cranelift backend so the
    /// optimization level has no effect (compilation is a single linear pre-decode pass).
    pub fn cranelift_opt_level(&mut self, level: OptLevel) -> &mut Self {
        self
    }

    /// Accepted for wasmtime parity; backtrace-detail behavior arrives with DWARF
    /// debug-info support, so this is currently a no-op.
    pub fn wasm_backtrace_details(&mut self, enable: WasmBacktraceDetails) -> &mut Self {
        self
    }

    /// Engine-wide GC-pressure high-water mark, in bytes.
    ///
    /// **Additive deviation from wasmtime** — there is no analog in `wasmtime::Config`.
    /// When total committed GC bytes across all stores of the engine cross this value,
    /// the engine requests collection from its stores (checked at the fuel/epoch
    /// back-edge safe point). Defaults to ~80% of detected physical RAM if unset.
    /// See `docs/ARCHITECTURE.md` §14.
    pub fn gc_memory_threshold(&mut self, bytes: usize) -> &mut Self {
        self.gc_memory_threshold = Some(bytes);
        self
    }

    pub fn collector(&mut self, collector: Collector) -> &mut Self {
        self.collector = collector;
        self
    }

    /// The selected garbage collector (read by the `Engine`; uniform until a tracing collector lands).
    pub(crate) fn collector_kind(&self) -> Collector {
        self.collector
    }

    /// The configured engine-wide GC-pressure threshold in bytes, if set.
    pub(crate) fn gc_memory_threshold_bytes(&self) -> Option<usize> {
        self.gc_memory_threshold
    }

    /// Enables async execution (`Func::call_async`, async host fns, yields).
    /// Off by default. Once enabled, the sync entry points reject this store.
    #[cfg(feature = "async")]
    pub fn async_support(&mut self, enable: bool) -> &mut Self {
        self.async_support = enable;
        self
    }

    /// Whether async support is enabled (`Config::async_support`).
    pub(crate) fn async_support_enabled(&self) -> bool {
        self.async_support
    }

    /// Accepted for wasmtime API parity but a **no-op** for this interpreter:
    /// suspend/resume is just parking the `Execution` state machine (no native
    /// fiber stack to size — see ARCHITECTURE §2.4), so there is no async stack.
    #[cfg(feature = "async")]
    pub fn async_stack_size(&mut self, size: usize) -> &mut Self {
        self
    }
}

/// Garbage-collector selection, mirroring `wasmtime::Collector`.
///
/// Our interpreter implements a single internal strategy — non-moving
/// stop-the-world mark-sweep; all variants are accepted for API compatibility
/// and select the same collector.
#[non_exhaustive]
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum Collector {
    #[default]
    Auto,
    DeferredReferenceCounting,
    Null,
    Copying,
}

/// Cranelift optimization level, mirroring `wasmtime::OptLevel`. Accepted for API
/// parity only — this interpreter has no optimizing backend, so all variants behave
/// identically (see [`Config::cranelift_opt_level`]).
#[non_exhaustive]
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum OptLevel {
    #[default]
    None,
    Speed,
    SpeedAndSize,
}

/// Whether to retain detailed backtrace info, mirroring `wasmtime::WasmBacktraceDetails`.
/// Currently accepted for parity; real effect arrives with DWARF debug-info support.
#[non_exhaustive]
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum WasmBacktraceDetails {
    #[default]
    Enable,
    Disable,
}
