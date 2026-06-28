//! PTY error type.
//!
//! Simplified from `polyhooks/src/pty/error.rs` — the `Unsplit` variant is
//! removed because we do not expose the async split/unsplit API.

/// Errors returned by the PTY primitives.
#[derive(Debug)]
pub enum Error {
    /// Underlying I/O error.
    Io(std::io::Error),
    /// Error from a `rustix` syscall.
    Rustix(rustix::io::Errno),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Rustix(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rustix::io::Errno> for Error {
    fn from(e: rustix::io::Errno) -> Self {
        Self::Rustix(e)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Rustix(e) => Some(e),
        }
    }
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
