//! `Linker<T>` — import resolution and multi-module linking.

use core::marker::PhantomData;

use crate::engine::Engine;
use crate::extern_::Extern;
use crate::func::{Caller, Func, IntoFunc};
use crate::instance::Instance;
use crate::module::Module;
use crate::store::{AsContext, AsContextMut};
use crate::value::{FuncType, Val};
use crate::Result;

/// Resolves module imports by `(module, name)` and links multiple modules together.
pub struct Linker<T> {
    engine: Engine,
    allow_shadowing: bool,
    allow_unknown_exports: bool,
    _marker: PhantomData<fn() -> T>,
}

impl<T> core::fmt::Debug for Linker<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Linker").finish_non_exhaustive()
    }
}

impl<T> Linker<T> {
    pub fn new(engine: &Engine) -> Linker<T> {
        Linker {
            engine: engine.clone(),
            allow_shadowing: false,
            allow_unknown_exports: false,
            _marker: PhantomData,
        }
    }

    pub fn define(
        &mut self,
        store: impl AsContext<Data = T>,
        module: &str,
        name: &str,
        item: impl Into<Extern>,
    ) -> Result<&mut Self>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn define_name(
        &mut self,
        store: impl AsContext<Data = T>,
        name: &str,
        item: impl Into<Extern>,
    ) -> Result<&mut Self>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn func_new(
        &mut self,
        module: &str,
        name: &str,
        ty: FuncType,
        func: impl Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<()> + Send + Sync + 'static,
    ) -> Result<&mut Self>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn func_wrap<Params, Args>(
        &mut self,
        module: &str,
        name: &str,
        func: impl IntoFunc<T, Params, Args>,
    ) -> Result<&mut Self>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn instance(
        &mut self,
        store: impl AsContextMut<Data = T>,
        module_name: &str,
        instance: Instance,
    ) -> Result<&mut Self>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn module(
        &mut self,
        store: impl AsContextMut<Data = T>,
        module_name: &str,
        module: &Module,
    ) -> Result<&mut Self>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn instantiate(
        &self,
        store: impl AsContextMut<Data = T>,
        module: &Module,
    ) -> Result<Instance>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn get(
        &self,
        store: impl AsContextMut<Data = T>,
        module: &str,
        name: &str,
    ) -> Result<Extern>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn get_default(&self, store: impl AsContextMut<Data = T>, module: &str) -> Result<Func>
    where
        T: 'static,
    {
        todo!()
    }

    pub fn alias(
        &mut self,
        module: &str,
        name: &str,
        as_module: &str,
        as_name: &str,
    ) -> Result<&mut Self> {
        todo!()
    }

    pub fn alias_module(&mut self, module: &str, as_module: &str) -> Result<()> {
        todo!()
    }

    pub fn allow_shadowing(&mut self, allow: bool) -> &mut Self {
        self.allow_shadowing = allow;
        self
    }

    pub fn allow_unknown_exports(&mut self, allow: bool) -> &mut Self {
        self.allow_unknown_exports = allow;
        self
    }
}
