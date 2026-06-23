//! Call frames. One per active wasm call; the function's code is held as an
//! `Arc<CompiledFunc>` so the run loop can read ops without borrowing the store
//! or the value/frame stacks.

use std::sync::Arc;

use crate::instance::Instance;
use crate::module::op::CompiledFunc;

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
