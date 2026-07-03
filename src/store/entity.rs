//! Runtime entity *data* owned by the store (the contents of `StoreInner`'s arenas).
//!
//! Kept separate from the public handle types (`Memory`/`Global`/`Table`/`Func`/
//! `Instance`), which live in their own modules — those are the embedder-facing
//! API; these are interpreter internals. Behavior on the handles
//! reads/writes these through `StoreInner`.

use core::any::Any;

use crate::extern_::{Global, Memory, Table, Tag};
use crate::func::Func;
use crate::instance::Instance;
use crate::module::Module;
use crate::value::{FuncType, GlobalType, MemoryType, Ref, TableType, TagType, Val};
use crate::{Error, Result};

use super::reclaim::ReclaimArena;

/// The error for a failed initial memory/table allocation (`try_reserve` denial or an over-large
/// declared size). A clean instantiation/`new` failure — never an OOM-abort.
fn alloc_err() -> Error {
    Error::msg("failed to allocate the initial memory or table")
}

/// Per-arena-entry overhead charged into the GC-heap byte budget for an `externref`/`exn` entry: the
/// slot `Option`, the parallel generation `u32`, and the `Box`/`Vec` allocator overhead — the
/// externref/exn analog of the GC heap's per-object overhead. A `Host` externref's boxed payload is
/// an opaque `T` whose size we can't see, so we charge this fixed wrapper cost (slightly
/// conservative); it's enough to drive collection + the limiter so arena growth can't run unbounded.
const ARENA_ENTRY_OVERHEAD: usize = 64;

/// One externref-arena entry: a host payload, or an *internalized* `anyref` produced by
/// `extern.convert_any` (carrying that ref's handle so `any.convert_extern` recovers it).
pub(crate) enum ExternEntry {
    Host(Box<dyn Any + Send + Sync>),
    Internal(u32),
}

impl ExternEntry {
    /// Heap byte charge for the GC budget (the opaque `Host` payload is charged a fixed wrapper cost).
    pub(crate) fn byte_size(&self) -> usize {
        ARENA_ENTRY_OVERHEAD
    }
}

/// Heap byte charge for one `externref` entry — the reservation pre-charge host `ExternRef::new`
/// uses before allocating (the `Host` payload's true size is opaque).
pub(crate) fn extern_charge() -> usize {
    ARENA_ENTRY_OVERHEAD
}

/// Store-side arena of `externref` entries — reclaimable (#27g): the mark-sweep collector frees
/// unreachable entries and recycles their slots, so `extern.convert_any` / host `ExternRef::new`
/// can't grow it without bound.
pub(crate) type ExternRefs = ReclaimArena<ExternEntry>;

/// Heap byte charge for an exception instance carrying `n_args` argument values (the reservation
/// pre-charge `throw` computes before popping; mirrors [`ExnEntity::byte_size`]).
pub(crate) fn exn_charge(n_args: usize) -> usize {
    ARENA_ENTRY_OVERHEAD.saturating_add(n_args.saturating_mul(core::mem::size_of::<Val>()))
}

/// Size of a WebAssembly memory page, in bytes.
pub(crate) const PAGE_SIZE: usize = 64 * 1024;
/// Maximum addressable pages for a 32-bit memory (4 GiB).
const MAX_PAGES: u64 = 1 << 16;

/// Runtime data backing a [`Memory`](crate::Memory).
#[derive(Debug)]
pub(crate) struct MemoryEntity {
    pub bytes: Vec<u8>,
    pub ty: MemoryType,
}

/// Runtime data backing a [`Global`](crate::Global).
#[derive(Debug)]
pub(crate) struct GlobalEntity {
    pub value: Val,
    pub ty: GlobalType,
}

/// Runtime data backing a [`Table`](crate::Table).
#[derive(Debug)]
pub(crate) struct TableEntity {
    pub elems: Vec<Ref>,
    pub ty: TableType,
}

/// Runtime data backing a [`Tag`](crate::Tag). A tag carries no mutable state — its identity is
/// the arena slot (store address), so two instantiations get distinct tags; an imported tag is
/// the same handle shared across modules. The `ty` is the exception signature.
#[derive(Debug)]
pub(crate) struct TagEntity {
    pub ty: TagType,
}

/// An exception instance backing an `exnref`: the throwing tag plus the argument values it carries.
/// `Rooted<ExnRef>` indexes the store's exn arena. The `args` are **GC roots** the tracing collector
/// (#27g) enumerates transitively; the arena itself is reclaimable, so an unreachable exception
/// (e.g. one caught and dropped in a throw-loop) is freed by mark-sweep.
#[derive(Debug)]
pub(crate) struct ExnEntity {
    pub tag: Tag,
    pub args: Vec<Val>,
    /// Backtrace captured at the original throw site (#29d). Carried on the instance so a
    /// `throw_ref` rethrow preserves the original site; `None` for host-created or when
    /// `wasm_backtrace` is off.
    pub backtrace: Option<crate::backtrace::WasmBacktrace>,
}

impl ExnEntity {
    /// Heap byte charge for the GC budget (overhead + the carried argument values).
    pub(crate) fn byte_size(&self) -> usize {
        exn_charge(self.args.len())
    }
}

/// Runtime data backing a [`Func`](crate::Func): either a defined wasm function
/// (its defining instance + module-space index) or a host function (its dynamic
/// signature + an index into the owning `Store<T>`'s typed `host_funcs`).
#[derive(Debug)]
pub(crate) enum FuncEntity {
    Wasm {
        instance: Instance,
        func_index: u32,
    },
    Host {
        ty: FuncType,
        /// Signature materialized once at registration so the per-call path never walks
        /// the engine's type registry (see `exec::host::invoke_host`).
        sig: std::sync::Arc<HostSig>,
        host_index: u32,
    },
    /// An async host function: signature + index into the store's `async_host_funcs`.
    /// The closure returns a future the async driver awaits before resuming.
    #[cfg(feature = "async")]
    HostAsync {
        ty: FuncType,
        /// Cached call-shape, like `Host::sig` (the async boundary is per-call hot too).
        sig: std::sync::Arc<HostSig>,
        host_index: u32,
    },
}

/// Runtime data backing an [`Instance`](crate::Instance): its resolved index
/// spaces.
#[derive(Debug)]
pub(crate) struct InstanceEntity {
    pub module: Module,
    pub funcs: Vec<Func>,
    pub memories: Vec<Memory>,
    pub globals: Vec<Global>,
    pub tables: Vec<Table>,
    /// Resolved tag handles (imported first, then defined) — store-address identity (#28a).
    pub tags: Vec<Tag>,
    /// Per-data-segment "dropped" flag (one bool per module data segment). Active
    /// segments are marked dropped right after instantiation; `data.drop` marks
    /// passive ones. A `memory.init` from a dropped segment with `len > 0` traps.
    pub dropped_data: Vec<bool>,
    /// Per-element-segment evaluated reference list (the "element instance"), built once at
    /// instantiation — so `table.init`/`array.new_elem`/`array.init_elem` copy these refs rather
    /// than re-evaluating the segment's expressions (which would re-allocate aggregates and break
    /// reference identity). Active/declared segments and `elem.drop`ped ones hold an empty vec.
    pub elems: Vec<Vec<Ref>>,
}

impl MemoryEntity {
    /// Allocates the initial (zeroed) backing store. Fallible (`try_reserve_exact`) so an over-large
    /// declared *initial* size returns a clean error instead of OOM-aborting the process, even with
    /// no `ResourceLimiter` installed (the limiter/default-ceiling policy gate runs before this).
    ///
    /// **Zero-on-allocation (#33b):** the backing bytes are always `resize(len, 0)`-initialized
    /// before the guest can read them — a guest must never observe a prior tenant's freed bytes or
    /// allocator residue. Uninitialized fast-paths (`set_len`/`MaybeUninit`/`with_capacity`-then-expose)
    /// are forbidden and already blocked by the crate's zero-`unsafe` invariant. The same holds for
    /// `grow` below and `TableEntity::new`/`grow`. See `SECURITY.md` (#36) for the full statement.
    pub(crate) fn new(ty: MemoryType) -> Result<Self> {
        let len = (ty.minimum() as usize)
            .checked_mul(PAGE_SIZE)
            .ok_or_else(alloc_err)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).map_err(|_| alloc_err())?;
        bytes.resize(len, 0);
        Ok(MemoryEntity { bytes, ty })
    }

    pub(crate) fn size_pages(&self) -> u64 {
        (self.bytes.len() / PAGE_SIZE) as u64
    }

    /// Grows by `delta` pages; returns the previous page count, or `None` if it would exceed the
    /// declared/architectural maximum **or** the host can't allocate it. The architectural ceiling
    /// is the index width's (memory64 → 2^48 pages, #42), and the allocation is fallible
    /// (`try_reserve_exact`) so an over-large guest grow returns -1 rather than OOM-aborting.
    pub(crate) fn grow(&mut self, delta: u64) -> Option<u64> {
        let old = self.size_pages();
        let new = old.checked_add(delta)?;
        let arch_max = if self.ty.is_64() {
            1u64 << 48
        } else {
            MAX_PAGES
        };
        let max = self.ty.maximum().unwrap_or(arch_max).min(arch_max);
        if new > max {
            return None;
        }
        let new_bytes = (new as usize).checked_mul(PAGE_SIZE)?;
        self.bytes
            .try_reserve_exact(new_bytes.saturating_sub(self.bytes.len()))
            .ok()?;
        self.bytes.resize(new_bytes, 0);
        Some(old)
    }
}

impl TableEntity {
    /// Allocates the initial backing store (every slot the typed `init` ref). Fallible like
    /// [`MemoryEntity::new`] so an over-large declared *initial* size errors rather than aborting.
    pub(crate) fn new(ty: TableType, init: Ref) -> Result<Self> {
        let len = ty.minimum() as usize;
        let mut elems = Vec::new();
        elems.try_reserve_exact(len).map_err(|_| alloc_err())?;
        elems.resize(len, init);
        Ok(TableEntity { elems, ty })
    }

    pub(crate) fn size(&self) -> u64 {
        self.elems.len() as u64
    }

    pub(crate) fn get(&self, index: u64) -> Option<Ref> {
        self.elems.get(index as usize).cloned()
    }

    pub(crate) fn set(&mut self, index: u64, val: Ref) -> bool {
        match self.elems.get_mut(index as usize) {
            Some(slot) => {
                *slot = val;
                true
            }
            None => false,
        }
    }

    /// Grows by `delta` elements (filled with `init`); returns the previous size, or `None` if it
    /// would exceed the declared/architectural maximum **or** the host can't allocate it. table64
    /// (#42) lifts the architectural ceiling to `u64::MAX`; the allocation is fallible so an
    /// over-large guest grow returns -1 rather than OOM-aborting.
    pub(crate) fn grow(&mut self, delta: u64, init: Ref) -> Option<u64> {
        let old = self.size();
        let new = old.checked_add(delta)?;
        let arch_max = if self.ty.is_64() {
            u64::MAX
        } else {
            u64::from(u32::MAX)
        };
        let max = self.ty.maximum().unwrap_or(arch_max).min(arch_max);
        if new > max {
            return None;
        }
        self.elems.try_reserve_exact(delta as usize).ok()?;
        self.elems.resize(new as usize, init);
        Some(old)
    }

    /// Fills `len` elements from `dst` with `val`; returns false if out of bounds.
    pub(crate) fn fill(&mut self, dst: u64, val: Ref, len: u64) -> bool {
        let end = match dst.checked_add(len) {
            Some(end) if end <= self.size() => end,
            _ => return false,
        };
        for i in dst..end {
            self.elems[i as usize] = val.clone();
        }
        true
    }
}

/// A host function's call-shape, cached at registration: the param types (to decode
/// operand cells into `Val` args) and one default `Val` per result (to pre-fill the
/// results buffer the callback writes into). Shared via `Arc` so the per-call path
/// borrows nothing from the store.
#[derive(Debug)]
pub(crate) struct HostSig {
    pub params: Box<[crate::value::ValType]>,
    pub result_defaults: Box<[crate::value::Val]>,
}

impl HostSig {
    pub(crate) fn new(ty: &FuncType) -> std::sync::Arc<HostSig> {
        std::sync::Arc::new(HostSig {
            params: ty.params().collect(),
            result_defaults: ty
                .results()
                .map(|t| crate::value::Val::default_for_valtype(&t))
                .collect(),
        })
    }
}
