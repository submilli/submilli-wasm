//! Async resource-limiter tests (`--features async`): growth/allocation decisions
//! that `.await`. Uses a test limiter that yields once (proving the await path) then
//! enforces a page cap, installed via `Store::limiter_async`.
#![allow(clippy::unwrap_used)]

use pollster::block_on;

use crate::config::Config;
use crate::engine::Engine;
use crate::extern_::Memory;
use crate::instance::Instance;
use crate::module::Module;
use crate::store::{ResourceLimiterAsync, Store};
use crate::value::{MemoryType, Val};

const PAGE: usize = 64 * 1024;

/// One-shot yield (Pending once, then Ready) so the limiter genuinely awaits.
async fn yield_once() {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct Y(bool);
    impl Future for Y {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
    Y(false).await;
}

/// An async limiter that caps memory at `max_pages` total, awaiting a yield first.
struct AsyncCap {
    max_pages: usize,
}

#[async_trait::async_trait]
impl ResourceLimiterAsync for AsyncCap {
    async fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> crate::Result<bool> {
        yield_once().await;
        Ok(desired <= self.max_pages * PAGE)
    }

    async fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> crate::Result<bool> {
        Ok(true)
    }
}

fn async_engine() -> Engine {
    let mut config = Config::new();
    config.async_support(true);
    Engine::new(&config).unwrap()
}

fn capped_store(engine: &Engine, max_pages: usize) -> Store<AsyncCap> {
    let mut store = Store::new(engine, AsyncCap { max_pages });
    store.limiter_async(|s| s);
    store
}

const GROWER: &str = "(module (memory 1)
    (func (export \"grow\") (param i32) (result i32) local.get 0 memory.grow))";

#[test]
fn in_wasm_memory_grow_consults_async_limiter() {
    let engine = async_engine();
    let m = Module::new(&engine, wat::parse_str(GROWER).unwrap()).unwrap();
    let mut store = capped_store(&engine, 2); // allow up to 2 pages total
    let inst = block_on(Instance::new_async(&mut store, &m, &[])).unwrap();
    let grow = inst.get_func(&mut store, "grow").unwrap();

    let mut out = [Val::I32(0)];
    block_on(grow.call_async(&mut store, &[Val::I32(1)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), 1); // 1 → 2 pages allowed
    block_on(grow.call_async(&mut store, &[Val::I32(1)], &mut out)).unwrap();
    assert_eq!(out[0].unwrap_i32(), -1); // 2 → 3 denied by the async limiter
}

#[test]
fn memory_new_async_respects_async_limiter() {
    let mut store = capped_store(&async_engine(), 2);
    // Within cap.
    assert!(block_on(Memory::new_async(&mut store, MemoryType::new(1, None))).is_ok());
    // Over cap.
    assert!(block_on(Memory::new_async(&mut store, MemoryType::new(3, None))).is_err());
}

#[test]
fn sync_memory_grow_errors_with_async_limiter() {
    let mut store = capped_store(&async_engine(), 4);
    let mem = block_on(Memory::new_async(&mut store, MemoryType::new(1, None))).unwrap();
    // Sync grow can't await the async limiter → error.
    assert!(mem.grow(&mut store, 1).is_err());
}

#[test]
fn sync_memory_new_errors_with_async_limiter() {
    let mut store = capped_store(&async_engine(), 4);
    assert!(Memory::new(&mut store, MemoryType::new(1, None)).is_err());
}
