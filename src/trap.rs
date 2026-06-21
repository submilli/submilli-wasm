//! `Trap` and the error model (wasmtime-compatible; carried inside `anyhow::Error`).

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
