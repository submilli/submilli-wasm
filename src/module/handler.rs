//! The per-function exception table (#28d/#28e). Each `try_table` records a [`HandlerSpan`] covering
//! its body's instruction range; on a throw, the unwinder finds the innermost span containing the
//! throw-site `ip` and matches catch clauses by tag (ARCHITECTURE §15). No runtime handler stack —
//! `try_table` compiles like a `block` plus this side-table, so the normal path is unaffected.

/// One `try_table`'s catch clauses, keyed by the instruction range of its body `[start_ip, end_ip)`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct HandlerSpan {
    pub start_ip: u32,
    pub end_ip: u32,
    /// Clauses in source order; the first whose tag matches catches the exception.
    pub clauses: Box<[HandlerRec]>,
}

/// One catch clause: which tag it matches, the operand height to restore to, what payload to push,
/// and the landing-pad `ip` (a one-instruction `Op::Br` to the clause's label) to jump to.
#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct HandlerRec {
    /// Module-relative tag index to match, or `None` for `catch_all`/`catch_all_ref`.
    pub tag: Option<u32>,
    /// Operand-stack height (above the frame's locals) to truncate to before pushing the payload.
    pub restore_height: u32,
    /// Push the exception's tag arguments (`catch`/`catch_ref`).
    pub payload_args: bool,
    /// Push the `exnref` itself (`catch_ref`/`catch_all_ref`).
    pub payload_ref: bool,
    /// The landing-pad instruction (an `Op::Br` to the clause's label).
    pub landing_ip: u32,
}
