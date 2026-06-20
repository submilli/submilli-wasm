//! `Trap` and the error model (wasmtime-compatible; carried inside `anyhow::Error`).

use crate::store::AsContext;

/// A wasm trap code. Carried *inside* an [`anyhow::Error`]; recover via
/// `err.downcast_ref::<Trap>()`. Variant names match `wasmtime::Trap`.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Trap {
    StackOverflow,
    MemoryOutOfBounds,
    HeapMisaligned,
    TableOutOfBounds,
    IndirectCallToNull,
    BadSignature,
    IntegerOverflow,
    IntegerDivisionByZero,
    BadConversionToInteger,
    UnreachableCodeReached,
    Interrupt,
    OutOfFuel,
    NullReference,
    NullArrayReference,
    NullStructReference,
    NullI31Reference,
    CastFailure,
    ArrayOutOfBounds,
    AllocationTooLarge,
    CannotEnterComponent,
    NoAsyncResult,
}

impl core::fmt::Display for Trap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Trap::StackOverflow => "call stack exhausted",
            Trap::MemoryOutOfBounds => "out of bounds memory access",
            Trap::HeapMisaligned => "misaligned memory access",
            Trap::TableOutOfBounds => "undefined element: out of bounds table access",
            Trap::IndirectCallToNull => "uninitialized element",
            Trap::BadSignature => "indirect call type mismatch",
            Trap::IntegerOverflow => "integer overflow",
            Trap::IntegerDivisionByZero => "integer divide by zero",
            Trap::BadConversionToInteger => "invalid conversion to integer",
            Trap::UnreachableCodeReached => "wasm `unreachable` instruction executed",
            Trap::Interrupt => "interrupt",
            Trap::OutOfFuel => "all fuel consumed by WebAssembly",
            Trap::NullReference => "null reference",
            Trap::NullArrayReference => "null array reference",
            Trap::NullStructReference => "null structure reference",
            Trap::NullI31Reference => "null i31 reference",
            Trap::CastFailure => "cast failure",
            Trap::ArrayOutOfBounds => "out of bounds array access",
            Trap::AllocationTooLarge => "allocation size too large",
            Trap::CannotEnterComponent => "cannot enter component instance",
            Trap::NoAsyncResult => "async function returned no result",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for Trap {}

/// A captured wasm stack backtrace, mirroring `wasmtime::WasmBacktrace`.
#[derive(Debug)]
pub struct WasmBacktrace {
    frames: Vec<FrameInfo>,
}

impl WasmBacktrace {
    /// Captures a backtrace from the current execution, if backtraces are enabled.
    pub fn capture(store: impl AsContext) -> WasmBacktrace {
        todo!()
    }

    /// Captures a backtrace unconditionally.
    pub fn force_capture(store: impl AsContext) -> WasmBacktrace {
        todo!()
    }

    /// The captured frames, most-recent first.
    pub fn frames(&self) -> &[FrameInfo] {
        &self.frames
    }
}

/// Information about a single wasm stack frame, mirroring `wasmtime::FrameInfo`.
#[derive(Debug)]
pub struct FrameInfo {
    func_index: u32,
}

impl FrameInfo {
    /// The index of the function in its defining module.
    pub fn func_index(&self) -> u32 {
        self.func_index
    }
}
