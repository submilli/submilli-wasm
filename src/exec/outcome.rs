//! Control-flow results threaded through the interpreter: why [`run`](super::Execution::run)
//! suspended ([`Outcome`]), what one [`step`](super::Execution::step) decided
//! ([`StepOutcome`]), and a resolved call target ([`ResolvedCall`]/[`CallReq`]).

use std::sync::Arc;

use crate::extern_::{Memory, Table};
use crate::func::Func;
use crate::instance::Instance;
use crate::module::op::CompiledFunc;
use crate::value::Ref;

/// Why [`Execution::run`](super::Execution::run) returned: either the call finished or it
/// suspended on something the (generic) driver in [`host`](super::host) must service.
pub(crate) enum Outcome {
    Finished,
    HostCall {
        func: Func,
        instance: Instance,
    },
    /// Suspended on an async host function; only the async driver can service it.
    #[cfg(feature = "async")]
    HostAsync {
        func: Func,
        instance: Instance,
    },
    /// The active fuel slice was exhausted with reserve remaining: yield to the
    /// executor, refuel, and resume. Only produced under an async fuel-yield interval.
    #[cfg(feature = "async")]
    FuelYield,
    EpochDeadline,
    Grow {
        memory: Memory,
        delta: u64,
    },
    /// In-wasm `table.grow`: routed through the (generic) driver so the limiter is consulted.
    TableGrow {
        table: Table,
        delta: u64,
        init: Ref,
    },
}

/// What a single [`step`](super::Execution::step) decided: advance, or perform a
/// call/grow that the run loop turns into an [`Outcome`].
pub(super) enum StepOutcome {
    Advance(u32),
    DoCall(CallReq),
    DoHostCall {
        func: Func,
        instance: Instance,
        return_ip: u32,
    },
    #[cfg(feature = "async")]
    DoHostAsyncCall {
        func: Func,
        instance: Instance,
        return_ip: u32,
    },
    DoGrow {
        memory: Memory,
        delta: u64,
        return_ip: u32,
    },
    DoTableGrow {
        table: Table,
        delta: u64,
        init: Ref,
        return_ip: u32,
    },
}

/// A resolved callee: a wasm body to push a frame for, or a host func to suspend on.
pub(super) enum ResolvedCall {
    Wasm(Instance, Arc<CompiledFunc>),
    Host(Func),
    #[cfg(feature = "async")]
    HostAsync(Func),
}

/// A pending wasm call (frame to push) produced by [`step`](super::Execution::step).
pub(super) struct CallReq {
    pub return_ip: u32,
    pub instance: Instance,
    pub code: Arc<CompiledFunc>,
}
