//! Fused single-pass driver. [`ValidateThenLower`] implements wasmparser's [`VisitOperator`] so each
//! operator is validated (via the `FuncValidator`) and lowered (via the [`Translator`]'s inherent
//! `visit_*` methods) from a **single decode** — no 56-byte `Operator` is ever materialized.
//!
//! The lowering methods live as inherent `impl Translator` blocks in the per-category modules
//! (numeric/memory/table/ref/gc/control, and simd behind the feature). Operators outside our
//! feature target still need a method for the trait to type-check; [`unsupported_lowering`] emits a
//! trapping stub for them (never reached — validation rejects them first).

use wasmparser::{
    for_each_visit_operator, FrameKind, FrameStack, FuncValidator, ValidatorResources,
    VisitOperator,
};

use super::control::BlockKind;
use super::{wp_err, Translator};
use crate::Result;

/// Validates each operator then lowers it, sharing one decode. Holds the byte offset of the operator
/// currently being visited (set by the driver loop before each `visit_operator`).
pub(super) struct ValidateThenLower<'a, 'v> {
    pub validator: &'v mut FuncValidator<ValidatorResources>,
    pub translator: &'v mut Translator<'a>,
    pub offset: usize,
}

/// Generates a straight-line `visit_*` method that lowers a nullary op via `$helper`
/// (`unop`/`binop`/`store`/`constop`/`emit`), skipping emission while unreachable.
macro_rules! lower_nullary {
    ($($visit:ident => $helper:ident $variant:ident;)*) => {
        $(
            pub(in crate::module::compile) fn $visit(&mut self) -> $crate::Result<()> {
                if self.reachable {
                    self.$helper($crate::module::op::Op::$variant);
                }
                Ok(())
            }
        )*
    };
}
pub(in crate::module::compile) use lower_nullary;

/// Like [`lower_nullary`] but for ops carrying a single `memarg` immediate (loads/stores).
macro_rules! lower_memarg {
    ($($visit:ident => $helper:ident $variant:ident;)*) => {
        $(
            pub(in crate::module::compile) fn $visit(
                &mut self,
                memarg: wasmparser::MemArg,
            ) -> $crate::Result<()> {
                if self.reachable {
                    let m = $crate::module::compile::conv::memarg(memarg);
                    self.$helper($crate::module::op::Op::$variant(m));
                }
                Ok(())
            }
        )*
    };
}
pub(in crate::module::compile) use lower_memarg;

/// The uniform `VisitOperator` body: validate the operator, record its source offset, then delegate
/// to the translator's inherent `visit_*` of the same name. Validation runs **before** lowering, so
/// an invalid operator never reaches the (index-trusting) lowering code.
macro_rules! validate_then_lower {
    ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*) )*) => {
        $(
            fn $visit(&mut self $($(, $arg: $argty)*)?) -> Result<()> {
                self.validator
                    .visitor(self.offset)
                    .$visit($($($arg.clone()),*)?)
                    .map_err(wp_err)?;
                self.translator.cur_offset = self.offset as u32;
                self.translator.$visit($($($arg),*)?)
            }
        )*
    };
}

/// Lets the fused driver hand `ValidateThenLower` straight to `BinaryReader::visit_operator`,
/// skipping `OperatorsReader`'s duplicate syntactic control stack (and its per-body allocation +
/// per-op `FrameStackAdapter` bookkeeping). The translator's `ctrl` stack is structurally complete
/// (frames are pushed/popped even in unreachable code), so it can answer the reader's only
/// questions: "is a frame open?" and "is the top an `if`?". `If` is reported even after `else` —
/// the validator (which runs on every operator before lowering) rejects a duplicate `else` anyway.
impl FrameStack for ValidateThenLower<'_, '_> {
    fn current_frame(&self) -> Option<FrameKind> {
        Some(match self.translator.open_frame_kind()? {
            BlockKind::Block => FrameKind::Block,
            BlockKind::Loop => FrameKind::Loop,
            BlockKind::If => FrameKind::If,
            BlockKind::TryTable => FrameKind::TryTable,
        })
    }
}

impl<'a> VisitOperator<'a> for ValidateThenLower<'a, '_> {
    type Output = Result<()>;

    #[cfg(feature = "simd")]
    fn simd_visitor(
        &mut self,
    ) -> Option<&mut dyn wasmparser::VisitSimdOperator<'a, Output = Self::Output>> {
        Some(self)
    }

    for_each_visit_operator!(validate_then_lower);
}

/// Emits a trapping inherent `visit_*` stub for every operator in a proposal we do **not** support,
/// so the `VisitOperator` impl type-checks. Supported proposals expand to nothing — their real
/// lowering methods live in the category modules. Never executed: the validator rejects these ops.
macro_rules! unsupported_lowering {
    ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*) )*) => {
        $( unsupported_lowering!(@one @$proposal $op $({ $($arg: $argty),* })? => $visit); )*
    };

    // --- supported proposals: real methods provided by the category modules ---
    (@one @mvp $($rest:tt)*) => {};
    (@one @sign_extension $($rest:tt)*) => {};
    (@one @saturating_float_to_int $($rest:tt)*) => {};
    (@one @bulk_memory $($rest:tt)*) => {};
    (@one @reference_types $($rest:tt)*) => {};
    (@one @tail_call $($rest:tt)*) => {};
    (@one @function_references $($rest:tt)*) => {};
    (@one @gc $($rest:tt)*) => {};
    (@one @exceptions $($rest:tt)*) => {};

    // --- everything else: trap (unreachable on validated input) ---
    (@one @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident) => {
        pub(in crate::module::compile) fn $visit(&mut self $($(, $arg: $argty)*)?) -> $crate::Result<()> {
            Err($crate::Error::msg(concat!("unsupported operator: ", stringify!($visit))))
        }
    };
}

impl Translator<'_> {
    for_each_visit_operator!(unsupported_lowering);
}
