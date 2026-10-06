//! Call frames. One per active wasm call; the function's code is held as an
//! `Arc<CompiledFunc>` so the run loop can read ops without borrowing the store
//! or the value/frame stacks.

use crate::error::InternalError;
use crate::instance::Instance;
use crate::module::code::Code;
use crate::module::op::{BranchTarget, CompiledFunc};

#[derive(Debug)]
pub(crate) struct Frame {
    pub code: Code,
    /// Cached copy of the function record (spans + scalar facts) so the run loop and call
    /// paths never re-resolve `functions[index]` on frame switches.
    pub func: CompiledFunc,
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
            func,
            ip: 0,
            locals_base,
            operand_base,
            instance,
            func_index,
            delimiter: None,
        });
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
        let func = code.func();
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
        Ok(())
    }

    /// Moves the top `keep` operands down over `pop` discarded ones, then jumps.
    #[inline]
    pub(super) fn take_branch(&mut self, t: BranchTarget) -> crate::Result<()> {
        if self.values.len() != self.shadow.len() {
            return Err(InternalError::OperandStack("value/shadow length mismatch").into());
        }
        let len = self.values.len();
        let keep = usize::from(t.keep);
        let needed = keep
            .checked_add(usize::from(t.pop))
            .ok_or(InternalError::OperandStack("branch stack fixup overflow"))?;
        let floor = self
            .frames
            .last()
            .ok_or(InternalError::FrameStack("missing branch frame"))?
            .operand_base as usize;
        if len
            .checked_sub(floor)
            .is_none_or(|available| available < needed)
        {
            return Err(InternalError::OperandStack("branch stack fixup underflow").into());
        }
        let src = len - keep;
        let dst = src - usize::from(t.pop);
        self.values.copy_within(src..len, dst);
        self.values.truncate(dst + keep);
        // The root shadow moves in lockstep with the cell stack (same offsets/length).
        self.shadow.copy_within(src..len, dst);
        self.shadow.truncate(dst + keep);
        Ok(())
    }

    #[inline]
    pub(super) fn top(&self) -> crate::Result<(Code, CompiledFunc, u32, u32, Instance)> {
        let f = self
            .frames
            .last()
            .ok_or(InternalError::FrameStack("missing current frame"))?;
        if f.delimiter.is_some() {
            return Err(InternalError::FrameStack("delimiter used as executable frame").into());
        }
        Ok((f.code.clone(), f.func, f.ip, f.locals_base, f.instance))
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
        self.values.copy_within(len - n..len, dst);
        self.values.truncate(dst + n);
        self.shadow.copy_within(len - n..len, dst);
        self.shadow.truncate(dst + n);
        Ok(self.frames.len() == stop_depth)
    }
}
