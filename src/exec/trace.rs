//! Backtrace capture (#29d): build a [`WasmBacktrace`] from the live frame stack at a trap/throw,
//! or from the host-call snapshot for `WasmBacktrace::capture`. Frames are most-recent first.

use crate::backtrace::{FrameInfo, WasmBacktrace};
use crate::instance::Instance;
use crate::module::code::Code;
use crate::store::StoreInner;
use crate::trap::Trap;
use crate::Error;

use super::frame::{Delimiter, Frame};
use super::Execution;

/// Builds one frame's [`FrameInfo`], or `None` if the instance isn't registered (only happens in
/// synthetic, instance-free test executions). `func_index` is carried on the frame (#29e), so this
/// only reads `offsets[ip]` — no scan.
pub(crate) fn frame_info(
    inner: &StoreInner,
    instance: Instance,
    func_index: u32,
    code: &Code,
    ip: u32,
) -> Option<FrameInfo> {
    let module = inner.try_instance(instance)?.module.clone();
    let code_offset = code.offsets().and_then(|o| o.get(ip as usize).copied());
    Some(FrameInfo::new(module, func_index, code_offset))
}

/// Builds a backtrace by walking `frames` most-recent first: wasm frames become [`FrameInfo`]s,
/// host-re-entry delimiters become host-boundary markers (rendered by [`WasmBacktrace`]'s
/// `Display`), and the top-level delimiter is skipped. `top_ip` overrides the ip of the topmost
/// real frame (the live trap/throw site); the rest use their saved call-site ip (`frame.ip - 1`).
fn build(inner: &StoreInner, frames: &[Frame], top_ip: Option<u32>) -> WasmBacktrace {
    let last_real = frames.iter().rposition(|f| f.delimiter.is_none());
    let mut infos = Vec::new();
    let mut boundaries = Vec::new();
    for (i, f) in frames.iter().enumerate().rev() {
        match f.delimiter {
            // A host→wasm boundary sits just above the frames already collected.
            Some(Delimiter::HostReentry) => boundaries.push(infos.len()),
            Some(Delimiter::TopLevel) => {} // embedder→wasm entry: no marker
            None => {
                let ip = match top_ip {
                    Some(ip) if Some(i) == last_real => ip,
                    _ => f.ip.saturating_sub(1),
                };
                if let Some(info) = frame_info(inner, f.instance, f.func_index, &f.code, ip) {
                    infos.push(info);
                }
            }
        }
    }
    WasmBacktrace::from_frames_with_boundaries(infos, boundaries)
}

/// Builds a backtrace from the parked execution (for `WasmBacktrace::capture` inside a host fn):
/// every frame is at a call site, so all use `frame.ip - 1`. Empty when nothing is parked.
pub(crate) fn from_parked(inner: &StoreInner) -> WasmBacktrace {
    match inner.parked_exec() {
        Some(exec) => build(inner, exec.frames(), None),
        None => WasmBacktrace::from_frames(Vec::new()),
    }
}

impl Execution {
    /// The live frame stack (for backtrace capture from the parked execution).
    pub(crate) fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// Captures a backtrace from the live frame stack, most-recent first (the top frame uses the
    /// live trap/throw `top_ip`), spanning any parked outer calls with host-boundary markers.
    pub(super) fn capture_backtrace(&self, inner: &StoreInner, top_ip: u32) -> WasmBacktrace {
        build(inner, &self.frames, Some(top_ip))
    }

    /// For a trap reaching the run-loop boundary uncaught: attach a captured backtrace (gated on
    /// `wasm_backtrace`). Exceptions carry their backtrace on the instance instead — left untouched.
    /// The backtrace becomes the error's *source* and the `Trap` its *context*, so `Display` stays
    /// the trap message while `downcast_ref::<WasmBacktrace>()` still recovers the trace.
    pub(super) fn attach_trap_backtrace(&self, inner: &StoreInner, err: Error, ip: u32) -> Error {
        if !inner.engine().wasm_backtrace_enabled() {
            return err;
        }
        match err.downcast_ref::<Trap>().copied() {
            Some(trap) => Error::new(self.capture_backtrace(inner, ip)).context(trap),
            None => err, // non-trap (PendingException, or a host-surfaced error): left as-is
        }
    }

    /// Attaches a backtrace to an error raised at a suspension point (e.g. an epoch-deadline
    /// interrupt), where the run loop already saved the live ip on the top frame. Mirrors
    /// [`attach_trap_backtrace`](Self::attach_trap_backtrace) but sources the ip from that frame
    /// rather than the call site, so the interrupt trace points at the instruction we stopped on.
    pub(super) fn attach_suspension_backtrace(&self, inner: &StoreInner, err: Error) -> Error {
        match self.frames.last() {
            Some(f) => self.attach_trap_backtrace(inner, err, f.ip),
            None => err,
        }
    }
}
