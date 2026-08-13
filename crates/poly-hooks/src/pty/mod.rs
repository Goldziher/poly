//! Blocking PTY primitives (Unix-only).
//!
//! Ported from `polyhooks/src/pty/{sys,error,types}.rs`. Unlike the upstream
//! version — which wraps the master fd in a tokio `AsyncFd` — this module
//! exposes only the synchronous `sys::Pty` layer.
//!
//! Nothing currently executes hooks on a PTY: the ported executor took no
//! [`crate::timeout::Budget`], so a hook run through it would have escaped the
//! kill subsystem entirely, and it was removed rather than left one call away
//! from resurrection (see [`crate::process`]). These primitives are kept
//! because restoring colour output is worth doing — but it must be built on
//! [`crate::supervise`], where the budget cannot be forgotten.

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
    Ok((pty, pts))
}
