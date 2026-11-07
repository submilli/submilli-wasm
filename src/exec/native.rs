//! Native-stack accounting for nested entries (#30). A host function that calls back into wasm, or
//! into another host function, nests Rust frames on the native stack, which the heap frame
//! accounting can't see; unbounded, that recursion aborts the process. So we measure it (as
//! wasmtime bounds the real stack pointer): the outermost entry on a thread ([`Entry`]) records its
//! stack position, and every nested entry charges the native bytes used since then against
//! `max_wasm_stack`. A position is the address of a local — compared, never dereferenced, so no
//! `unsafe`.
//!
//! The base is per thread, not per store (unlike wasmtime's per-store limit): a store entered from
//! another store's host fn is charged the stack beneath it too — the conservative choice, since the
//! native stack is shared. This assumes one contiguous stack per thread: running entries on
//! stackful coroutines or a stack-growing helper (`stacker`) is unsupported, and on wasm32 only the
//! shadow stack is measured, not the engine's own call stack (keep `max_wasm_stack` modest there).

use std::cell::Cell;
use std::future::Future;

thread_local! {
    /// `(base position, live entries)`: the outermost live entry's stack position, and how many
    /// entries are live on this thread.
    static ENTRIES: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

/// The current native stack position.
#[inline(never)]
fn position() -> usize {
    let marker = 0u8;
    std::hint::black_box(std::ptr::addr_of!(marker)).addr()
}

fn entries() -> (usize, usize) {
    ENTRIES.try_with(Cell::get).unwrap_or((0, 0))
}

fn set_entries(value: (usize, usize)) {
    let _ = ENTRIES.try_with(|entries| entries.set(value));
}

/// One embedder entry (`Func::call`, or one poll of `Func::call_async`) for as long as it lives;
/// the outermost on the thread records the base position.
pub(crate) struct Entry(());

impl Entry {
    pub(crate) fn begin() -> Self {
        let (base, live) = entries();
        let base = if live == 0 { position() } else { base };
        set_entries((base, live.saturating_add(1)));
        Entry(())
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        let (base, live) = entries();
        set_entries((base, live.saturating_sub(1)));
    }
}

/// Native bytes used since the outermost entry, if the caller is a *nested* entry (`0` for the
/// outermost one, whose frames the heap accounting already covers, and outside any entry).
pub(crate) fn nested_bytes() -> usize {
    match entries() {
        (base, live) if live > 1 => base.abs_diff(position()),
        _ => 0,
    }
}

/// Runs `future` as an entry on every poll: between polls the stack unwinds, and the task may
/// resume on another thread, so the base can't outlive one poll.
pub(crate) async fn per_poll<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        let _entry = Entry::begin();
        future.as_mut().poll(cx)
    })
    .await
}
