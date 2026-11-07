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
    /// `at`: the absolute slot the old size (or -1) is written to once serviced.
    Grow {
        memory: Memory,
        delta: u64,
        at: usize,
    },
    /// In-wasm `table.grow`: routed through the (generic) driver so the limiter is consulted.
    TableGrow {
        table: Table,
        delta: u64,
        init: Ref,
        at: usize,
    },
    /// A guest GC allocation outgrew the reservation: grow it to `reserved_target` bytes through the
    /// (generic) limiter, then re-execute the allocating op. `bytes_needed` is the allocation's
    /// charge (reported if the limiter denies growth). See `Execution::gc_reserve`.
    GcGrow {
        reserved_target: usize,
        bytes_needed: u64,
    },
}

/// [`step`](super::Execution::step)'s return value when the op left a [`StepOutcome`] in
/// `Execution::control` instead of naming the next ip. Ops index a validated function body, far
/// below `u32::MAX`.
pub(super) const CONTROL: u32 = u32::MAX;

/// A call/grow the run loop must service after a [`step`](super::Execution::step), turning it
/// into a frame switch or an [`Outcome`]. Passed out of line (see [`CONTROL`]): returning this
/// large enum from every op kept the next ip off registers (~30% of CoreMark).
#[derive(Debug)]
pub(super) enum StepOutcome {
    Call(CallReq),
    /// Tail call to a wasm callee (#39): replace the current frame. `CallReq::return_ip` is unused.
    TailCall(CallReq),
    /// Tail call to a host fn (#39): pop the current frame, then run the host (its results return
    /// to the caller). `n_params` is needed to reposition the args before popping.
    TailHostCall {
        func: Func,
        instance: Instance,
        /// Absolute operand index of the first host argument.
        args_at: usize,
        n_params: usize,
    },
    HostCall {
        func: Func,
        instance: Instance,
        /// One past the host arguments (absolute): the call's stack window.
        top: usize,
        return_ip: u32,
    },
    /// Tail call to an async host fn (#39, async only).
    #[cfg(feature = "async")]
    TailHostAsyncCall {
        func: Func,
        instance: Instance,
        /// Absolute operand index of the first host argument.
        args_at: usize,
        n_params: usize,
    },
    #[cfg(feature = "async")]
    HostAsyncCall {
        func: Func,
        instance: Instance,
        /// One past the host arguments (absolute): the call's stack window.
        top: usize,
        return_ip: u32,
    },
    Grow {
        memory: Memory,
        delta: u64,
        at: usize,
        return_ip: u32,
    },
    TableGrow {
        table: Table,
        delta: u64,
        init: Ref,
        at: usize,
        return_ip: u32,
    },
    /// Grow the GC reservation to `reserved_target` bytes (limiter-consulted), then re-execute the
    /// allocating op at `return_ip`. `bytes_needed` is the allocation's charge (reported on denial).
    GcGrow {
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
#[derive(Debug)]
pub(super) struct CallReq {
    pub return_ip: u32,
    /// Absolute operand index of the first argument (the callee's frame base).
    pub args_at: usize,
    pub instance: Instance,
    pub func_index: u32,
    pub code: Code,
}
