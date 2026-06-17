//! `Func`, `TypedFunc`, `Caller`, and the host-function traits.

mod into_func;
mod wasm_ty;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use into_func::IntoFunc;
pub use wasm_ty::{WasmParams, WasmResults, WasmRet, WasmTy};

use core::marker::PhantomData;

use crate::engine::Engine;
use crate::extern_::Extern;
use crate::instance::Instance;
use crate::store::{AsContext, AsContextMut, StoreContext, StoreContextMut};
use crate::value::{FuncType, Val};
use crate::Result;

/// A reference to a callable WebAssembly function. Lightweight, store-bound handle.
#[derive(Copy, Clone, Debug)]
pub struct Func {
    pub(crate) index: u32,
}

impl Func {
    /// Creates a host function with a dynamic signature.
    pub fn new<T: 'static>(
        store: impl AsContextMut<Data = T>,
        ty: FuncType,
        func: impl Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<()> + Send + Sync + 'static,
    ) -> Func {
        todo!()
    }

    /// Creates a host function from a typed Rust closure.
    pub fn wrap<T, Params, Results>(
        store: impl AsContextMut<Data = T>,
        func: impl IntoFunc<T, Params, Results>,
    ) -> Func
    where
        T: 'static,
    {
        todo!()
    }

    /// Calls this function with dynamically-typed arguments.
    pub fn call(
        &self,
        mut store: impl AsContextMut,
        params: &[Val],
        results: &mut [Val],
    ) -> Result<()> {
        let inner = store.as_context_mut().into_inner_mut();
        let fe = inner.func(*self);
        let (def_inst, func_index) = (fe.instance, fe.func_index);
        let module = inner.instance(def_inst).module.clone();
        let ty = module.inner().func_type(func_index).clone();

        check_args(params, &ty)?;
        if results.len() != ty.results().len() {
            return Err(crate::Error::msg("wrong number of results"));
        }

        let code = module.inner().compiled(func_index);
        let out = crate::exec::execute(inner, def_inst, code, params.to_vec())?;
        results.clone_from_slice(&out);
        Ok(())
    }

    /// Obtains a statically-typed handle to this function.
    pub fn typed<Params, Results>(
        &self,
        store: impl AsContext,
    ) -> Result<TypedFunc<Params, Results>>
    where
        Params: WasmParams,
        Results: WasmResults,
    {
        todo!()
    }

    /// Returns this function's signature.
    pub fn ty(&self, store: impl AsContext) -> FuncType {
        let inner = store.as_context().inner();
        let fe = inner.func(*self);
        inner
            .instance(fe.instance)
            .module
            .inner()
            .func_type(fe.func_index)
            .clone()
    }
}

/// Checks the argument count and per-value types against `ty`'s parameters.
fn check_args(params: &[Val], ty: &FuncType) -> Result<()> {
    if params.len() != ty.params().len() {
        return Err(crate::Error::msg("wrong number of arguments"));
    }
    for (val, pty) in params.iter().zip(ty.params()) {
        if !crate::extern_::val_matches(val, &pty) {
            return Err(crate::Error::msg("argument type mismatch"));
        }
    }
    Ok(())
}

/// A statically-typed view of a [`Func`].
pub struct TypedFunc<Params, Results> {
    func: Func,
    _marker: PhantomData<fn(Params) -> Results>,
}

impl<Params, Results> Copy for TypedFunc<Params, Results> {}

impl<Params, Results> Clone for TypedFunc<Params, Results> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Params, Results> core::fmt::Debug for TypedFunc<Params, Results> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TypedFunc").finish_non_exhaustive()
    }
}

impl<Params, Results> TypedFunc<Params, Results>
where
    Params: WasmParams,
    Results: WasmResults,
{
    /// Calls this function with statically-typed arguments.
    pub fn call(&self, store: impl AsContextMut, params: Params) -> Result<Results> {
        todo!()
    }

    /// The underlying untyped [`Func`].
    pub fn func(&self) -> &Func {
        &self.func
    }
}

/// Context passed to host functions, giving access to the caller's store and exports.
pub struct Caller<'a, T: 'static> {
    store: StoreContextMut<'a, T>,
    instance: Instance,
}

impl<T: 'static> core::fmt::Debug for Caller<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Caller").finish_non_exhaustive()
    }
}

impl<T: 'static> Caller<'_, T> {
    pub fn data(&self) -> &T {
        self.store.data()
    }

    pub fn data_mut(&mut self) -> &mut T {
        self.store.data_mut()
    }

    pub fn get_export(&mut self, name: &str) -> Option<Extern> {
        todo!()
    }

    pub fn engine(&self) -> &Engine {
        self.store.engine()
    }
}

impl<T: 'static> AsContext for Caller<'_, T> {
    type Data = T;

    fn as_context(&self) -> StoreContext<'_, T> {
        self.store.as_context()
    }
}

impl<T: 'static> AsContextMut for Caller<'_, T> {
    fn as_context_mut(&mut self) -> StoreContextMut<'_, T> {
        self.store.as_context_mut()
    }
}
