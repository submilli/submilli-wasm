//! `Engine` — shared, thread-safe compilation/runtime root; holds the epoch counter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use crate::config::Config;
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
            }),
        })
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
