//! Everything the supervisor does with a child's output: drain it, hand it on
//! while the child is still running, and read a lock notice out of it.
//!
//! Split out of [`super`] so each file keeps one concern (and stays under the
//! workspace line cap). The parent module owns the deadline and the kill; this
//! one owns the two pipes.
//!
//! Draining on dedicated threads is not optional — a child that fills the
//! 64 KiB pipe buffer while nobody reads it blocks forever. What *is* a
//! decision is where the bytes go next: [`Live::pump`] is called from the
//! supervising thread, so the sink is never touched by a drain thread. A sink
//! draws to a terminal, and a terminal write between a pipe and the reader
//! emptying it is a stalled drain — which is a full pipe, which is the hang the
//! whole module exists to prevent. It also means a sink needs no locking of its
//! own.

use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

/// Read granularity for the drain threads. Large enough that a chatty child is
/// not read a syscall at a time, small enough that a lock notice is seen while
/// it still matters.
const DRAIN_CHUNK: usize = 8 * 1024;

/// What cargo prints — and then says nothing else — while it is queued behind a
/// lock held by another cargo process.
///
/// The full line is `Blocking waiting for file lock on <resource>` (`package
/// cache`, `artifact directory`, …), styled and indented like every other cargo
/// status line. Matching the stable prefix and taking the rest as the resource
/// keeps poly from having to enumerate cargo's lock names, which have changed
/// across releases.
const LOCK_WAIT_PREFIX: &str = "Blocking waiting for file lock on ";

/// The same notice with no resource named, for the older/degenerate form.
const LOCK_WAIT_BARE: &str = "Blocking waiting for file lock";

/// What poly reports as the resource when cargo names none.
const LOCK_WAIT_UNNAMED: &str = "a file";

/// A child's most recent word on whether it is *working* or *queued*.
///
/// A hook blocked on a lock held by a cargo process poly did not start — a
/// developer's own build, rust-analyzer — prints one `Blocking waiting for file
/// lock` line and then nothing at all. From the outside that is
/// indistinguishable from a wedged tool, and the liveness notice used to report
/// both as "still running". This records what the child last said so the notice
/// can tell them apart.
///
/// It holds the **latest** answer, not a latch: once the lock is granted cargo
/// resumes printing, and the next line clears the wait. A stale "waiting on a
/// lock" would be its own false report.
#[derive(Debug, Default)]
pub struct LockWait {
    /// The resource named by the last lock notice, cleared by any later output.
    resource: Mutex<Option<String>>,
}

impl LockWait {
    /// What the child is waiting for, if its most recent output said so.
    #[must_use]
    pub fn waiting_on(&self) -> Option<String> {
        self.lock().clone()
    }

    /// Record one complete line of the child's output.
    ///
    /// Blank lines are ignored: cargo's notice is often followed by an empty
    /// line, and treating that as progress would clear the wait a moment after
    /// announcing it.
    fn observe_line(&self, line: &str) {
        let line = console::strip_ansi_codes(line);
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        *self.lock() = lock_wait_resource(line).map(str::to_owned);
    }

    /// A poisoned lock still holds a valid answer: the only writer replaces the
    /// value wholesale, so there is no torn state to recover from — and a
    /// liveness notice must never panic the supervisor it is reporting on.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        lock(&self.resource)
    }
}

/// Take `mutex`, treating poison as a non-event.
///
/// Every mutex here guards state a panicking drain thread cannot tear: a whole
/// replaced `Option<String>`, or a `Vec<u8>` only ever appended to. A supervisor
/// that panicked because the process it was watching died badly would turn one
/// hook's crash into the whole run's.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The resource `line` says a tool is blocked on, if it is a cargo lock notice.
fn lock_wait_resource(line: &str) -> Option<&str> {
    if let Some(index) = line.find(LOCK_WAIT_PREFIX) {
        let resource = line[index + LOCK_WAIT_PREFIX.len()..].trim();
        return Some(if resource.is_empty() {
            LOCK_WAIT_UNNAMED
        } else {
            resource
        });
    }
    line.contains(LOCK_WAIT_BARE).then_some(LOCK_WAIT_UNNAMED)
}

/// Hand every complete line in `buffer` after `scanned` to `lock_wait`,
/// returning the offset just past the last one.
///
/// A partial trailing line is deliberately left unscanned — cargo's notice ends
/// with a newline, and judging a half-arrived line would let "Blocking waiting
/// for file lock" match text that turns out to be something else.
fn observe_lines(buffer: &[u8], scanned: usize, lock_wait: &LockWait) -> usize {
    let mut start = scanned;
    while let Some(offset) = buffer[start..].iter().position(|&byte| byte == b'\n') {
        let end = start + offset;
        lock_wait.observe_line(&String::from_utf8_lossy(&buffer[start..end]));
        start = end + 1;
    }
    start
}

/// One of the child's pipes, drained to EOF on its own thread.
///
/// The thread owns the reading and the line scanning; the supervising thread
/// only ever reads bytes it has not seen yet, from `handed` onward.
struct Stream {
    /// Everything read from the pipe so far. Appended to by the drain thread,
    /// read by the supervisor, and moved out whole once the pipe is at EOF.
    bytes: Arc<Mutex<Vec<u8>>>,
    /// The drain thread, joined by [`Stream::finish`].
    handle: JoinHandle<()>,
    /// How much of `bytes` has already been handed onward. The prefix stays in
    /// place — it is the captured output the caller gets back.
    handed: usize,
}

impl Stream {
    /// Start draining `reader`, so a chatty child never blocks on a full pipe
    /// while the supervisor is waiting on the clock.
    ///
    /// Each complete line is handed to `lock_wait` on the way past, on this
    /// thread: the observation exists to describe a child that has *not*
    /// exited, so it cannot wait for one that has.
    fn drain<R: Read + Send + 'static>(mut reader: R, lock_wait: Arc<LockWait>) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let written = Arc::clone(&bytes);
        let handle = std::thread::spawn(move || {
            let mut scanned = 0usize;
            let mut chunk = [0u8; DRAIN_CHUNK];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut buffer = lock(&written);
                        buffer.extend_from_slice(&chunk[..read]);
                        scanned = observe_lines(&buffer, scanned, &lock_wait);
                    }
                    // A signal interrupted the read; the pipe is still open, so
                    // reading again is the retry. Treating it as EOF would
                    // truncate the output a hook is about to be judged on.
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        Self {
            bytes,
            handle,
            handed: 0,
        }
    }

    /// Hand everything that arrived since the last call to `emit`.
    fn pump(&mut self, scratch: &mut Vec<u8>, emit: &mut dyn FnMut(&[u8])) {
        if take_new(&self.bytes, &mut self.handed, scratch) {
            emit(scratch);
        }
    }

    /// Wait for EOF, hand on the tail, and take the bytes the pipe produced.
    ///
    /// A panicked reader yields what a dead pipe would — whatever it managed to
    /// read — rather than taking the run down with it.
    fn finish(self, scratch: &mut Vec<u8>, emit: &mut dyn FnMut(&[u8])) -> Vec<u8> {
        let Self {
            bytes,
            handle,
            mut handed,
        } = self;
        let _ = handle.join();
        if take_new(&bytes, &mut handed, scratch) {
            emit(scratch);
        }
        std::mem::take(&mut *lock(&bytes))
    }
}

/// Copy everything after `handed` into `scratch`, advancing it; `false` when
/// nothing new had arrived.
///
/// Copying rather than emitting under the lock is deliberate: see the module
/// docs on why a drain thread must never wait for a sink.
fn take_new(bytes: &Mutex<Vec<u8>>, handed: &mut usize, scratch: &mut Vec<u8>) -> bool {
    let bytes = lock(bytes);
    if bytes.len() == *handed {
        return false;
    }
    scratch.clear();
    scratch.extend_from_slice(&bytes[*handed..]);
    *handed = bytes.len();
    true
}

/// The child's two pipes and the sink their bytes are handed to while it runs.
pub(super) struct Live<'a> {
    /// The child's stdout, absent only if the pipe could not be taken.
    stdout: Option<Stream>,
    /// The child's stderr, likewise.
    stderr: Option<Stream>,
    /// Reused staging buffer, so pumping a torrential child does not allocate
    /// once per poll.
    scratch: Vec<u8>,
    /// Where new bytes go. Called only from the supervising thread.
    emit: &'a mut dyn FnMut(&[u8]),
}

impl<'a> Live<'a> {
    /// Start draining both pipes, scanning both for a lock notice.
    pub(super) fn start<O, E>(
        stdout: Option<O>,
        stderr: Option<E>,
        lock_wait: &Arc<LockWait>,
        emit: &'a mut dyn FnMut(&[u8]),
    ) -> Self
    where
        O: Read + Send + 'static,
        E: Read + Send + 'static,
    {
        Self {
            stdout: stdout.map(|pipe| Stream::drain(pipe, Arc::clone(lock_wait))),
            stderr: stderr.map(|pipe| Stream::drain(pipe, Arc::clone(lock_wait))),
            scratch: Vec::new(),
            emit,
        }
    }

    /// Hand on whatever both pipes have produced since the last pump.
    pub(super) fn pump(&mut self) {
        let Self {
            stdout,
            stderr,
            scratch,
            emit,
        } = self;
        for stream in [stdout.as_mut(), stderr.as_mut()].into_iter().flatten() {
            stream.pump(scratch, &mut **emit);
        }
    }

    /// Drain both pipes to EOF and return their captured bytes, handing on
    /// anything the last pump missed.
    pub(super) fn finish(self) -> (Vec<u8>, Vec<u8>) {
        let Self {
            stdout,
            stderr,
            mut scratch,
            emit,
        } = self;
        let out = match stdout {
            Some(stream) => stream.finish(&mut scratch, &mut *emit),
            None => Vec::new(),
        };
        let err = match stderr {
            Some(stream) => stream.finish(&mut scratch, &mut *emit),
            None => Vec::new(),
        };
        (out, err)
    }
}

#[cfg(test)]
mod tests {
    use super::{LockWait, observe_lines};

    /// The notice arrives ANSI-styled, indented, and split across reads. None of
    /// that may hide it, and a half-arrived line may not be judged early.
    #[test]
    fn a_lock_notice_is_recognised_through_styling_and_chunk_boundaries() {
        let watch = LockWait::default();
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"    \x1b[1;36mBlocking\x1b[0m waiting for file lock on package ca");
        let mut scanned = observe_lines(&buffer, 0, &watch);
        assert_eq!(watch.waiting_on(), None, "a partial line must not be judged");

        buffer.extend_from_slice(b"che\n");
        scanned = observe_lines(&buffer, scanned, &watch);
        assert_eq!(watch.waiting_on().as_deref(), Some("package cache"));

        buffer.extend_from_slice(b"\n");
        scanned = observe_lines(&buffer, scanned, &watch);
        assert_eq!(
            watch.waiting_on().as_deref(),
            Some("package cache"),
            "a blank line is not progress"
        );

        buffer.extend_from_slice(b"    Checking poly-hooks v0.1.0\n");
        observe_lines(&buffer, scanned, &watch);
        assert_eq!(watch.waiting_on(), None, "real output means the lock was granted");
    }
}
