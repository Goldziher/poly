//! Ask a `poly` executable which version it is, tolerating one that cannot run.
//!
//! A binary on `PATH` is not necessarily a working binary. Copying a fresh build
//! over a running `~/.cargo/bin/poly` invalidates its macOS code signature, and
//! every later invocation — `--version` included — is killed by the kernel with
//! **no output whatsoever**, which reads exactly like a hang. So this probe
//! never assumes success: it bounds the wait, captures the exit signal, and
//! reports "this executable could not tell us what it is" as a first-class
//! outcome rather than an absence.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

/// How long a `poly --version` may take before it is treated as hung. Generous
/// enough for a cold spawn of a large binary on a loaded machine — a false
/// "hung" verdict would be its own misleading report — but bounded, so a
/// genuinely hung executable cannot hang `poly doctor` with it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Polling interval while waiting for the probe to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// What an executable said when asked for its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Probe {
    /// It answered: the trimmed first line of its `--version` output.
    Reported {
        /// The full identity line, e.g. `poly 0.19.7 (release build v0.19.7, release)`.
        version: String,
    },
    /// It ran but could not be understood as a version.
    Failed {
        /// Human-readable explanation, including exit status or signal.
        detail: String,
    },
}

impl Probe {
    /// The reported version line, or `None` when the probe failed.
    pub fn version(&self) -> Option<&str> {
        match self {
            Probe::Reported { version } => Some(version),
            Probe::Failed { .. } => None,
        }
    }

    /// Whether this executable failed to identify itself — a defect in its own
    /// right, not merely missing information.
    pub fn is_failure(&self) -> bool {
        matches!(self, Probe::Failed { .. })
    }

    /// One-line rendering for reports.
    pub fn display(&self) -> &str {
        match self {
            Probe::Reported { version } => version,
            Probe::Failed { detail } => detail,
        }
    }
}

/// Run `<path> --version` and classify the outcome.
///
/// Never panics and never blocks indefinitely: the child is killed once
/// [`PROBE_TIMEOUT`] elapses.
pub fn probe_version(path: &Path) -> Probe {
    let spawned = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return Probe::Failed {
                detail: format!("could not be executed: {error}"),
            };
        }
    };

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The child has already been reaped; `wait_with_output` only
                // drains the (tiny) pipes and returns the cached status.
                return match child.wait_with_output() {
                    Ok(output) => classify(status, &output.stdout, &output.stderr),
                    Err(error) => Probe::Failed {
                        detail: format!("output could not be read: {error}"),
                    },
                };
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Probe::Failed {
                    detail: format!("did not answer `--version` within {}s (hung)", PROBE_TIMEOUT.as_secs()),
                };
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(error) => {
                return Probe::Failed {
                    detail: format!("could not be waited on: {error}"),
                };
            }
        }
    }
}

/// Turn an exit status plus captured output into a [`Probe`].
fn classify(status: std::process::ExitStatus, stdout: &[u8], stderr: &[u8]) -> Probe {
    let reported = first_line(stdout);
    if status.success() && !reported.is_empty() {
        return Probe::Reported { version: reported };
    }
    Probe::Failed {
        detail: failure_detail(status, stdout, stderr),
    }
}

/// Explain a failed probe, naming the signature-invalidation case explicitly
/// because it is otherwise indistinguishable from a hang.
fn failure_detail(status: std::process::ExitStatus, stdout: &[u8], stderr: &[u8]) -> String {
    let silent = stdout.is_empty() && stderr.is_empty();
    if let Some(signal) = terminating_signal(status) {
        let mut detail = format!("killed by signal {signal} (exit {})", 128 + signal);
        if silent {
            detail.push_str(
                " with no output — the executable is unrunnable, \
                 typically an invalidated code signature from overwriting a running binary; reinstall it",
            );
        }
        return detail;
    }
    let message = match first_line(stderr) {
        line if line.is_empty() => first_line(stdout),
        line => line,
    };
    match status.code() {
        Some(code) if silent => format!("exited {code} without reporting a version"),
        Some(code) => format!("exited {code}: {message}"),
        None => "exited abnormally without reporting a version".to_string(),
    }
}

/// The signal that terminated the process.
#[cfg(unix)]
fn terminating_signal(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;

    status.signal()
}

/// Windows has no terminating signals; the exit code carries everything.
#[cfg(not(unix))]
fn terminating_signal(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

/// The first non-empty line of a byte stream, lossily decoded and trimmed.
fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_executable_reports_a_failure_not_a_panic() {
        let probe = probe_version(Path::new("/nonexistent/poly"));
        assert!(probe.is_failure(), "a missing executable is a failed probe");
        assert!(probe.version().is_none());
        assert!(
            probe.display().contains("could not be executed"),
            "detail explains the spawn failure: {}",
            probe.display()
        );
    }

    /// Uses a script rather than `/usr/bin/true`, which is not the same program
    /// everywhere: GNU coreutils' `true --version` *prints a version*, so the
    /// system binary only satisfies this test's premise on BSD/macOS and the
    /// test failed on every Linux runner.
    #[cfg(unix)]
    #[test]
    fn a_binary_that_prints_nothing_is_a_failure_not_an_empty_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("silent-poly");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&path);
        let probe = probe_version(&path);
        assert!(
            probe.is_failure(),
            "exit 0 with no output must not be read as a version"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_that_answers_is_reported_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-poly");
        std::fs::write(&path, "#!/bin/sh\necho 'poly 9.9.9 (dev build vX, debug)'\n").unwrap();
        make_executable(&path);
        let probe = probe_version(&path);
        assert_eq!(
            probe.version(),
            Some("poly 9.9.9 (dev build vX, debug)"),
            "the identity line is captured verbatim"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_killed_by_a_signal_names_the_signature_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("killed-poly");
        // SIGKILL itself with no output — the shape of the macOS broken-signature
        // failure, which the shell surfaces as exit 137.
        std::fs::write(&path, "#!/bin/sh\nkill -9 $$\n").unwrap();
        make_executable(&path);
        let probe = probe_version(&path);
        assert!(probe.is_failure());
        let detail = probe.display();
        assert!(detail.contains("exit 137"), "names the shell-visible status: {detail}");
        assert!(detail.contains("code signature"), "explains the likely cause: {detail}");
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
