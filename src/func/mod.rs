//! `Func`, `TypedFunc`, `Caller`, and the host-function traits.

#[cfg(feature = "async")]
mod async_func;
mod into_func;
mod wasm_ty;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(feature = "async")]
pub(crate) use into_func::into_async_func;
pub use into_func::IntoFunc;
pub use wasm_ty::{WasmParams, WasmResults, WasmRet, WasmTy};

use core::marker::PhantomData;
use std::sync::Arc;

use crate::engine::Engine;
use crate::extern_::Extern;
use crate::instance::Instance;
use crate::store::{
    AsContext, AsContextMut, FuncEntity, StoreContext, StoreContextMut, StoreInner,
};
use crate::value::{FuncType, Val};
use crate::Result;

/// A reference to a callable WebAssembly function. Lightweight, store-bound handle.
#[derive(Copy, Clone, Debug)]
pub struct Func {
    pub(crate) index: u32,
    /// The store this handle was minted by (#34); checked on access to reject cross-store misuse.
    /// `0` = **unchecked** — a funcref *value* rebuilt from a bare operand-cell index by
    /// [`from_raw`](Self::from_raw), which is same-store by construction (the cell encoding can't
    /// carry the store id). The embedder's named `Func` handles (from `Instance::get_func`) are
    /// minted with a real id and fully checked.
    pub(crate) store: u64,
}

impl Func {
    /// Wraps a raw store-arena index (the funcref handle stored in tables / GC bodies). The handle is
    /// **unchecked** (`store: 0`) — a funcref value is same-store by construction (#34).
    pub(crate) fn from_raw(index: u32) -> Self {
        Func { index, store: 0 }
    }

    /// The raw store-arena index behind this funcref handle.
    pub(crate) fn raw(self) -> u32 {
        self.index
    }
}

/// The resolved kind of a callee, with the entity's Copy fields extracted.
enum Callee {
    Wasm(Instance, u32),
    Host(u32),
    #[cfg(feature = "async")]
    HostAsync(u32),
}

impl Func {
    /// Creates a host function with a dynamic signature.
    pub fn new<T: 'static>(
        mut store: impl AsContextMut<Data = T>,
        ty: FuncType,
        func: impl Fn(Caller<'_, T>, &[Val], &mut [Val]) -> Result<()> + Send + Sync + 'static,
    ) -> Func {
        let mut ctx = store.as_context_mut();
        let host_index = ctx.store_mut().push_host_func(Arc::new(func));
        ctx.inner_mut().alloc_func(FuncEntity::Host {
            sig: crate::store::HostSig::new(&ty),
            ty,
            host_index,
        })
    }

    /// Creates a host function from a typed Rust closure.
    pub fn wrap<T, Params, Results>(
        mut store: impl AsContextMut<Data = T>,
        func: impl IntoFunc<T, Params, Results>,
    ) -> Func
    where
        T: 'static,
    {
        let engine = store.as_context().engine().clone();
        let (ty, cb) = func.into_func(&engine);
        let mut ctx = store.as_context_mut();
        let host_index = ctx.store_mut().push_host_func(cb);
        ctx.inner_mut().alloc_func(FuncEntity::Host {
            sig: crate::store::HostSig::new(&ty),
            ty,
            host_index,
        })
    }

    /// Calls this function with dynamically-typed arguments.
    pub fn call(
        &self,
        mut store: impl AsContextMut,
        params: &[Val],
        results: &mut [Val],
    ) -> Result<()> {
        // Sync `call` is permitted even on an async-enabled store: this interpreter has no
        // fibers, so the sync driver runs fine. (If the call reaches an async host fn, the sync
        // driver still errors with "synchronous context" — the real constraint.)
        let ty = self.ty(&store);
        check_args(params, &ty)?;
        if results.len() != ty.results().len() {
            return Err(crate::Error::msg("wrong number of results"));
        }
        // Copy out the entity's Copy fields, releasing the borrow before we
        // re-borrow the store mutably for the call.
        let kind = self.resolve_callee(store.as_context().inner());
        let out = match kind {
            Callee::Wasm(instance, func_index) => {
                let mut ctx = store.as_context_mut();
                let code = ctx.inner().instance(instance).module.code(func_index);
                let result_tys: Vec<crate::value::ValType> = ty.results().collect();
                let args = crate::extern_::coerce_args(&mut ctx.store_mut().inner, params, &ty)?;
                crate::exec::host::execute(
                    ctx.store_mut(),
                    instance,
                    func_index,
                    code,
                    args,
                    &result_tys,
                )?
            }
            Callee::Host(host_index) => {
                let cb = store.as_context_mut().store_mut().host_funcs[host_index as usize].clone();
                let mut out = default_results(&ty);
                // Contain a host-fn panic (#33): clear any pending exception it set via `Store::throw`
                // before re-raising, so this store is left consistent for reuse (wasmtime parity).
                match crate::exec::guard::catch_host(|| {
                    cb(Caller::new(store.as_context_mut(), None), params, &mut out)
                }) {
                    Ok(result) => result?,
                    Err(payload) => {
                        store.as_context_mut().store_mut().take_pending_exception();
                        crate::exec::guard::reraise(payload);
                    }
                }
                out
            }
            #[cfg(feature = "async")]
            Callee::HostAsync(_) => {
                return Err(crate::Error::msg(
                    "cannot call an async host function synchronously; use `call_async`",
                ));
            }
        };
        results.clone_from_slice(&out);
        Ok(())
    }

    /// Extracts the callee's `Copy` fields, releasing the entity borrow before the call.
    fn resolve_callee(self, inner: &StoreInner) -> Callee {
        match inner.func(self) {
            FuncEntity::Wasm {
                instance,
                func_index,
            } => Callee::Wasm(*instance, *func_index),
            FuncEntity::Host { host_index, .. } => Callee::Host(*host_index),
            #[cfg(feature = "async")]
            FuncEntity::HostAsync { host_index, .. } => Callee::HostAsync(*host_index),
        }
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
        let ty = self.ty(&store);
        let params_match = ty.params().eq(wasm_ty::valtypes_of::<Params>());
        let results_match = ty.results().eq(wasm_ty::valtypes_of::<Results>());
        if params_match && results_match {
            Ok(TypedFunc {
                func: *self,
                _marker: PhantomData,
            })
        } else {
            Err(crate::Error::msg("typed function signature mismatch"))
        }
    }

    /// Returns this function's signature.
    pub fn ty(&self, store: impl AsContext) -> FuncType {
        let inner = store.as_context().inner();
        match inner.func(*self) {
            FuncEntity::Wasm {
                instance,
                func_index,
            } => inner
                .instance(*instance)
                .module
                .inner()
                .func_type(*func_index)
                .clone(),
            FuncEntity::Host { ty, .. } => ty.clone(),
            #[cfg(feature = "async")]
            FuncEntity::HostAsync { ty, .. } => ty.clone(),
        }
    }
}

/// A zero/default-initialized results buffer sized to `ty`'s results.
fn default_results(ty: &FuncType) -> Vec<Val> {
    ty.results().map(|t| Val::default_for_valtype(&t)).collect()
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
    pub fn call(&self, mut store: impl AsContextMut, params: Params) -> Result<Results> {
        let mut args = Vec::new();
        params.into_vals(&mut args);
        let mut results = vec![Val::I32(0); wasm_ty::valtypes_of::<Results>().len()];
        self.func.call(&mut store, &args, &mut results)?;
        Ok(Results::from_vals(&results))
    }

    /// The underlying untyped [`Func`].
    pub fn func(&self) -> &Func {
        &self.func
    }
}

/// Context passed to host functions, giving access to the caller's store and exports.
///
/// `instance` is the *calling* wasm instance (for `get_export`), or `None` when the
/// host function is invoked at the top level via [`Func::call`].
pub struct Caller<'a, T: 'static> {
    store: StoreContextMut<'a, T>,
    instance: Option<Instance>,
}

impl<'a, T: 'static> Caller<'a, T> {
    pub(crate) fn new(store: StoreContextMut<'a, T>, instance: Option<Instance>) -> Self {
        Caller { store, instance }
    }
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
        let instance = self.instance?;
        instance.export(self.store.inner(), name)
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
