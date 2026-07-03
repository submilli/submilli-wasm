//! Call frames. One per active wasm call; the function's code is held as an
//! `Arc<CompiledFunc>` so the run loop can read ops without borrowing the store
//! or the value/frame stacks.

use std::sync::Arc;

use crate::instance::Instance;
use crate::module::op::{BranchTarget, CompiledFunc};

#[derive(Debug)]
pub(crate) struct Frame {
    pub code: Arc<CompiledFunc>,
    /// Resume point in `code.ops` (saved when this frame makes a call).
    pub ip: u32,
    /// Index into `Execution.values` where this frame's locals begin.
    pub locals_base: u32,
    /// The instance this frame executes in (resolves globals/memories/callees).
    pub instance: Instance,
    /// This function's index in its defining module — for backtraces (#29e), avoiding a
    /// pointer-scan over `module.functions` at capture time.
    pub func_index: u32,
    /// A boundary marker, not an executable frame: it separates one (sub-)call's frames from the
    /// parked outer call's on the single shared operand/frame stack. The run loop never executes
    /// one (`stop_depth` stops the call above it); its `code`/`instance` are inert filler.
    pub delimiter: Option<Delimiter>,
}

/// What a delimiter frame marks. `HostReentry` is a host→wasm re-entry (`Func::call` from a host
/// function) — rendered as a host-boundary marker in backtraces; `TopLevel` is the embedder→wasm
/// entry at the bottom of the stack, which carries no backtrace marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delimiter {
    TopLevel,
    HostReentry,
}

impl super::Execution {
    pub(super) fn push_call(
        &mut self,
        instance: Instance,
        func_index: u32,
        code: Arc<CompiledFunc>,
    ) {
        let locals_base = self.values.len() as u32 - code.n_params;
        for ty in &code.local_types {
            self.push_default(ty);
        }
        self.frames.push(Frame {
            code,
            ip: 0,
            locals_base,
            instance,
            func_index,
            delimiter: None,
        });
    }

    /// Pushes a [`Delimiter`] boundary marker (no operands, inert `code`/`instance` filler). The
    /// next `push_call` lays the entered function's frame directly above it; `run`/`unwind` stop at
    /// this frame's depth so the call below it stays parked and untouched.
    pub(super) fn push_delimiter(
        &mut self,
        kind: Delimiter,
        instance: Instance,
        code: Arc<CompiledFunc>,
    ) {
        let locals_base = self.values.len() as u32;
        self.frames.push(Frame {
            code,
            ip: 0,
            locals_base,
            instance,
            func_index: 0,
            delimiter: Some(kind),
        });
    }

    /// Moves the top `keep` operands down over `pop` discarded ones, then jumps.
    pub(super) fn take_branch(&mut self, t: &BranchTarget) {
        if t.pop == 0 {
            return; // nothing discarded — the kept operands are already in place
        }
        let len = self.values.len();
        let keep = t.keep as usize;
        let src = len - keep;
        let dst = src - t.pop as usize;
        self.values.copy_within(src..len, dst);
        self.values.truncate(dst + keep);
        // The root shadow moves in lockstep with the cell stack (same offsets/length).
        self.shadow.copy_within(src..len, dst);
        self.shadow.truncate(dst + keep);
    }

    pub(super) fn top(&self) -> (Arc<CompiledFunc>, u32, u32, Instance) {
        let f = self.frames.last().expect("current frame");
        (f.code.clone(), f.ip, f.locals_base, f.instance)
    }

    /// Pops the current frame, moving its top `n_results` operands down to the
    /// frame base. Returns true if the frame stack has fallen back to `stop_depth`
    /// (this call's boundary) — i.e. the call this `run` was driving has finished.
    pub(super) fn do_return(&mut self, n_results: u32, stop_depth: usize) -> bool {
        let frame = self.frames.pop().expect("frame stack underflow");
        let n = n_results as usize;
        let len = self.values.len();
        let dst = frame.locals_base as usize;
        self.values.copy_within(len - n..len, dst);
        self.values.truncate(dst + n);
        self.shadow.copy_within(len - n..len, dst);
        self.shadow.truncate(dst + n);
        self.frames.len() == stop_depth
    }
}
