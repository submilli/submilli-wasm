//! `Instance` — an instantiated module: its resolved index spaces plus the
//! export-lookup API. Instantiation itself lives in [`init`].

pub(crate) mod const_eval;
pub(crate) mod init;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use crate::extern_::{Extern, Global, Memory, Table};
use crate::func::{Func, TypedFunc, WasmParams, WasmResults};
use crate::module::inner::ExportKind;
use crate::module::Module;
use crate::store::{AsContextMut, StoreInner};
use crate::{Error, Result};

/// Rejects instantiation if it would push the store past the limiter's entity caps.
fn check_limits<T: 'static>(
    store: &mut impl AsContextMut<Data = T>,
    module: &Module,
) -> Result<()> {
    let mut ctx = store.as_context_mut();
    let s = ctx.store_mut();
    let m = module.inner();
    if let Some(max) = s.limiter_instances() {
        if s.inner.instance_count() + 1 > max {
            return Err(Error::msg("instance count exceeds the store limit"));
        }
    }
    if let Some(max) = s.limiter_memories() {
        if s.inner.memory_count() + m.memories.len() > max {
            return Err(Error::msg("memory count exceeds the store limit"));
        }
    }
    if let Some(max) = s.limiter_tables() {
        if s.inner.table_count() + m.tables.len() > max {
            return Err(Error::msg("table count exceeds the store limit"));
        }
    }
    Ok(())
}

/// Consults the limiter for each defined memory/table's *initial* size before instantiation
/// allocates anything (wasmtime consults the limiter at memory/table creation, not only on grow).
/// With no limiter installed, the finite default ceiling is the bound (`src/store/grow.rs`). A
/// denial fails instantiation cleanly — nothing is allocated. (Imported memories/tables were
/// already gated when first created.)
fn check_initial_sizes<T: 'static>(
    store: &mut impl AsContextMut<Data = T>,
    module: &Module,
) -> Result<()> {
    let mut ctx = store.as_context_mut();
    let s = ctx.store_mut();
    let m = module.inner();
    for mt in &m.memories {
        if !s.limiter_allows_memory(mt.minimum(), mt.maximum())? {
            return Err(Error::msg("memory minimum size exceeds the store limit"));
        }
    }
    for td in &m.tables {
        if !s.limiter_allows_table(td.ty.min, td.ty.max)? {
            return Err(Error::msg("table minimum size exceeds the store limit"));
        }
    }
    Ok(())
}

/// Async sibling of [`check_initial_sizes`]: awaits an async resource limiter.
#[cfg(feature = "async")]
async fn check_initial_sizes_async<T: 'static>(
    store: &mut impl AsContextMut<Data = T>,
    module: &Module,
) -> Result<()> {
    let mut ctx = store.as_context_mut();
    let s = ctx.store_mut();
    let m = module.inner();
    for mt in &m.memories {
        if !s
            .limiter_allows_memory_async(mt.minimum(), mt.maximum())
            .await?
        {
            return Err(Error::msg("memory minimum size exceeds the store limit"));
        }
    }
    for td in &m.tables {
        if !s.limiter_allows_table_async(td.ty.min, td.ty.max).await? {
            return Err(Error::msg("table minimum size exceeds the store limit"));
        }
    }
    Ok(())
}

/// Shared instantiation core: enforces limits, builds the instance, and resolves the
/// optional `start` function — leaving the caller to run it sync ([`Instance::new`]) or
/// async ([`Instance::new_async`]).
fn instantiate_resolve_start(
    store: &mut impl AsContextMut,
    module: &Module,
    imports: &[Extern],
) -> Result<(Instance, Option<Func>)> {
    check_limits(store, module)?;
    let instance = init::instantiate(store.as_context_mut().into_inner_mut(), module, imports)?;
    let start = module
        .inner()
        .start
        .map(|idx| store.as_context_mut().inner().instance(instance).funcs[idx as usize]);
    Ok((instance, start))
}

/// An instantiated WebAssembly module. Lightweight, store-bound handle.
#[derive(Copy, Clone, Debug)]
pub struct Instance {
    pub(crate) index: u32,
}

impl Instance {
    pub fn new(
        mut store: impl AsContextMut,
        module: &Module,
        imports: &[Extern],
    ) -> Result<Instance> {
        // Sync instantiation is permitted on an async-enabled store (no fibers here). A `start`
        // that calls an async host fn still errors via the sync driver's "synchronous context".
        check_initial_sizes(&mut store, module)?;
        let (instance, start) = instantiate_resolve_start(&mut store, module, imports)?;
        // The start function runs before any export is callable; a trap aborts
        // instantiation. Routed through `Func::call` so it handles wasm/host starts.
        if let Some(func) = start {
            func.call(&mut store, &[], &mut [])?;
        }
        Ok(instance)
    }

    /// Async sibling of [`new`](Instance::new): runs the `start` function as a `Future`
    /// (so an async host fn it calls can suspend). Requires an async store.
    #[cfg(feature = "async")]
    pub async fn new_async(
        mut store: impl AsContextMut,
        module: &Module,
        imports: &[Extern],
    ) -> Result<Instance> {
        if !store.as_context().engine().is_async() {
            return Err(Error::msg(
                "cannot use `new_async` without `Config::async_support(true)`",
            ));
        }
        check_initial_sizes_async(&mut store, module).await?;
        let (instance, start) = instantiate_resolve_start(&mut store, module, imports)?;
        if let Some(func) = start {
            func.call_async(&mut store, &[], &mut []).await?;
        }
        Ok(instance)
    }

    pub fn get_func(&self, mut store: impl AsContextMut, name: &str) -> Option<Func> {
        match self.export(store.as_context_mut().inner(), name)? {
            Extern::Func(f) => Some(f),
            _ => None,
        }
    }

    pub fn get_typed_func<Params, Results>(
        &self,
        mut store: impl AsContextMut,
        name: &str,
    ) -> Result<TypedFunc<Params, Results>>
    where
        Params: WasmParams,
        Results: WasmResults,
    {
        let func = self
            .get_func(&mut store, name)
            .ok_or_else(|| crate::Error::msg(format!("no function export `{name}`")))?;
        func.typed(&store)
    }

    pub fn get_memory(&self, mut store: impl AsContextMut, name: &str) -> Option<Memory> {
        match self.export(store.as_context_mut().inner(), name)? {
            Extern::Memory(m) => Some(m),
            _ => None,
        }
    }

    pub fn get_global(&self, mut store: impl AsContextMut, name: &str) -> Option<Global> {
        match self.export(store.as_context_mut().inner(), name)? {
            Extern::Global(g) => Some(g),
            _ => None,
        }
    }

    pub fn get_table(&self, mut store: impl AsContextMut, name: &str) -> Option<Table> {
        match self.export(store.as_context_mut().inner(), name)? {
            Extern::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn get_export(&self, mut store: impl AsContextMut, name: &str) -> Option<Extern> {
        self.export(store.as_context_mut().inner(), name)
    }

    /// Resolves a named export to its store handle via the instance's index spaces.
    pub(crate) fn export(self, inner: &StoreInner, name: &str) -> Option<Extern> {
        let entity = inner.instance(self);
        let export = entity
            .module
            .inner()
            .exports
            .iter()
            .find(|e| e.name == name)?;
        Some(match export.kind {
            ExportKind::Func(i) => Extern::Func(entity.funcs[i as usize]),
            ExportKind::Table(i) => Extern::Table(entity.tables[i as usize]),
            ExportKind::Memory(i) => Extern::Memory(entity.memories[i as usize]),
            ExportKind::Global(i) => Extern::Global(entity.globals[i as usize]),
            ExportKind::Tag(i) => Extern::Tag(entity.tags[i as usize]),
        })
    }
}
