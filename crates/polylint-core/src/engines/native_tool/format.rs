//! Per-file format dispatch: pipe source content through a native formatter
//! CLI and collect its stdout output.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::Context;

use crate::engine::{FormatOutput, SourceFile};

use super::spec::ToolSpec;

/// Pipe `src.content` through the format binary described by `spec`
/// (stdin → stdout).
///
/// `indent_width` is injected as `-i {indent_width}` when
/// `spec.format_indent_flag` is true (used by shfmt).
///
/// Returns:
/// - [`FormatOutput::Unchanged`] when the tool exits non-zero (syntax error
///   in the source — never corrupt the file), or when the output equals the
///   input byte-for-byte.
/// - [`FormatOutput::Formatted(s)`] when the tool produced different output.
///
/// # Deadlock prevention
///
/// A dedicated OS thread writes stdin while the main (rayon) worker thread
/// collects stdout via `wait_with_output`. This prevents the pipe-buffer
/// deadlock that can occur for source files larger than the OS pipe buffer
/// (~64 KB on Linux) when a formatter buffers all input before writing output.
pub(crate) fn format_via_tool(
    spec: &ToolSpec,
    src: &SourceFile,
    indent_width: usize,
) -> anyhow::Result<FormatOutput> {
    let format_binary = spec
        .format_binary
        .expect("format_via_tool called on a lint-only ToolSpec");

    let mut cmd = Command::new(format_binary);

    // Prepend `-i <n>` when the spec requests it (e.g. shfmt). Inserted
    // before the static format_args so the tool sees them in the right order.
    if spec.format_indent_flag {
        cmd.arg("-i");
        cmd.arg(indent_width.to_string());
    }
    cmd.args(spec.format_args);

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Suppress tool diagnostics: non-zero exit is the failure signal.
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn '{format_binary}'"))?;

    // Clone the Arc<str> — a reference-count bump, not a copy of the bytes.
    let content = std::sync::Arc::clone(&src.content);
    let mut stdin_handle = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("'{format_binary}' stdin pipe was not created"))?;

    // Write in a separate thread to prevent a deadlock that would occur if the
    // child's stdout pipe fills before we have read any of it.
    let write_thread = thread::spawn(move || -> std::io::Result<()> {
        stdin_handle.write_all(content.as_bytes())
        // stdin_handle is dropped here, sending EOF to the child.
    });

    // Collect all stdout while the write thread is running.
    let output = child
        .wait_with_output()
        .with_context(|| format!("'{format_binary}' wait_with_output failed"))?;

    // Check exit status BEFORE the write-thread join. A non-zero exit (e.g.
    // `zig fmt --stdin` on a syntax error) can close the child's stdin before
    // the write finishes, so the write thread sees a broken pipe — that is not
    // a real error, it is the tool rejecting input. Reap the thread without
    // propagating and preserve the file unchanged rather than risk data loss.
    if !output.status.success() {
        let _ = write_thread.join();
        return Ok(FormatOutput::Unchanged);
    }

    // Exit was clean — a write error here is genuinely unexpected.
    write_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdin write thread panicked for '{format_binary}'"))?
        .with_context(|| format!("failed to write to '{format_binary}' stdin"))?;

    let formatted = String::from_utf8(output.stdout)
        .with_context(|| format!("'{format_binary}' produced non-UTF-8 output"))?;

    if formatted == src.content.as_ref() {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}
