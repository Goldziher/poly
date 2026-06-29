//! Synchronous command execution wrapper.
//!
//! Ported from `polyhooks/src/process.rs`. All async methods (`await`) are
//! replaced with their blocking equivalents:
//!
//! - `output_with_sink`: captures stdout+stderr via `Command::output()`, then
//!   feeds the captured bytes to `sink` post-completion (spec: "capture then
//!   feed sink").
//! - `pty_output_with_sink` / `run_on_pty` (Unix): single blocking `read` loop
//!   on the PTY master fd; handles Linux `EIO`-as-EOF and fast-exit drain.

use std::ffi::OsStr;
use std::fmt::Display;
use std::path::Path;
use std::process::{Command, CommandArgs, CommandEnvs, ExitStatus, Output, Stdio};
use std::sync::LazyLock;

use crate::consts::env_vars::EnvVars;
use owo_colors::OwoColorize as _;
use thiserror::Error;
use tracing::trace;

use crate::git::GIT;

static LOG_TRUNCATE_LIMIT: LazyLock<usize> = LazyLock::new(|| {
    EnvVars::var(EnvVars::PREK_LOG_TRUNCATE_LIMIT)
        .ok()
        .and_then(|limit| limit.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(120)
});

/// Error from executing a [`Cmd`].
#[derive(Debug, Error)]
pub enum Error {
    /// The command failed to launch (binary not found, permission denied, …).
    #[error("Run command `{summary}` failed")]
    Exec {
        /// Brief description of what the command was attempting.
        summary: String,
        /// The underlying I/O failure.
        #[source]
        cause: std::io::Error,
    },
    /// The command launched but exited with a non-zero status.
    #[error("Command `{summary}` exited with an error:\n{error}")]
    Status {
        /// Brief description of what the command was attempting.
        summary: String,
        /// Structured exit-status information.
        error: StatusError,
    },
    /// PTY allocation or setup failed (Unix only).
    #[cfg(unix)]
    #[error("Failed to open pty")]
    Pty(#[from] crate::pty::Error),
    /// Subprocess setup for PTY failed.
    #[error("Failed to setup subprocess for pty")]
    PtySetup(#[from] std::io::Error),
}

/// A non-zero exit status, optionally with captured output.
#[derive(Debug)]
pub struct StatusError {
    /// The exit status.
    pub status: ExitStatus,
    /// Captured stdout/stderr, if available.
    pub output: Option<Output>,
}

impl Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "\n{}\n{}", "[status]".red(), self.status)?;

        if let Some(output) = &self.output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            write_trimmed_output_section(f, "[stdout]", &stdout)?;
            write_trimmed_output_section(f, "[stderr]", &stderr)?;
        }

        Ok(())
    }
}

fn write_trimmed_output_section(
    f: &mut std::fmt::Formatter<'_>,
    label: &str,
    output: &str,
) -> std::fmt::Result {
    let mut lines = output.split('\n').filter_map(|line| {
        let line = line.trim();
        if line.is_empty() { None } else { Some(line) }
    });

    let Some(first) = lines.next() else {
        return Ok(());
    };

    // Truncate the rendered output: a failing command's stdout/stderr may carry
    // secrets (tokens echoed by a misconfigured tool, env dumps, …) and is
    // written verbatim into error messages and trace logs. Cap the total at
    // `LOG_TRUNCATE_LIMIT` characters and elide the remainder.
    let limit = *LOG_TRUNCATE_LIMIT;
    writeln!(f, "\n{}", label.red())?;
    let mut used = 0usize;
    for line in std::iter::once(first).chain(lines) {
        if used >= limit {
            writeln!(f, "[...]")?;
            break;
        }
        let remaining = limit - used;
        if line.chars().count() > remaining {
            let truncated: String = line.chars().take(remaining).collect();
            writeln!(f, "{truncated} [...]")?;
            break;
        }
        writeln!(f, "{line}")?;
        used += line.chars().count();
    }
    Ok(())
}

/// A receiver for command output chunks streamed during execution.
pub trait OutputSink {
    /// Called with each available chunk of combined stdout+stderr output.
    fn write_chunk(&mut self, chunk: &[u8]);
}

impl<S: OutputSink> OutputSink for &mut S {
    fn write_chunk(&mut self, chunk: &[u8]) {
        (**self).write_chunk(chunk);
    }
}

fn write_output_chunk(output: &mut Vec<u8>, sink: &mut impl OutputSink, chunk: &[u8]) {
    output.extend_from_slice(chunk);
    sink.write_chunk(chunk);
}

/// A synchronous command wrapper with structured error reporting.
///
/// Wraps [`std::process::Command`] with logging, status checking, and optional
/// output streaming. All methods are **blocking** — no async/await.
pub struct Cmd {
    /// The underlying standard-library command.
    pub inner: Command,
    summary: String,
    check_status: bool,
}

// ── Constructors ─────────────────────────────────────────────────────────────

impl Cmd {
    /// Create a new [`Cmd`] with a human-readable summary for error messages.
    pub fn new(command: impl AsRef<OsStr>, summary: impl Into<String>) -> Self {
        Self {
            inner: Command::new(command),
            summary: summary.into(),
            check_status: true,
        }
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

impl Cmd {
    /// Configure whether a non-zero exit status produces an `Err`.
    ///
    /// Defaults to `true`. When `false`, callers must check the status
    /// themselves via [`Cmd::check_status`] or by inspecting the returned
    /// [`ExitStatus`].
    pub fn check(&mut self, checked: bool) -> &mut Self {
        self.check_status = checked;
        self
    }

    /// Redirect the command's stdout to stderr.
    pub fn stdout_to_stderr(&mut self) -> &mut Self {
        self.inner.stdout(std::io::stderr());
        self
    }
}

// ── Execution ─────────────────────────────────────────────────────────────────

impl Cmd {
    /// Run the command, ignoring its output (only checking the exit status).
    pub fn run(&mut self) -> Result<(), Error> {
        self.status()?;
        Ok(())
    }

    /// Spawn the command, returning a [`std::process::Child`].
    pub fn spawn(&mut self) -> Result<std::process::Child, Error> {
        self.log_command();
        self.inner.spawn().map_err(|cause| Error::Exec {
            summary: self.summary.clone(),
            cause,
        })
    }

    /// Run the command, capturing and returning its output.
    ///
    /// Checks the exit status unless [`Cmd::check`] was called with `false`.
    pub fn output(&mut self) -> Result<Output, Error> {
        self.log_command();
        let output = self.inner.output().map_err(|cause| Error::Exec {
            summary: self.summary.clone(),
            cause,
        })?;
        self.maybe_check_output(&output)?;
        Ok(output)
    }

    /// Like [`Cmd::output`], but feeds captured bytes to `sink` after the
    /// process exits.
    ///
    /// Sync conversion note: the upstream async version streamed chunks in
    /// arrival order using `tokio::select!`. Here we capture via
    /// `Command::output()` (which internally uses pipes) and then feed all
    /// stdout bytes followed by all stderr bytes to the sink in one pass.
    /// This matches the spec's "capture then feed sink" directive.
    pub fn output_with_sink<S: OutputSink>(&mut self, mut sink: S) -> Result<Output, Error> {
        self.log_command();
        self.inner.stdin(Stdio::null());
        self.inner.stdout(Stdio::piped());
        self.inner.stderr(Stdio::piped());

        let output = self.inner.output().map_err(|cause| Error::Exec {
            summary: self.summary.clone(),
            cause,
        })?;

        // Post-completion feed: stdout first, then stderr, in 4 KiB chunks so
        // downstream sinks are not overwhelmed with a single giant write.
        for chunk in output.stdout.chunks(4096) {
            sink.write_chunk(chunk);
        }
        for chunk in output.stderr.chunks(4096) {
            sink.write_chunk(chunk);
        }

        self.maybe_check_output(&output)?;
        Ok(output)
    }

    /// Run the command under a PTY when colour is requested; otherwise fall
    /// back to [`Cmd::output_with_sink`].
    #[cfg(windows)]
    pub fn pty_output_with_sink<S: OutputSink>(&mut self, sink: S) -> Result<Output, Error> {
        self.output_with_sink(sink)
    }

    /// Run the command under a PTY when colour is requested (Unix only).
    ///
    /// Sync conversion note: replaces `tokio::select!` with a single blocking
    /// read loop on the PTY master fd. EIO is treated as EOF (Linux closes the
    /// master with EIO once all slave handles are gone). The drain pass after
    /// `wait()` is handled implicitly because the blocking read loop runs
    /// until EOF/EIO before `wait()` is called.
    #[cfg(not(windows))]
    pub fn pty_output_with_sink<S: OutputSink>(&mut self, sink: S) -> Result<Output, Error> {
        // If colour is not requested, piped output is sufficient.
        if !*USE_COLOR {
            return self.output_with_sink(sink);
        }
        self.run_on_pty(sink)
    }

    #[cfg(not(windows))]
    fn run_on_pty<S: OutputSink>(&mut self, mut sink: S) -> Result<Output, Error> {
        use std::io::Read as _;

        let (mut pty, pts) = crate::pty::open()?;
        let (stdin_stdio, stdout_stdio, stderr_stdio) = pts.setup_subprocess()?;

        self.inner.stdin(stdin_stdio);
        self.inner.stdout(stdout_stdio);
        self.inner.stderr(stderr_stdio);

        let mut child = self.inner.spawn().map_err(|cause| Error::Exec {
            summary: self.summary.clone(),
            cause,
        })?;

        // Release every slave-side fd held by the parent so the master observes
        // the child's last-close and the read loop terminates.
        //
        // `drop(pts)` releases the master's own slave handle, but it is not
        // enough on its own: `setup_subprocess` cloned the slave fd three times
        // into the `Stdio` handles passed to `self.inner`, and
        // `std::process::Command` keeps those `Stdio` values alive in the parent
        // after `spawn()` (they are only released when the `Command`'s stdio is
        // reconfigured or the `Command` is dropped). While the parent still owns
        // a slave fd the kernel never sees the slave's final close, so a blocking
        // `read()` on the master never returns EOF (macOS) / `EIO` (Linux) and
        // the loop below blocks forever. Resetting the stdio to null drops those
        // three retained clones now that the child holds its own dup'd copies.
        drop(pts);
        self.inner.stdin(Stdio::null());
        self.inner.stdout(Stdio::null());
        self.inner.stderr(Stdio::null());

        let mut buffer = [0u8; 4096];
        let mut captured = Vec::new();

        // Blocking read loop — handles both macOS (Ok(0) = EOF) and Linux
        // (Err(EIO) = all slave handles closed).
        loop {
            match pty.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => write_output_chunk(&mut captured, &mut sink, &buffer[..n]),
                Err(err) if err.raw_os_error() == Some(libc::EIO) => break,
                Err(err) => return Err(Error::PtySetup(err)),
            }
        }

        let status = child.wait().map_err(|cause| Error::Exec {
            summary: self.summary.clone(),
            cause,
        })?;

        let output = Output {
            status,
            stdout: captured,
            stderr: Vec::new(),
        };
        self.maybe_check_output(&output)?;
        Ok(output)
    }

    /// Run the command, returning only the exit status.
    pub fn status(&mut self) -> Result<ExitStatus, Error> {
        self.log_command();
        let status = self.inner.status().map_err(|cause| Error::Exec {
            summary: self.summary.clone(),
            cause,
        })?;
        self.maybe_check_status(status)?;
        Ok(status)
    }
}

// ── Forwarded std::process::Command APIs ─────────────────────────────────────

impl Cmd {
    /// Forward to [`Command::arg`].
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.inner.arg(arg);
        self
    }

    /// Forward to [`Command::args`].
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(args);
        self
    }

    /// Forward to [`Command::env`].
    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.env(key, val);
        self
    }

    /// Forward to [`Command::envs`].
    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.envs(vars);
        self
    }

    /// Forward to [`Command::env_remove`].
    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.inner.env_remove(key);
        self
    }

    /// Forward to [`Command::env_clear`].
    pub fn env_clear(&mut self) -> &mut Self {
        self.inner.env_clear();
        self
    }

    /// Forward to [`Command::current_dir`].
    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        self.inner.current_dir(dir);
        self
    }

    /// Forward to [`Command::stdin`].
    pub fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdin(cfg);
        self
    }

    /// Forward to [`Command::stdout`].
    pub fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stdout(cfg);
        self
    }

    /// Forward to [`Command::stderr`].
    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Self {
        self.inner.stderr(cfg);
        self
    }

    /// Forward to [`Command::get_program`].
    pub fn get_program(&self) -> &OsStr {
        self.inner.get_program()
    }

    /// Forward to [`Command::get_args`].
    pub fn get_args(&self) -> CommandArgs<'_> {
        self.inner.get_args()
    }

    /// Forward to [`Command::get_envs`].
    pub fn get_envs(&self) -> CommandEnvs<'_> {
        self.inner.get_envs()
    }

    /// Forward to [`Command::get_current_dir`].
    pub fn get_current_dir(&self) -> Option<&Path> {
        self.inner.get_current_dir()
    }

    /// Remove git-injected environment variables to isolate git invocations.
    pub fn remove_git_envs(&mut self) -> &mut Self {
        for key in crate::git::GIT_ENV_TO_REMOVE.iter() {
            self.inner.env_remove(key);
        }
        self
    }
}

// ── Diagnostic APIs ───────────────────────────────────────────────────────────

impl Cmd {
    /// Return `Err` if `status` is not success.
    pub fn check_status(&self, status: ExitStatus) -> Result<(), Error> {
        if status.success() {
            Ok(())
        } else {
            Err(Error::Status {
                summary: self.summary.clone(),
                error: StatusError {
                    status,
                    output: None,
                },
            })
        }
    }

    /// Return `Err` if `output.status` is not success.
    pub fn check_output(&self, output: &Output) -> Result<(), Error> {
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::Status {
                summary: self.summary.clone(),
                error: StatusError {
                    status: output.status,
                    output: Some(output.clone()),
                },
            })
        }
    }

    /// Conditionally check status (respects [`Cmd::check`]).
    pub fn maybe_check_status(&self, status: ExitStatus) -> Result<(), Error> {
        if self.check_status {
            self.check_status(status)?;
        }
        Ok(())
    }

    /// Conditionally check output status (respects [`Cmd::check`]).
    pub fn maybe_check_output(&self, output: &Output) -> Result<(), Error> {
        if self.check_status {
            self.check_output(output)?;
        }
        Ok(())
    }

    /// Emit a trace log for the command about to run.
    pub fn log_command(&self) {
        trace!("Executing `{self}`");
    }
}

// ── Colour detection ─────────────────────────────────────────────────────────

/// Whether the terminal supports colour (and thus whether the PTY path is used).
pub static USE_COLOR: LazyLock<bool> = LazyLock::new(|| {
    // Respect the PREK_COLOR env var first, then fall back to anstyle-query.
    if let Some(v) = EnvVars::var_as_bool(EnvVars::PREK_COLOR) {
        v
    } else {
        #[cfg(windows)]
        let supports = anstyle_query::windows::enable_ansi_colors().is_some();
        #[cfg(not(windows))]
        let supports = anstyle_query::term_supports_color();
        supports
    }
});

// ── Display ───────────────────────────────────────────────────────────────────

fn skip_args(cmd: &OsStr, cur: &OsStr, next: Option<&&OsStr>) -> usize {
    if GIT.as_ref().is_ok_and(|git| cmd == git) {
        if cur == "-c" {
            if let Some(flag) = next {
                let flag = flag.as_encoded_bytes();
                if flag.starts_with(b"core.useBuiltinFSMonitor")
                    || flag.starts_with(b"protocol.version")
                {
                    return 2;
                }
            }
        } else if cur == "--no-ext-diff"
            || cur == "--no-textconv"
            || cur == "--ignore-submodules"
            || cur == "--no-color"
        {
            return 1;
        }
    }
    0
}

impl Display for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(cwd) = self.get_current_dir() {
            write!(f, "cd {} && ", cwd.to_string_lossy())?;
        }
        let program = self.get_program();
        let mut args = self.get_args().peekable();

        write!(f, "{}", program.to_string_lossy())?;
        if args.peek().is_some_and(|arg| *arg == program) {
            args.next();
        }

        let mut len = 0;
        while let Some(arg) = args.next() {
            let skip = skip_args(program, arg, args.peek());
            if skip > 0 {
                for _ in 1..skip {
                    args.next();
                }
                continue;
            }
            write!(f, " {}", arg.to_string_lossy())?;
            len += arg.len() + 1;
            if len > *LOG_TRUNCATE_LIMIT {
                write!(f, " [...]")?;
                break;
            }
        }
        Ok(())
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::{Cmd, OutputSink};

    #[derive(Default)]
    struct RecordingSink {
        count: usize,
        bytes: Vec<u8>,
    }

    impl OutputSink for RecordingSink {
        fn write_chunk(&mut self, chunk: &[u8]) {
            self.count += 1;
            self.bytes.extend_from_slice(chunk);
        }
    }

    #[test]
    fn output_with_sink_streams_piped_stdout_and_stderr() {
        let mut sink = RecordingSink::default();
        let output = Cmd::new("/bin/sh", "piped streaming output test")
            .arg("-c")
            .arg("printf 'OUT\\n'; printf 'ERR\\n' >&2")
            .check(false)
            .output_with_sink(&mut sink)
            .expect("command should succeed");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("OUT\n"), "stdout: {stdout:?}");
        assert!(stderr.contains("ERR\n"), "stderr: {stderr:?}");
        assert_ne!(sink.count, 0, "sink should have received chunks");
    }

    #[test]
    #[cfg(unix)]
    fn pty_output_captures_trailing_output_after_fast_exit() {
        // Run a few iterations to catch potential race conditions in the blocking read loop.
        for _ in 0..5 {
            let mut sink = RecordingSink::default();
            let output = Cmd::new("/bin/sh", "pty trailing output test")
                .arg("-c")
                .arg("printf 'FINAL\\n'")
                .check(false)
                .run_on_pty(&mut sink)
                .expect("pty command should succeed");

            assert!(output.status.success());
            // PTY translates \n → \r\n on some systems; normalise.
            let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
            assert_eq!(stdout, "FINAL\n");
            assert!(output.stderr.is_empty());
        }
    }
}
