//! Runtime entity *data* owned by the store (the contents of `StoreInner`'s arenas).
//!
//! Kept separate from the public handle types (`Memory`/`Global`/`Table`/`Func`/
//! `Instance`), which live in their own modules — those are the embedder-facing
//! API; these are interpreter internals. Behavior on the handles
//! reads/writes these through `StoreInner`.

use core::any::Any;
use std::sync::Arc;

use crate::extern_::{Global, Memory, Table, Tag};
use crate::func::Func;
use crate::instance::Instance;
use crate::module::op::CompiledFunc;
use crate::module::Module;
use crate::value::{FuncType, GlobalType, MemoryType, Ref, TableType, TagType, Val};

/// One entry of the host-call frame snapshot (`StoreInner::host_frames`): enough to rebuild a
/// `FrameInfo` on demand for `WasmBacktrace::capture` from inside a host fn (#29d).
#[derive(Debug)]
pub(crate) struct HostFrame {
    pub instance: Instance,
    pub code: Arc<CompiledFunc>,
    pub ip: u32,
}

/// One externref-arena entry: a host payload, or an *internalized* `anyref` produced by
/// `extern.convert_any` (carrying that ref's handle so `any.convert_extern` recovers it).
pub(crate) enum ExternEntry {
    Host(Box<dyn Any + Send + Sync>),
    Internal(u32),
}

/// Store-side arena of `externref` entries. Grow-only for now: entries live for the store's
/// lifetime (no reclamation until a tracing collector, whose host-root enumeration hook lands
/// then). `Box<dyn Any>` isn't `Debug`, hence the manual impl.
#[derive(Default)]
pub(crate) struct ExternRefs(pub Vec<ExternEntry>);

impl core::fmt::Debug for ExternRefs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExternRefs")
            .field("len", &self.0.len())
            .finish()
    }
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
/// `Rooted<ExnRef>` indexes the store's exn arena. The `args` are **GC roots** the future tracing
/// collector (#27g) must enumerate (inert under the null collector; freed on `Store` drop).
#[derive(Debug)]
pub(crate) struct ExnEntity {
    pub tag: Tag,
    pub args: Vec<Val>,
    /// Backtrace captured at the original throw site (#29d). Carried on the instance so a
    /// `throw_ref` rethrow preserves the original site; `None` for host-created or when
    /// `wasm_backtrace` is off.
    pub backtrace: Option<crate::backtrace::WasmBacktrace>,
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
        host_index: u32,
    },
    /// An async host function: signature + index into the store's `async_host_funcs`.
    /// The closure returns a future the async driver awaits before resuming.
    #[cfg(feature = "async")]
    HostAsync {
        ty: FuncType,
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
    pub(crate) fn new(ty: MemoryType) -> Self {
        let bytes = vec![0; ty.minimum() as usize * PAGE_SIZE];
        MemoryEntity { bytes, ty }
    }

    pub(crate) fn size_pages(&self) -> u64 {
        (self.bytes.len() / PAGE_SIZE) as u64
    }

    /// Grows by `delta` pages; returns the previous page count, or `None` if it
    /// would exceed the declared/architectural maximum.
    pub(crate) fn grow(&mut self, delta: u64) -> Option<u64> {
        let old = self.size_pages();
        let new = old.checked_add(delta)?;
        let max = self.ty.maximum().unwrap_or(MAX_PAGES).min(MAX_PAGES);
        if new > max {
            return None;
        }
        self.bytes.resize(new as usize * PAGE_SIZE, 0);
        Some(old)
    }
}

impl TableEntity {
    pub(crate) fn new(ty: TableType, init: Ref) -> Self {
        let elems = vec![init; ty.minimum() as usize];
        TableEntity { elems, ty }
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

    /// Grows by `delta` elements (filled with `init`); returns the previous size,
    /// or `None` if it would exceed the declared maximum.
    pub(crate) fn grow(&mut self, delta: u64, init: Ref) -> Option<u64> {
        let old = self.size();
        let new = old.checked_add(delta)?;
        let max = self.ty.maximum().unwrap_or(u64::from(u32::MAX));
        if new > max {
            return None;
        }
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
