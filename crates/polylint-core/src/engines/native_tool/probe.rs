//! Tool presence probing: spawn the binary with null I/O, then query its
//! version string. Cached for the process lifetime via the per-tool
//! `OnceLock`s in [`super::spec`].

use std::process::{Command, Stdio};

use super::spec::ToolSpec;

/// Determine if the tool described by `spec` is present on `PATH` and return
/// its version string.
///
/// Returns `Some(version_string)` on success, `None` when the binary cannot
/// be spawned (not on `PATH` or not executable).
///
/// The probe is inexpensive: it spawns the binary once with null I/O (just
/// to confirm it exists), then runs the version command. Callers cache the
/// result in the per-tool `OnceLock` so this function runs at most once per
/// process.
pub(crate) fn probe_tool(spec: &ToolSpec) -> Option<String> {
    // Spawn the probe binary with all I/O null to verify presence.
    // Format tools (gofmt, rustfmt, shfmt, zig fmt) exit cleanly on null
    // stdin. Lint-only tools (shellcheck with no args) print usage to stderr
    // and exit non-zero — the exit code is irrelevant; we only check whether
    // the binary can be spawned at all.
    let mut child = Command::new(spec.probe_binary())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?; // None → binary not on PATH
    let _ = child.wait(); // Reap the child to avoid zombies.

    // Binary is present; query the version.
    let raw = Command::new(spec.version_binary)
        .args(spec.version_args)
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| {
            // Some tools (e.g. older gofmt) write version to stderr.
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if stdout.is_empty() {
                String::from_utf8_lossy(&o.stderr).trim().to_string()
            } else {
                stdout
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}:found", spec.probe_binary()));

    Some(raw)
}
