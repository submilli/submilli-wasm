//! Call frames. One per active wasm call; the function's code is held as an
//! `Arc<CompiledFunc>` so the run loop can read ops without borrowing the store
//! or the value/frame stacks.

use std::sync::Arc;

use crate::error::InternalError;
use crate::instance::Instance;
use crate::module::code::{Code, Span};
use crate::module::op::{BranchTarget, CompiledFunc};

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
    /// Index into `Execution.values` where this frame's locals begin.
    pub locals_base: u32,
    /// First operand slot above this frame's parameters and locals.
    pub operand_base: u32,
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

    #[inline]
    pub(super) fn push_call(
        &mut self,
        instance: Instance,
        func_index: u32,
        code: Code,
    ) -> crate::Result<()> {
        let func = code.func(); // resolved once; the frame caches the record
        if self.values.len() != self.shadow.len() {
            return Err(InternalError::OperandStack("value/shadow length mismatch").into());
        }
        let value_len = u32::try_from(self.values.len())
            .map_err(|_| InternalError::FrameStack("operand stack length exceeds u32"))?;
        let caller_floor = self.frames.last().map_or(0, |frame| frame.operand_base);
        if value_len
            .checked_sub(caller_floor)
            .is_none_or(|available| available < func.n_params)
        {
            return Err(InternalError::OperandStack("call parameter underflow").into());
        }
        let locals_base = value_len
            .checked_sub(func.n_params)
            .ok_or(InternalError::FrameStack("call parameter underflow"))?;
        let local_types = code.local_types_of(&func);
        let operand_base = value_len
            .checked_add(
                u32::try_from(local_types.len())
                    .map_err(|_| InternalError::FrameStack("local count exceeds u32"))?,
            )
            .ok_or(InternalError::FrameStack("frame operand base exceeds u32"))?;
        for ty in local_types {
            self.push_default(ty);
        }
        self.frames.push(Frame {
            code,
            func: FrameFunc::of(&func),
            ip: 0,
            locals_base,
            operand_base,
            instance,
            func_index,
            delimiter: None,
        });
        self.sync_floor();
        Ok(())
    }

    /// Pushes a [`Delimiter`] boundary marker (no operands, inert `code`/`instance` filler). The
    /// next `push_call` lays the entered function's frame directly above it; `run`/`unwind` stop at
    /// this frame's depth so the call below it stays parked and untouched.
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
        let locals_base = u32::try_from(self.values.len())
            .map_err(|_| InternalError::FrameStack("operand stack length exceeds u32"))?;
        let func = FrameFunc::of(&code.func());
        self.frames.push(Frame {
            code,
            func,
            ip: 0,
            locals_base,
            operand_base: locals_base,
            instance,
            func_index: 0,
            delimiter: Some(kind),
        });
        self.sync_floor();
        Ok(())
    }

    /// Moves the top `keep` operands down over `pop` discarded ones, then jumps.
    #[inline]
    pub(super) fn take_branch(&mut self, t: BranchTarget) -> crate::Result<()> {
        // The common case (loop back-edges, most forward branches): the kept values already sit
        // on the label's floor, so there is nothing to move.
        // A pop-free branch still checks that its kept values exist above the frame floor, so a
        // miscompiled height fails here rather than at some later op.
        if t.pop == 0 {
            return self.ensure_operands(usize::from(t.keep)).map(|_| ());
        }
        self.drop_below(usize::from(t.keep), usize::from(t.pop))
    }

    /// Discards the `pop` operands beneath the top `keep`, sliding the kept ones down. The root
    /// shadow moves in lockstep with the cell stack (same offsets/length).
    fn drop_below(&mut self, keep: usize, pop: usize) -> crate::Result<()> {
        let needed = keep
            .checked_add(pop)
            .ok_or(InternalError::OperandStack("branch stack fixup overflow"))?;
        let dst = self.ensure_operands(needed)?;
        self.slide_down(dst + pop, dst, keep)
    }

    /// Moves the top `n` operands (starting at `src`) down to `dst < src` and drops everything
    /// above them; the shared tail of branch fixups and returns. The root shadow moves in lockstep.
    /// A single value (the common case) moves without the `memmove` call `copy_within` makes.
    fn slide_down(&mut self, src: usize, dst: usize, n: usize) -> crate::Result<()> {
        let len = self.values.len();
        if src.checked_add(n) != Some(len) || dst > src || self.shadow.len() != len {
            return Err(InternalError::OperandStack("stack fixup range out of bounds").into());
        }
        if n <= 1 {
            let top = self
                .values
                .get(src)
                .copied()
                .zip(self.shadow.get(src).copied());
            self.values.truncate(dst);
            self.shadow.truncate(dst);
            if let (1, Some((cell, tag))) = (n, top) {
                self.push_cell(cell, tag);
            }
            return Ok(());
        }
        self.values.copy_within(src..len, dst);
        self.values.truncate(dst + n);
        self.shadow.copy_within(src..len, dst);
        self.shadow.truncate(dst + n);
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

    /// Pops the current frame, moving its top `n_results` operands down to the
    /// frame base. Returns true if the frame stack has fallen back to `stop_depth`
    /// (this call's boundary) — i.e. the call this `run` was driving has finished.
    #[inline]
    pub(super) fn do_return(
        &mut self,
        n_results: u32,
        stop_depth: usize,
        tail: bool,
    ) -> crate::Result<bool> {
        if self.frames.len() <= stop_depth || self.values.len() != self.shadow.len() {
            return Err(InternalError::FrameStack("invalid return boundary").into());
        }
        let frame = self
            .frames
            .last()
            .ok_or(InternalError::FrameStack("frame stack underflow"))?;
        if frame.delimiter.is_some() {
            return Err(InternalError::FrameStack("cannot return from a delimiter").into());
        }
        let n = n_results as usize;
        let len = self.values.len();
        let dst = frame.locals_base as usize;
        let floor = frame.operand_base as usize;
        let available = len.checked_sub(floor).ok_or(InternalError::OperandStack(
            "return stack below frame floor",
        ))?;
        let result_end = dst.checked_add(n).ok_or(InternalError::OperandStack(
            "return result position overflow",
        ))?;
        if dst > floor || result_end > len || available < n || (!tail && available != n) {
            return Err(InternalError::ResultShape("wrong number of frame results").into());
        }
        self.frames.pop();
        self.sync_floor();
        self.slide_down(len - n, dst, n)?;
        Ok(self.frames.len() == stop_depth)
    }
}
