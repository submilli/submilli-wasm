//! Backtrace capture (#29d): build a [`WasmBacktrace`] from the live frame stack at a trap/throw,
//! or from the host-call snapshot for `WasmBacktrace::capture`. Frames are most-recent first.

use std::sync::Arc;

use crate::backtrace::{FrameInfo, WasmBacktrace};
use crate::instance::Instance;
use crate::module::op::CompiledFunc;
use crate::store::{HostFrame, StoreInner};
use crate::trap::Trap;
use crate::Error;

use super::Execution;

/// Builds one frame's [`FrameInfo`], or `None` if the instance isn't registered (only happens in
/// synthetic, instance-free test executions). `func_index` is recovered by pointer-identity over
/// the module's compiled functions (only on the error/capture path, so the O(funcs) scan is fine).
pub(crate) fn frame_info(
    inner: &StoreInner,
    instance: Instance,
    code: &Arc<CompiledFunc>,
    ip: u32,
) -> Option<FrameInfo> {
    let module = inner.try_instance(instance)?.module.clone();
    let (func_index, code_offset) = {
        let mi = module.inner();
        let func_index = mi
            .functions
            .iter()
            .position(|f| Arc::ptr_eq(f, code))
            .map_or(0, |i| i as u32 + mi.num_imported_funcs);
        let code_offset = code
            .offsets
            .as_deref()
            .and_then(|o| o.get(ip as usize).copied());
        (func_index, code_offset)
    };
    Some(FrameInfo::new(module, func_index, code_offset))
}

/// Builds a backtrace from the store's host-call frame snapshot (for `WasmBacktrace::capture`).
pub(crate) fn from_host_frames(inner: &StoreInner) -> WasmBacktrace {
    let frames = inner
        .host_frames()
        .iter()
        .rev()
        .filter_map(|hf: &HostFrame| frame_info(inner, hf.instance, &hf.code, hf.ip))
        .collect();
    WasmBacktrace::from_frames(frames)
}

impl Execution {
    /// Captures a backtrace from the live frame stack, most-recent first. The top frame uses
    /// `top_ip` (the live trap/throw ip); callers use their saved call-site ip.
    pub(super) fn capture_backtrace(&self, inner: &StoreInner, top_ip: u32) -> WasmBacktrace {
        let last = self.frames.len().saturating_sub(1);
        let frames = self
            .frames
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(i, f)| {
                let ip = if i == last {
                    top_ip
                } else {
                    f.ip.saturating_sub(1)
                };
                frame_info(inner, f.instance, &f.code, ip)
            })
            .collect();
        WasmBacktrace::from_frames(frames)
    }

    /// A snapshot of the live wasm frames for the host-call window. Each frame is at a call site,
    /// so the recorded ip is `frame.ip - 1` (mirroring the unwinder's `fault_ip`).
    pub(super) fn host_frame_snapshot(&self) -> Vec<HostFrame> {
        self.frames
            .iter()
            .map(|f| HostFrame {
                instance: f.instance,
                code: f.code.clone(),
                ip: f.ip.saturating_sub(1),
            })
            .collect()
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
}
