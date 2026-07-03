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
    pub(super) fn service_gates(
        &mut self,
        inner: &mut StoreInner,
        (fuel, epoch, gc_pressure): (bool, bool, bool),
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
        // Honor an engine-wide GC-pressure request posted to this store's mailbox at this safe
        // point (operands are roots via the shadow). Read-and-clear, so we collect once per posted
        // request (not over and over); only large-footprint stores bother (no thundering herd);
        // request, not force — a finishing store simply never reaches here.
        if gc_pressure && inner.gc.footprint_over_floor() && inner.take_gc_request() {
            self.gc_collect_now(inner);
        }
        Ok(None)
    }
}
