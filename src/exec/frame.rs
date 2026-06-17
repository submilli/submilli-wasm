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
}
