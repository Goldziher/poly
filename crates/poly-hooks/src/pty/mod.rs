//! Blocking PTY primitives (Unix-only).
//!
//! Ported from `polyhooks/src/pty/{sys,error,types}.rs`. Unlike the upstream
//! version — which wraps the master fd in a tokio `AsyncFd` — this module
//! exposes only the synchronous `sys::Pty` layer. The `process` module's
//! `run_on_pty` method uses a plain blocking read loop instead of
//! `tokio::select!`.

#![cfg(unix)]

mod error;
mod sys;
mod types;

pub use error::{Error, Result};
pub use sys::{Pts, Pty};
pub use types::Size;

/// Open a new blocking PTY master + slave pair.
///
/// Unlike the upstream `prek` version, the master fd is **not** put into
/// non-blocking mode. Callers should drive it with a standard blocking
/// `Read` loop; EOF / `EIO` signals that all slave handles have been closed.
pub fn open() -> Result<(Pty, Pts)> {
    let pty = Pty::open()?;
    let pts = pty.pts()?;
    // Intentionally NOT calling pty.set_nonblocking() — callers block on read.
    Ok((pty, pts))
}
