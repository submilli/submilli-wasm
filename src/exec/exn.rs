//! `throw` / `throw_ref`: raise an exception, plus the in-frame unwinder (#28e) and the embedder
//! boundary (#28g). A raised exception travels as an internal [`PendingException`] `Err`; the run
//! loop intercepts it for handler search, and at the embedder boundary it becomes the public
//! [`ThrownException`] with the `exnref` parked on the store's pending slot.

use crate::exception::ThrownException;
use crate::extern_::Tag;
use crate::instance::Instance;
use crate::module::handler::HandlerRec;
use crate::module::op::CompiledFunc;
use crate::store::{ExnEntity, StoreInner};
use crate::trap::Trap;
use crate::value::{ExnRef, Rooted, Val};
use crate::{Error, Result};

use super::outcome::StepOutcome;
use super::Execution;

/// An exception in flight: a handle to the store's exception instance, carried inside `crate::Error`
/// as it unwinds. Internal — at the embedder boundary it becomes [`ThrownException`].
#[derive(Debug)]
pub(crate) struct PendingException {
    pub exn: Rooted<ExnRef>,
}

impl core::fmt::Display for PendingException {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("uncaught exception")
    }
}

impl std::error::Error for PendingException {}

/// Maps an error reaching the embedder boundary: an in-flight exception parks its `exnref` on the
/// store and becomes the public [`ThrownException`] (carrying the throw-site backtrace captured on
/// the instance, #29d); any other error (a trap, etc.) passes through.
pub(super) fn surface_exception(inner: &mut StoreInner, err: Error) -> Error {
    match err.downcast_ref::<PendingException>() {
        Some(p) => {
            let exn = p.exn;
            let backtrace = inner.exn(exn).backtrace.clone();
            inner.set_pending_exception(exn);
            // Backtrace as source, `ThrownException` as context: `Display` stays the exception
            // message while `downcast_ref::<WasmBacktrace>()` recovers the throw-site trace.
            match backtrace {
                Some(bt) => Error::new(bt).context(ThrownException),
                None => ThrownException.into(),
            }
        }
        None => err,
    }
}

impl Execution {
    /// `throw $tag`: pop the tag's arguments, capture the throw-site backtrace, allocate an
    /// exception instance carrying it, and raise it. `ip` is the throwing op's index (#29d).
    pub(super) fn throw(
        &mut self,
        inner: &mut StoreInner,
        instance: Instance,
        tag_idx: u32,
        ip: u32,
    ) -> Result<StepOutcome> {
        let tag = inner.instance(instance).tags[tag_idx as usize];
        let n = inner.tag(tag).ty.ty().params().len();
        let mut args: Vec<Val> = (0..n).map(|_| self.pop()).collect();
        args.reverse(); // popped top-first; restore declaration order
        let backtrace = inner
            .engine()
            .wasm_backtrace_enabled()
            .then(|| self.capture_backtrace(inner, ip));
        let exn = inner.alloc_exn(ExnEntity {
            tag,
            args,
            backtrace,
        });
        Err(PendingException { exn }.into())
    }

    /// `throw_ref`: re-raise a caught `exnref` (null traps).
    pub(super) fn throw_ref(&mut self) -> Result<StepOutcome> {
        match self.pop() {
            Val::ExnRef(Some(exn)) => Err(PendingException { exn }.into()),
            Val::ExnRef(None) => Err(Trap::NullReference.into()),
            _ => unreachable!("throw_ref operand is an exnref (validated)"),
        }
    }

    /// Intercept a raised exception (#28e): search frames outward for a `try_table` whose body
    /// contains the fault site and whose tag matches. On a match, restore the operand stack, push
    /// the clause payload, and point the frame at the landing pad (`Ok` — the run loop reloads and
    /// runs the landing-pad `Br`). A non-exception error, or no handler in any frame, propagates
    /// (`Err`) — uncaught exceptions surface to the embedder. `fault_ip` is the throw-site ip in the
    /// top frame; for unwound-into callers it is the call-site ip (`frame.ip` is the return address).
    pub(super) fn unwind(
        &mut self,
        inner: &mut StoreInner,
        err: Error,
        mut fault_ip: u32,
    ) -> Result<()> {
        let exn = match err.downcast_ref::<PendingException>() {
            Some(p) => p.exn,
            None => return Err(err),
        };
        let thrown = inner.exn(exn).tag;
        loop {
            let frame = self.frames.last().expect("unwind: empty frame stack");
            let (code, base, instance) = (frame.code.clone(), frame.locals_base, frame.instance);
            if let Some(rec) = find_clause(&code, fault_ip, inner, instance, thrown) {
                let floor = base + code.n_params + code.local_types.len() as u32;
                self.values.truncate((floor + rec.restore_height) as usize);
                if rec.payload_args {
                    for a in inner.exn(exn).args.clone() {
                        self.push(a);
                    }
                }
                if rec.payload_ref {
                    self.push(Val::ExnRef(Some(exn)));
                }
                self.frames.last_mut().expect("frame").ip = rec.landing_ip;
                return Ok(());
            }
            self.values.truncate(base as usize);
            self.frames.pop();
            match self.frames.last() {
                Some(f) => fault_ip = f.ip.saturating_sub(1),
                None => return Err(err),
            }
        }
    }

    /// A host function threw `exn` (via `Store::throw`): re-enter the unwinder from the host call
    /// site (the top frame is suspended there, `ip` = the return address) so the guest's `try_table`
    /// can catch it. `Ok` ⇒ caught (resume at the landing pad); `Err` ⇒ uncaught (→ embedder).
    pub(super) fn raise_host_exception(
        &mut self,
        inner: &mut StoreInner,
        exn: Rooted<ExnRef>,
    ) -> Result<()> {
        let fault_ip = self
            .frames
            .last()
            .expect("host call frame")
            .ip
            .saturating_sub(1);
        // A host-thrown exception has no backtrace yet; capture the wasm stack at the host-call
        // site so an uncaught host exception still reports a backtrace (#29d).
        if inner.exn(exn).backtrace.is_none() && inner.engine().wasm_backtrace_enabled() {
            let bt = self.capture_backtrace(inner, fault_ip);
            inner.exn_mut(exn).backtrace = Some(bt);
        }
        self.unwind(inner, PendingException { exn }.into(), fault_ip)
    }
}

/// The first catch clause (innermost span, source order) whose tag matches the thrown exception.
fn find_clause(
    code: &CompiledFunc,
    ip: u32,
    inner: &StoreInner,
    instance: Instance,
    thrown: Tag,
) -> Option<HandlerRec> {
    for span in &code.handlers {
        if ip >= span.start_ip && ip < span.end_ip {
            for rec in &span.clauses {
                let matches = match rec.tag {
                    None => true,
                    Some(idx) => inner.instance(instance).tags[idx as usize].index == thrown.index,
                };
                if matches {
                    return Some(*rec);
                }
            }
        }
    }
    None
}
