//! The rare per-op run-loop gates (fuel, epoch interruption, GC pressure), out of the hot path:
//! `run` consults them only when the engine config enables at least one.

use super::{Execution, Outcome};
use crate::store::{FuelStep, StoreInner};
use crate::trap::Trap;
use crate::Result;

impl Execution {
    /// Services the rare per-op gates — `(fuel, epoch, gc_pressure)` per the engine config:
    /// `Ok(Some(_))` suspends the loop (yield/deadline), `Err` traps (fuel exhausted), `Ok(None)`
    /// continues. Off the hot path: `run` calls this only when at least one gate is enabled.
    #[inline]
    pub(super) fn service_gates(
        &mut self,
        inner: &mut StoreInner,
        (fuel, epoch): (bool, bool),
        ip: u32,
    ) -> Result<Option<Outcome>> {
        if fuel {
            match inner.consume_fuel_step() {
                FuelStep::Ran => {}
                FuelStep::Exhausted => {
                    return Err(self.attach_trap_backtrace(inner, Trap::OutOfFuel.into(), ip))
                }
                FuelStep::NeedYield => {
                    #[cfg(feature = "async")]
                    {
                        self.frames.last_mut().expect("current frame").ip = ip;
                        return Ok(Some(Outcome::FuelYield));
                    }
                    // Unreachable without async (an interval needs an async store); stays total.
                    #[cfg(not(feature = "async"))]
                    return Err(self.attach_trap_backtrace(inner, Trap::OutOfFuel.into(), ip));
                }
            }
        }
        if epoch && inner.epoch_deadline_reached() {
            self.frames.last_mut().expect("current frame").ip = ip;
            return Ok(Some(Outcome::EpochDeadline));
        }
        Ok(None)
    }

    /// Honors a posted engine-wide GC-pressure request (read-and-clear; operands are roots via
    /// the shadow, so any safepoint with consistent stacks qualifies). Amortized off the per-op
    /// path: the dispatch loop calls this every [`super::GC_CHECK_INTERVAL`] guest calls, and
    /// the async driver after every host-call await (the natural long-latency point — other
    /// tenants generate pressure while this guest is parked). Only large-footprint stores
    /// collect (no thundering herd); request, not force.
    pub(super) fn service_gc_pressure(&mut self, inner: &mut StoreInner) {
        if inner.gc.footprint_over_floor() && inner.take_gc_request() {
            self.gc_collect_now(inner);
        }
    }

    /// [`service_gc_pressure`](Self::service_gc_pressure) with the armed check included, for
    /// call sites outside the dispatch loop (which precomputes the armed flag).
    pub(super) fn gc_pressure_safepoint(&mut self, inner: &mut StoreInner) {
        if inner.gc.is_collecting() && inner.engine().gc_memory_threshold().is_some() {
            self.service_gc_pressure(inner);
        }
    }
}
