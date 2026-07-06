//! Per-file format dispatch: pipe source content through a native formatter
//! CLI and collect its stdout output.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::Context;

use crate::engine::{FormatOutput, SourceFile};

use super::spec::ToolSpec;

/// Largest input written to the child's stdin **inline** on the current rayon
/// worker (no dedicated thread). Inputs up to this size fit within the OS pipe
/// buffer on every supported platform — macOS pipe capacity can be as small as
/// 16 KiB — so a single-threaded `write_all` cannot block waiting for the child
/// to drain, and there is no deadlock risk. This removes a per-file kernel
/// thread spawn/join from the hot path for the common case (most source files
/// are well under 8 KiB). Larger inputs fall back to a dedicated writer thread.
const STDIN_INLINE_LIMIT: usize = 8 * 1024;

/// How stdin was fed to the child: inline (small inputs) or via a writer thread
/// (large inputs, to avoid a pipe-buffer deadlock against `wait_with_output`).
enum StdinWriter {
    Inline(std::io::Result<()>),
    Thread(thread::JoinHandle<std::io::Result<()>>),
}

/// Pipe `src.content` through the format binary described by `spec`
/// (stdin → stdout).
///
/// `indent_width` is injected as `-i {indent_width}` when
/// `spec.format_indent_flag` is true (used by shfmt).
///
/// For rustfmt (`spec.rustfmt_config_flag`), the child runs in the source
/// file's directory so rustfmt discovers the governing `rustfmt.toml` itself;
/// with no project config, rustfmt applies its own defaults — poly's output
/// matches `cargo fmt` either way.
///
/// Returns:
/// - [`FormatOutput::Unchanged`] when the tool exits non-zero (syntax error
///   in the source — never corrupt the file), or when the output equals the
///   input byte-for-byte.
/// - [`FormatOutput::Formatted(s)`] when the tool produced different output.
///
/// # Deadlock prevention
///
/// Inputs up to [`STDIN_INLINE_LIMIT`] are written inline on the calling rayon
/// worker (they fit the OS pipe buffer, so the write cannot block). Larger
/// inputs are written from a dedicated OS thread while this thread drains stdout
/// via `wait_with_output`, preventing the pipe-buffer deadlock that can occur
/// when a formatter buffers all input before writing any output.
pub(crate) fn format_via_tool(spec: &ToolSpec, src: &SourceFile, indent_width: usize) -> anyhow::Result<FormatOutput> {
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
    // Pass `--edition <year>` resolved from the file's Cargo.toml when the tool
    // accepts it (rustfmt). Without this, rustfmt assumes edition 2015 and
    // reformats edition-2024 source that `cargo fmt` leaves clean.
    if spec.edition_flag {
        cmd.arg("--edition");
        cmd.arg(super::edition::resolve_edition(&src.path));
    }
    // Anchor the child process to the source file's directory when the spec
    // needs config-file discovery rooted at the file's location:
    // - rustfmt_config_flag: rustfmt reads rustfmt.toml walking up from cwd.
    // - run_in_file_dir: swift-format discovers .swift-format the same way.
    // Without anchoring, both tools discover config relative to poly's own cwd.
    if (spec.rustfmt_config_flag || spec.run_in_file_dir)
        && let Some(parent) = src.path.parent().filter(|p| p.is_dir())
    {
        cmd.current_dir(parent);
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

    // Feed stdin without deadlocking against our own `wait_with_output`:
    // small inputs fit the pipe buffer, so write them inline (no thread spawn);
    // larger inputs are written from a dedicated thread that runs while we drain
    // stdout, since the child may buffer all input before emitting any output.
    let writer = if content.len() <= STDIN_INLINE_LIMIT {
        let result = stdin_handle.write_all(content.as_bytes());
        drop(stdin_handle); // send EOF to the child
        StdinWriter::Inline(result)
    } else {
        StdinWriter::Thread(thread::spawn(move || -> std::io::Result<()> {
            stdin_handle.write_all(content.as_bytes())
            // stdin_handle is dropped here, sending EOF to the child.
        }))
    };

    // Collect all stdout (while the writer thread, if any, is running).
    let output = child
        .wait_with_output()
        .with_context(|| format!("'{format_binary}' wait_with_output failed"))?;

    // Check exit status BEFORE inspecting the write outcome. A non-zero exit
    // (e.g. `zig fmt --stdin` on a syntax error) can close the child's stdin
    // before the write finishes, so the writer sees a broken pipe — that is not
    // a real error, it is the tool rejecting input. Discard the write outcome
    // and preserve the file unchanged rather than risk data loss.
    if !output.status.success() {
        if let StdinWriter::Thread(handle) = writer {
            let _ = handle.join();
        }
        return Ok(FormatOutput::Unchanged);
    }

    // Exit was clean — a write error here is genuinely unexpected.
    match writer {
        StdinWriter::Inline(result) => {
            result.with_context(|| format!("failed to write to '{format_binary}' stdin"))?;
        }
        StdinWriter::Thread(handle) => {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("stdin write thread panicked for '{format_binary}'"))?
                .with_context(|| format!("failed to write to '{format_binary}' stdin"))?;
        }
    }

    let formatted = normalize_newlines(
        String::from_utf8(output.stdout).with_context(|| format!("'{format_binary}' produced non-UTF-8 output"))?,
    );

    if formatted == src.content.as_ref() {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}

/// Normalize a native tool's stdout to LF line endings.
///
/// Some first-party CLIs emit CRLF on Windows (e.g. `Rscript`/styler), which
/// would make output platform-dependent and diverge from poly's LF default.
/// Collapsing `\r\n` to `\n` keeps formatted output identical across hosts; it is
/// a no-op on Unix, where the tool already emits LF, so no allocation happens.
fn normalize_newlines(formatted: String) -> String {
    if formatted.contains('\r') {
        formatted.replace("\r\n", "\n")
    } else {
        formatted
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_newlines;

    #[test]
    fn crlf_output_is_normalized_to_lf() {
        assert_eq!(
            normalize_newlines("x <- 1\r\ny <- 2\r\n".to_string()),
            "x <- 1\ny <- 2\n"
        );
    }

    #[test]
    fn lf_output_is_left_untouched() {
        assert_eq!(normalize_newlines("x <- 1\ny <- 2\n".to_string()), "x <- 1\ny <- 2\n");
    }
}
