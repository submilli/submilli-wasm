//! Control-flow results threaded through the interpreter: why [`run`](super::Execution::run)
//! suspended ([`Outcome`]), what one [`step`](super::Execution::step) decided
//! ([`StepOutcome`]), and a resolved call target ([`ResolvedCall`]/[`CallReq`]).

use crate::extern_::{Memory, Table};
use crate::func::Func;
use crate::instance::Instance;
use crate::module::code::Code;
use crate::value::Ref;

/// Why [`Execution::run`](super::Execution::run) returned: either the call finished or it
/// suspended on something the (generic) driver in [`host`](super::host) must service.
pub(crate) enum Outcome {
    Finished,
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
    /// A guest GC allocation outgrew the reservation: grow it to `reserved_target` bytes through the
    /// (generic) limiter, then re-execute the allocating op. `bytes_needed` is the allocation's
    /// charge (reported if the limiter denies growth). See `Execution::gc_reserve`.
    GcGrow {
        reserved_target: usize,
        bytes_needed: u64,
    },
}

/// What a single [`step`](super::Execution::step) decided: advance, or perform a
/// call/grow that the run loop turns into an [`Outcome`].
pub(super) enum StepOutcome {
    Advance(u32),
    DoCall(CallReq),
    /// Tail call to a wasm callee (#39): replace the current frame. `CallReq::return_ip` is unused.
    DoTailCall(CallReq),
    /// Tail call to a host fn (#39): pop the current frame, then run the host (its results return
    /// to the caller). `n_params` is needed to reposition the args before popping.
    DoTailHostCall {
        func: Func,
        instance: Instance,
        n_params: u32,
    },
    DoHostCall {
        func: Func,
        instance: Instance,
        return_ip: u32,
    },
    /// Tail call to an async host fn (#39, async only).
    #[cfg(feature = "async")]
    DoTailHostAsyncCall {
        func: Func,
        instance: Instance,
        n_params: u32,
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
    /// Grow the GC reservation to `reserved_target` bytes (limiter-consulted), then re-execute the
    /// allocating op at `return_ip`. `bytes_needed` is the allocation's charge (reported on denial).
    DoGcGrow {
        reserved_target: usize,
        bytes_needed: u64,
        return_ip: u32,
    },
}

/// A resolved callee: a wasm body (defining instance + func index + code) to push a frame for,
/// or a host func to suspend on.
pub(super) enum ResolvedCall {
    Wasm(Instance, u32, Code),
    Host(Func),
    #[cfg(feature = "async")]
    HostAsync(Func),
}

/// A pending wasm call (frame to push) produced by [`step`](super::Execution::step).
pub(super) struct CallReq {
    pub return_ip: u32,
    pub instance: Instance,
    pub func_index: u32,
    pub code: Code,
}
