//! wasmtime-compatible error type, `Result` alias, and `bail!`/`ensure!`/`format_err!`.
//!
//! wasmtime 45.x defines its own `Error`/`Result` (no longer `anyhow::Error`).
//! We mirror that surface with a thin wrapper over `anyhow` so embedder code that
//! constructs/inspects errors (`Error::msg`, `downcast_ref::<Trap>()`, `bail!`)
//! ports unchanged.

use core::fmt::{Debug, Display};

/// A failure of an interpreter-owned invariant. This type is deliberately crate-private: callers
/// receive the existing public [`Error`] while guest exception handling cannot recognize or catch
/// the failure as a WebAssembly exception.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InternalError {
    OperandStack(&'static str),
    FrameStack(&'static str),
    ResultShape(&'static str),
    GcMetadata(&'static str),
    Compiler(&'static str),
}

impl Display for InternalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (area, detail) = match self {
            InternalError::OperandStack(detail) => ("operand stack", detail),
            InternalError::FrameStack(detail) => ("frame stack", detail),
            InternalError::ResultShape(detail) => ("result shape", detail),
            InternalError::GcMetadata(detail) => ("GC metadata", detail),
            InternalError::Compiler(detail) => ("compiler", detail),
        };
        write!(f, "internal {area} invariant violated: {detail}")
    }
}

impl std::error::Error for InternalError {}

/// The error type, mirroring `wasmtime::Error`.
pub struct Error(anyhow::Error);

/// The result type, mirroring `wasmtime::Result`.
pub type Result<T, E = Error> = core::result::Result<T, E>;

impl Error {
    pub fn new<E>(error: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Error(anyhow::Error::new(error))
    }

    pub fn msg<M>(message: M) -> Self
    where
        M: Display + Debug + Send + Sync + 'static,
    {
        Error(anyhow::Error::msg(message))
    }

    #[must_use]
    pub fn context<C>(self, context: C) -> Self
    where
        C: Display + Send + Sync + 'static,
    {
        Error(self.0.context(context))
    }

    pub fn downcast<E>(self) -> Result<E, Self>
    where
        E: Display + Debug + Send + Sync + 'static,
    {
        self.0.downcast::<E>().map_err(Error)
    }

    #[must_use]
    pub fn is<E>(&self) -> bool
    where
        E: Display + Debug + Send + Sync + 'static,
    {
        self.0.is::<E>()
    }

    #[must_use]
    pub fn downcast_ref<E>(&self) -> Option<&E>
    where
        E: Display + Debug + Send + Sync + 'static,
    {
        self.0.downcast_ref::<E>()
    }

    pub fn downcast_mut<E>(&mut self) -> Option<&mut E>
    where
        E: Display + Debug + Send + Sync + 'static,
    {
        self.0.downcast_mut::<E>()
    }

    #[must_use]
    pub fn root_cause(&self) -> &(dyn std::error::Error + 'static) {
        self.0.root_cause()
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl<E> From<E> for Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(error: E) -> Self {
        Error(anyhow::Error::new(error))
    }
}

impl From<Error> for anyhow::Error {
    fn from(error: Error) -> Self {
        error.0
    }
}

/// Constructs an [`Error`] from a format string (like `anyhow::format_err!`).
#[macro_export]
macro_rules! format_err {
    ($($arg:tt)*) => { $crate::Error::msg(::std::format!($($arg)*)) };
}

/// Returns early with an [`Error`] built from a format string (like `anyhow::bail!`).
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return ::core::result::Result::Err($crate::format_err!($($arg)*))
    };
}

/// Returns early with an [`Error`] if a condition is false (like `anyhow::ensure!`).
#[macro_export]
macro_rules! ensure {
    ($cond:expr $(,)?) => {
        if !($cond) {
            $crate::bail!(::core::concat!("condition failed: ", ::core::stringify!($cond)));
        }
    };
    ($cond:expr, $($arg:tt)*) => {
        if !($cond) {
            $crate::bail!($($arg)*);
        }
    };
}
