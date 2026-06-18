//! `Instance` — an instantiated module: its resolved index spaces plus the
//! export-lookup API. Instantiation itself lives in [`init`].

mod init;
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
        check_limits(&mut store, module)?;
        let instance = init::instantiate(store.as_context_mut().into_inner_mut(), module, imports)?;
        // The start function runs before any export is callable; a trap aborts
        // instantiation. Routed through `Func::call` so it handles wasm/host starts.
        if let Some(start_idx) = module.inner().start {
            let func = store.as_context_mut().inner().instance(instance).funcs[start_idx as usize];
            func.call(&mut store, &[], &mut [])?;
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
        })
    }
}
