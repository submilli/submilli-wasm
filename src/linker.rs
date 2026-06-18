//! `Linker<T>` — import resolution and multi-module linking.
//!
//! Holds a `(module, name)` map of definitions: either a concrete store-bound
//! [`Extern`] (from `define`/`instance`) or a deferred host function (from
//! `func_new`/`func_wrap`) materialized into a `Store` at `instantiate`/`get`
//! time. `func_*` take no store (matching wasmtime), which is why host defs are
//! stored lazily.

#[cfg(test)]
#[path = "linker_tests.rs"]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use crate::engine::Engine;
use crate::extern_::Extern;
use crate::func::{Caller, Func, IntoFunc};
use crate::instance::Instance;
use crate::module::Module;
use crate::store::{AsContext, AsContextMut, FuncEntity, HostFunc};
use crate::value::{FuncType, Val};
use crate::{Error, Result};

/// A linker definition: a concrete extern, or a host function to materialize lazily.
enum Def<T: 'static> {
    Extern(Extern),
    Host { ty: FuncType, cb: HostFunc<T> },
}

// Manual `Clone` so we don't require `T: Clone` (the derive would).
impl<T: 'static> Clone for Def<T> {
    fn clone(&self) -> Self {
        match self {
            Def::Extern(e) => Def::Extern(e.clone()),
            Def::Host { ty, cb } => Def::Host {
                ty: ty.clone(),
                cb: cb.clone(),
            },
        }
    }
}

/// Resolves module imports by `(module, name)` and links multiple modules together.
pub struct Linker<T: 'static> {
    engine: Engine,
    allow_shadowing: bool,
    allow_unknown_exports: bool,
    map: HashMap<(String, String), Def<T>>,
}

impl<T: 'static> core::fmt::Debug for Linker<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Linker").finish_non_exhaustive()
    }
}

impl<T: 'static> Linker<T> {
    pub fn new(engine: &Engine) -> Linker<T> {
        Linker {
            engine: engine.clone(),
            allow_shadowing: false,
            allow_unknown_exports: false,
            map: HashMap::new(),
        }
    }

    pub fn define(
        &mut self,
        _store: impl AsContext<Data = T>,
        module: &str,
        name: &str,
        item: impl Into<Extern>,
    ) -> Result<&mut Self> {
        self.insert(module, name, Def::Extern(item.into()))?;
        Ok(self)
    }

    pub fn define_name(
        &mut self,
        _store: impl AsContext<Data = T>,
        name: &str,
        item: impl Into<Extern>,
    ) -> Result<&mut Self> {
        self.insert("", name, Def::Extern(item.into()))?;
        Ok(self)
    }

    pub fn func_new(
        &mut self,
        module: &str,
        name: &str,
        ty: FuncType,
        func: impl Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<()> + Send + Sync + 'static,
    ) -> Result<&mut Self> {
        self.insert(
            module,
            name,
            Def::Host {
                ty,
                cb: Arc::new(func),
            },
        )?;
        Ok(self)
    }

    pub fn func_wrap<Params, Args>(
        &mut self,
        module: &str,
        name: &str,
        func: impl IntoFunc<T, Params, Args>,
    ) -> Result<&mut Self> {
        let (ty, cb) = func.into_func(&self.engine);
        self.insert(module, name, Def::Host { ty, cb })?;
        Ok(self)
    }

    pub fn instance(
        &mut self,
        store: impl AsContextMut<Data = T>,
        module_name: &str,
        instance: Instance,
    ) -> Result<&mut Self> {
        let pairs: Vec<(String, Extern)> = {
            let inner = store.as_context().inner();
            let names: Vec<String> = inner
                .instance(instance)
                .module
                .inner()
                .exports
                .iter()
                .map(|e| e.name.clone())
                .collect();
            names
                .into_iter()
                .filter_map(|n| instance.export(inner, &n).map(|e| (n, e)))
                .collect()
        };
        for (name, ext) in pairs {
            self.insert(module_name, &name, Def::Extern(ext))?;
        }
        Ok(self)
    }

    pub fn module(
        &mut self,
        mut store: impl AsContextMut<Data = T>,
        module_name: &str,
        module: &Module,
    ) -> Result<&mut Self> {
        let inst = self.instantiate(&mut store, module)?;
        self.instance(&mut store, module_name, inst)
    }

    pub fn instantiate(
        &self,
        mut store: impl AsContextMut<Data = T>,
        module: &Module,
    ) -> Result<Instance> {
        let mut imports = Vec::new();
        for imp in module.imports() {
            imports.push(self.resolve(&mut store, imp.module(), imp.name())?);
        }
        Instance::new(&mut store, module, &imports)
    }

    pub fn get(
        &self,
        mut store: impl AsContextMut<Data = T>,
        module: &str,
        name: &str,
    ) -> Result<Extern> {
        self.resolve(&mut store, module, name)
    }

    pub fn get_default(
        &self,
        mut store: impl AsContextMut<Data = T>,
        module: &str,
    ) -> Result<Func> {
        match self.resolve(&mut store, module, "")? {
            Extern::Func(f) => Ok(f),
            _ => Err(Error::msg("default export is not a function")),
        }
    }

    pub fn alias(
        &mut self,
        module: &str,
        name: &str,
        as_module: &str,
        as_name: &str,
    ) -> Result<&mut Self> {
        let def = self
            .map
            .get(&(module.to_string(), name.to_string()))
            .cloned()
            .ok_or_else(|| Error::msg(format!("no export `{module}::{name}` to alias")))?;
        self.insert(as_module, as_name, def)?;
        Ok(self)
    }

    pub fn alias_module(&mut self, module: &str, as_module: &str) -> Result<()> {
        let entries: Vec<(String, Def<T>)> = self
            .map
            .iter()
            .filter(|((m, _), _)| m == module)
            .map(|((_, n), d)| (n.clone(), d.clone()))
            .collect();
        for (name, def) in entries {
            self.insert(as_module, &name, def)?;
        }
        Ok(())
    }

    pub fn allow_shadowing(&mut self, allow: bool) -> &mut Self {
        self.allow_shadowing = allow;
        self
    }

    pub fn allow_unknown_exports(&mut self, allow: bool) -> &mut Self {
        self.allow_unknown_exports = allow;
        self
    }

    /// Inserts a definition, honoring `allow_shadowing`.
    fn insert(&mut self, module: &str, name: &str, def: Def<T>) -> Result<()> {
        let key = (module.to_string(), name.to_string());
        if !self.allow_shadowing && self.map.contains_key(&key) {
            return Err(Error::msg(format!(
                "import of `{module}::{name}` defined twice"
            )));
        }
        self.map.insert(key, def);
        Ok(())
    }

    /// Resolves `(module, name)` to an `Extern`, materializing a host def into `store`.
    fn resolve(
        &self,
        store: &mut impl AsContextMut<Data = T>,
        module: &str,
        name: &str,
    ) -> Result<Extern> {
        match self.map.get(&(module.to_string(), name.to_string())) {
            Some(Def::Extern(e)) => Ok(e.clone()),
            Some(Def::Host { ty, cb }) => {
                let mut ctx = store.as_context_mut();
                let host_index = ctx.store_mut().push_host_func(cb.clone());
                let f = ctx.inner_mut().alloc_func(FuncEntity::Host {
                    ty: ty.clone(),
                    host_index,
                });
                Ok(Extern::Func(f))
            }
            None => Err(Error::msg(format!("unknown import: `{module}::{name}`"))),
        }
    }
}
