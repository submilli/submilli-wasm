//! Call frames. One per active wasm call; the function's code is held as an
//! `Arc<CompiledFunc>` so the run loop can read ops without borrowing the store
//! or the value/frame stacks.

use std::sync::Arc;

use super::cell::{Cell, RefTag, SLOT_BYTES};
use crate::error::InternalError;
use crate::instance::Instance;
use crate::module::code::{Code, Span};
use crate::module::op::{BranchTarget, CompiledFunc};
use crate::trap::Trap;

/// The part of a function's [`CompiledFunc`] the run loop reads on every frame switch, cached so
/// it never re-resolves `functions[index]`. Not the whole 64-byte record: copying that into each
/// frame and back out on every call/return was measurable in call-heavy code.
#[derive(Copy, Clone, Debug)]
pub(crate) struct FrameFunc {
    pub ops: Span,
    pub n_results: u32,
}

impl FrameFunc {
    fn of(func: &CompiledFunc) -> Self {
        FrameFunc {
            ops: func.ops,
            n_results: func.n_results,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Frame {
    pub code: Code,
    pub func: FrameFunc,
    /// Resume point in `code.ops` (saved when this frame makes a call).
    pub ip: u32,
    /// Index into `Execution.values` where this frame's locals (slot 0) begin.
    pub locals_base: u32,
    /// First operand-stack slot above this frame's parameters and locals.
    pub operand_base: u32,
    /// The operand buffer's length while this frame is on top (its slots end here, or further
    /// if the caller's do); restored when a callee returns.
    pub extent: u32,
    /// One past this frame's last live slot, exact at its safepoints (a call, a stack-window op,
    /// a throw, a host return, a caught exception): the collector scans `[locals_base, live_end)`
    /// and clears the tags of dead slots above it. Between safepoints it goes stale (hot ops write
    /// references above it), so host code entered at an arbitrary op — the epoch callback — first
    /// widens it to the buffer's end (`widen_top_live_range`).
    pub live_end: u32,
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
    /// Re-derives the cached operand floor after any frame push/pop (see `Execution::floor`).
    #[inline]
    pub(super) fn sync_floor(&mut self) {
        self.floor = self
            .frames
            .last()
            .map_or(usize::MAX, |frame| frame.operand_base as usize);
    }

    /// Pushes a wasm callee's frame at `args_at`, where its arguments already sit (zero-copy: they
    /// become its first locals; its results land back there). Grows the operand buffer to the
    /// frame's extent with zeroed cells and default-initializes the declared locals; the
    /// temporaries above are never read before written (validation), so they aren't cleared.
    #[inline]
    pub(super) fn push_call(
        &mut self,
        instance: Instance,
        func_index: u32,
        code: Code,
        args_at: usize,
    ) -> crate::Result<()> {
        let func = code.func(); // resolved once; the frame caches the record
        let local_types = code.local_types_of(&func);
        let geometry = frame_geometry(args_at, &func, local_types.len())?;
        let args_end = args_at.saturating_add(func.n_params as usize);
        if args_at < self.floor || args_end > self.values.len() {
            return Err(InternalError::OperandStack("call arguments out of place").into());
        }
        let extent = geometry.end.max(self.values.len());
        self.grow_for_frame(extent)?;
        for (i, ty) in local_types.iter().enumerate() {
            self.set_default(args_end + i, ty)?;
        }
        self.frames.push(Frame {
            code,
            func: FrameFunc::of(&func),
            ip: 0,
            locals_base: geometry.base,
            operand_base: geometry.operand_base,
            extent: to_u32(extent)?,
            live_end: geometry.operand_base,
            instance,
            func_index,
            delimiter: None,
        });
        self.sync_floor();
        self.sp = geometry.operand_base as usize;
        Ok(())
    }

    /// Charges a new frame (the buffer grown to `extent`) against `max_wasm_stack` *before*
    /// allocating — a frame's operand area is sized statically, so one call can claim a large
    /// extent — then grows the buffer zeroed. An allocation failure traps instead of aborting.
    fn grow_for_frame(&mut self, extent: usize) -> crate::Result<()> {
        let frames = self.frames.len().saturating_add(1);
        if self.stack_bytes_for(extent, frames)? >= self.stack_limit {
            return Err(Trap::StackOverflow.into());
        }
        let extra = extent.saturating_sub(self.values.len());
        if extra == 0 {
            return Ok(());
        }
        if self.values.try_reserve(extra).is_err() || self.shadow.try_reserve(extra).is_err() {
            return Err(Trap::StackOverflow.into());
        }
        self.values
            .resize(extent, Cell::of_bytes([0u8; SLOT_BYTES]));
        self.shadow.resize(extent, RefTag::NONE);
        Ok(())
    }

    /// Pushes a [`Delimiter`] boundary marker (no slots, inert `code`/`instance` filler) at the
    /// buffer's end. The entered call's arguments and frame go directly above it; `run`/`unwind`
    /// stop at this frame's depth so the call below it stays parked and untouched.
    #[inline]
    pub(super) fn push_delimiter(
        &mut self,
        kind: Delimiter,
        instance: Instance,
        code: Code,
    ) -> crate::Result<()> {
        if self.values.len() != self.shadow.len() {
            return Err(InternalError::OperandStack("value/shadow length mismatch").into());
        }
        let base = to_u32(self.values.len())?;
        let func = FrameFunc::of(&code.func());
        self.frames.push(Frame {
            code,
            func,
            ip: 0,
            locals_base: base,
            operand_base: base,
            extent: base,
            live_end: base,
            instance,
            func_index: 0,
            delimiter: Some(kind),
        });
        self.sync_floor();
        self.sp = base as usize;
        Ok(())
    }

    /// A taken branch's operand fixup: the top `keep` values (ending at absolute `top`, the
    /// branch's stack top after its own operands) move down over the `pop` discarded ones. Static
    /// heights make the common `pop == 0` case free; it still checks the kept values sit above the
    /// frame floor, so a miscompiled height fails closed.
    #[inline]
    pub(super) fn take_branch(&mut self, t: BranchTarget, top: usize) -> crate::Result<()> {
        // A branch that keeps nothing moves nothing — and its condition/index operand may live in
        // any slot (a local), so `top` is not a stack top to check.
        if t.keep == 0 {
            return Ok(());
        }
        let src = top
            .checked_sub(usize::from(t.keep))
            .filter(|&src| src >= self.floor && top <= self.values.len())
            .ok_or(InternalError::OperandStack("branch fixup out of range"))?;
        if t.pop == 0 {
            return Ok(());
        }
        let dst = src
            .checked_sub(usize::from(t.pop))
            .filter(|&dst| dst >= self.floor)
            .ok_or(InternalError::OperandStack(
                "branch fixup below frame floor",
            ))?;
        self.move_slots(src, dst, usize::from(t.keep))
    }

    /// Copies `n` slots (values and their root-shadow tags) from `src` down to `dst`.
    #[inline]
    fn move_slots(&mut self, src: usize, dst: usize, n: usize) -> crate::Result<()> {
        let end = src.checked_add(n).filter(|&end| end <= self.values.len());
        if end.is_none() || dst > src || self.shadow.len() != self.values.len() {
            return Err(InternalError::OperandStack("slot move out of range").into());
        }
        match n {
            0 => {}
            1 => {
                let (cell, tag) = self.cell_at(src)?;
                self.set_cell(dst, cell, tag)?;
            }
            _ => {
                self.values.copy_within(src..src + n, dst);
                self.shadow.copy_within(src..src + n, dst);
            }
        }
        Ok(())
    }

    #[inline]
    pub(super) fn top(&self) -> crate::Result<(Code, FrameFunc, u32, u32, Instance)> {
        let f = self.top_frame()?;
        Ok((f.code.clone(), f.func, f.ip, f.locals_base, f.instance))
    }

    /// [`top`](Self::top) for the run loop's frame switches: retargets the loop's `code` handle
    /// in place, bumping the module `Arc` only when the new frame runs in a different module.
    /// Kept out of line: inlined into the dispatch loop it perturbed the hot ops' code layout
    /// (same instruction count, 8–15% more cycles on loop-heavy code).
    #[inline(never)]
    pub(super) fn top_into(
        &self,
        code: &mut Code,
    ) -> crate::Result<(FrameFunc, u32, u32, Instance)> {
        let f = self.top_frame()?;
        if Arc::ptr_eq(&code.module, &f.code.module) {
            code.index = f.code.index;
        } else {
            *code = f.code.clone();
        }
        Ok((f.func, f.ip, f.locals_base, f.instance))
    }

    #[inline]
    fn top_frame(&self) -> crate::Result<&Frame> {
        let f = self
            .frames
            .last()
            .ok_or(InternalError::FrameStack("missing current frame"))?;
        if f.delimiter.is_some() {
            return Err(InternalError::FrameStack("delimiter used as executable frame").into());
        }
        Ok(f)
    }

    /// Returns from the current frame: its `n_results` results (at its operand base — static
    /// heights put them there) move to the frame base, the caller's results position. Returns true
    /// if the frame stack has fallen back to `stop_depth` (the call this `run` drove finished).
    #[inline]
    pub(super) fn do_return(&mut self, n_results: u32, stop_depth: usize) -> crate::Result<bool> {
        if self.frames.len() <= stop_depth {
            return Err(InternalError::FrameStack("invalid return boundary").into());
        }
        let frame = self.top_frame()?;
        let (src, dst) = (frame.operand_base as usize, frame.locals_base as usize);
        self.finish_return(src, dst, n_results as usize, stop_depth)
    }

    /// Pops the current frame after moving `n` values from absolute `src` (a tail call's
    /// arguments) to the frame base; see [`do_return`](Self::do_return).
    pub(super) fn return_from(
        &mut self,
        src: usize,
        n: usize,
        stop_depth: usize,
    ) -> crate::Result<bool> {
        if self.frames.len() <= stop_depth {
            return Err(InternalError::FrameStack("invalid return boundary").into());
        }
        let dst = self.top_frame()?.locals_base as usize;
        self.finish_return(src, dst, n, stop_depth)
    }

    #[inline]
    fn finish_return(
        &mut self,
        src: usize,
        dst: usize,
        n: usize,
        stop_depth: usize,
    ) -> crate::Result<bool> {
        self.move_slots(src, dst, n)?;
        self.pop_frame(dst.saturating_add(n));
        Ok(self.frames.len() == stop_depth)
    }

    /// Pops the top frame and shrinks the operand buffer back to the caller's extent, keeping at
    /// least `keep_to` cells (results just returned below the old frame).
    #[inline]
    pub(super) fn pop_frame(&mut self, keep_to: usize) {
        self.frames.pop();
        self.sync_floor();
        let caller_extent = self.frames.last().map_or(0, |frame| frame.extent as usize);
        let len = caller_extent.max(keep_to);
        self.values.truncate(len);
        self.shadow.truncate(len);
    }

    /// The current (top, executable) frame's base: a tail call lays its callee there.
    pub(super) fn frame_base(&self) -> crate::Result<usize> {
        Ok(self.top_frame()?.locals_base as usize)
    }
}

/// A callee frame's absolute layout: `base` (slot 0), `operand_base` (operand-stack slot 0), and
/// `end` (one past its deepest operand slot).
struct FrameGeometry {
    base: u32,
    operand_base: u32,
    end: usize,
}

#[inline]
fn frame_geometry(
    args_at: usize,
    func: &CompiledFunc,
    n_declared: usize,
) -> crate::Result<FrameGeometry> {
    let overflow = || InternalError::FrameStack("frame exceeds the operand index space");
    // Results return from the operand base, so they must fit the frame (always true for compiled
    // code: the compiler sizes every frame for its results); a corrupt artifact fails closed here.
    if func.n_results > func.max_operands {
        return Err(InternalError::FrameStack("function results exceed its frame").into());
    }
    let base = to_u32(args_at)?;
    let n_locals = u32::try_from(n_declared)
        .ok()
        .and_then(|n| n.checked_add(func.n_params))
        .ok_or_else(overflow)?;
    let operand_base = base.checked_add(n_locals).ok_or_else(overflow)?;
    let end = operand_base
        .checked_add(func.max_operands)
        .ok_or_else(overflow)?;
    Ok(FrameGeometry {
        base,
        operand_base,
        end: end as usize,
    })
}

#[inline]
fn to_u32(index: usize) -> crate::Result<u32> {
    u32::try_from(index).map_err(|_| InternalError::FrameStack("operand index exceeds u32").into())
}
