//! The generic execution driver: runs the (non-generic) interpreter core and
//! services host-function suspensions, which need the typed `Store<T>` to build a
//! `Caller<'_, T>`. Keeping this thin and `T`-generic isolates the data type from
//! the interpreter loop. See ARCHITECTURE §7/§10.

use std::sync::Arc;

use super::{Execution, Outcome};
use crate::extern_::Memory;
use crate::func::{Caller, Func};
use crate::instance::Instance;
use crate::module::op::CompiledFunc;
use crate::store::{FuncEntity, Store, UpdateDeadline};
use crate::trap::Trap;
use crate::value::Val;
use crate::Result;

/// Runs `code` (of `instance`) with `args`, servicing host calls, and returns the
/// results. The wasm core runs on `&mut store.inner`; only host calls touch `T`.
pub(crate) fn execute<T>(
    store: &mut Store<T>,
    instance: Instance,
    code: Arc<CompiledFunc>,
    args: Vec<Val>,
) -> Result<Vec<Val>> {
    let mut exec = Execution {
        values: args,
        frames: Vec::new(),
    };
    exec.push_call(instance, code);
    loop {
        match exec.run(&mut store.inner)? {
            Outcome::Finished => return Ok(exec.values),
            Outcome::HostCall { func, instance } => exec.invoke_host(store, func, instance)?,
            Outcome::EpochDeadline => apply_epoch_deadline(store)?,
            Outcome::Grow { memory, delta } => exec.do_grow(store, memory, delta)?,
        }
    }
}

/// Applies the store's epoch-deadline policy: trap (no callback) or invoke the
/// callback and act on its `UpdateDeadline` (extend-and-continue, or trap).
fn apply_epoch_deadline<T>(store: &mut Store<T>) -> Result<()> {
    let mut cb = store.epoch_callback.take();
    let action = match cb.as_mut() {
        Some(f) => f(store.as_context_mut())?,
        None => UpdateDeadline::Interrupt,
    };
    store.epoch_callback = cb;
    match action {
        UpdateDeadline::Interrupt => Err(Trap::Interrupt.into()),
        UpdateDeadline::Continue(delta) => {
            let next = store.inner.engine().current_epoch().saturating_add(delta);
            store.inner.set_epoch_deadline(next);
            Ok(())
        }
        #[cfg(feature = "async")]
        UpdateDeadline::Yield(_) => Err(Trap::Interrupt.into()),
    }
}

impl Execution {
    /// Invokes a suspended host function: pops its args off the operand stack,
    /// runs the closure with a `Caller`, and pushes the results back. A host `Err`
    /// propagates as the call's trap/error.
    fn invoke_host<T>(
        &mut self,
        store: &mut Store<T>,
        func: Func,
        instance: Instance,
    ) -> Result<()> {
        let (n_params, mut results, host_index) = match store.inner.func(func) {
            FuncEntity::Host { ty, host_index } => (
                ty.params().len(),
                ty.results()
                    .map(|t| Val::default_for(&t))
                    .collect::<Vec<_>>(),
                *host_index,
            ),
            FuncEntity::Wasm { .. } => unreachable!("run only suspends on host funcs"),
        };
        let params = self.values.split_off(self.values.len() - n_params);
        let cb = store.host_funcs[host_index as usize].clone();
        cb(
            Caller::new(store.as_context_mut(), Some(instance)),
            &params,
            &mut results,
        )?;
        self.values.extend(results);
        Ok(())
    }

    /// Services a suspended `memory.grow`: consults the limiter and pushes the new
    /// page count, or `-1` on a soft failure (a trap propagates from `grow_memory`).
    fn do_grow<T>(&mut self, store: &mut Store<T>, memory: Memory, delta: u64) -> Result<()> {
        let result = match store.grow_memory(memory, delta)? {
            Some(old) => old as i32,
            None => -1,
        };
        self.push(Val::I32(result));
        Ok(())
    }
}
