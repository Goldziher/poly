//! Low-level PTY primitives backed by `rustix`.
//!
//! Ported from `polyhooks/src/pty/sys.rs`. The only change from upstream is
//! replacing `fs_err::OpenOptions` with `std::fs::OpenOptions` to avoid a
//! transitive tokio dependency from `fs-err`'s tokio feature.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;

/// The master end of a pseudo-terminal.
#[derive(Debug)]
pub struct Pty(OwnedFd);

impl Pty {
    /// Allocate a new PTY master, with `CLOEXEC` set.
    pub fn open() -> crate::pty::Result<Self> {
        let pt =
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)?;
        rustix::pty::grantpt(&pt)?;
        rustix::pty::unlockpt(&pt)?;

        let mut flags = rustix::io::fcntl_getfd(&pt)?;
        flags |= rustix::io::FdFlags::CLOEXEC;
        rustix::io::fcntl_setfd(&pt, flags)?;

        Ok(Self(pt))
    }

    /// Wrap an existing, already-open PTY fd.
    ///
    /// # Safety
    ///
    /// `fd` must be a valid, open file descriptor belonging to a PTY master.
    pub unsafe fn from_fd(fd: OwnedFd) -> Self {
        Self(fd)
    }

    /// Resize the terminal window associated with this PTY.
    pub fn set_term_size(&self, size: crate::pty::Size) -> crate::pty::Result<()> {
        Ok(rustix::termios::tcsetwinsize(
            &self.0,
            rustix::termios::Winsize::from(size),
        )?)
    }

    /// Open the slave (pts) end of this PTY.
    pub fn pts(&self) -> crate::pty::Result<Pts> {
        let name = rustix::pty::ptsname(&self.0, vec![])?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(
                rustix::fs::OFlags::NOCTTY
                    .bits()
                    .try_into()
                    .expect("OFlags bits fit in i32"),
            )
            .open(std::ffi::OsStr::from_bytes(name.as_bytes()))
            .map_err(crate::pty::Error::Io)?;
        // SAFETY: we just opened this fd with the correct flags.
        Ok(Pts(unsafe {
            OwnedFd::from_raw_fd(std::os::unix::io::IntoRawFd::into_raw_fd(file))
        }))
    }

    /// Put the PTY master into non-blocking mode.
    pub fn set_nonblocking(&self) -> rustix::io::Result<()> {
        let mut opts = rustix::fs::fcntl_getfl(&self.0)?;
        opts |= rustix::fs::OFlags::NONBLOCK;
        rustix::fs::fcntl_setfl(&self.0, opts)?;
        Ok(())
    }

    /// Read available bytes, returning `(filled, remaining)`.
    pub fn read_buf<'a>(
        &self,
        buf: &'a mut [std::mem::MaybeUninit<u8>],
    ) -> std::io::Result<(&'a mut [u8], &'a mut [std::mem::MaybeUninit<u8>])> {
        rustix::io::read(&self.0, buf).map_err(std::io::Error::from)
    }
}

impl From<Pty> for OwnedFd {
    fn from(pty: Pty) -> Self {
        // Move the owned fd out directly — no raw-fd round-trip (and no leak via
        // `forget`) is needed to transfer ownership.
        pty.0
    }
}

impl AsFd for Pty {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsRawFd for Pty {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl Read for Pty {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        rustix::io::read(&self.0, buf).map_err(std::io::Error::from)
    }
}

impl Write for Pty {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        rustix::io::write(&self.0, buf).map_err(std::io::Error::from)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for &Pty {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        rustix::io::read(&self.0, buf).map_err(std::io::Error::from)
    }
}

impl Write for &Pty {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        rustix::io::write(&self.0, buf).map_err(std::io::Error::from)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The slave (child) end of a PTY.
pub struct Pts(OwnedFd);

impl Pts {
    /// Wrap an existing slave fd.
    ///
    /// # Safety
    ///
    /// `fd` must be a valid, open file descriptor belonging to a PTY slave.
    pub unsafe fn from_fd(fd: OwnedFd) -> Self {
        Self(fd)
    }

    /// Clone the slave fd into three `Stdio` handles (stdin / stdout / stderr).
    pub fn setup_subprocess(
        &self,
    ) -> std::io::Result<(
        std::process::Stdio,
        std::process::Stdio,
        std::process::Stdio,
    )> {
        Ok((
            self.0.try_clone()?.into(),
            self.0.try_clone()?.into(),
            self.0.try_clone()?.into(),
        ))
    }

    /// Return a closure that makes the calling process a session leader and
    /// sets this pts as the controlling terminal.
    pub fn session_leader(&self) -> impl FnMut() -> std::io::Result<()> + use<> {
        let pts_fd = self.0.as_raw_fd();
        move || {
            rustix::process::setsid()?;
            // SAFETY: `pts_fd` is the raw fd of `self.0`. The caller MUST keep
            // the `Pts` that owns it alive until `Command::spawn()` returns —
            // this closure runs in the forked child via `pre_exec`, before
            // `exec`, and if the owning `Pts` were dropped earlier the fd would
            // be closed and this `borrow_raw` would alias a dangling descriptor.
            rustix::process::ioctl_tiocsctty(unsafe { BorrowedFd::borrow_raw(pts_fd) })?;
            Ok(())
        }
    }
}

impl From<Pts> for OwnedFd {
    fn from(pts: Pts) -> Self {
        pts.0
    }
}

impl AsFd for Pts {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsRawFd for Pts {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}
