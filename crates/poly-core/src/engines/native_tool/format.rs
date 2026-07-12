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
/// stdin is always written from a dedicated OS thread while this thread drains
/// stdout via `wait_with_output`. Feeding stdin and draining stdout concurrently
/// prevents the classic pipe-buffer deadlock: a tool that emits to stdout while
/// still reading stdin can fill its stdout pipe and block on the write, which —
/// if this thread were itself blocked in a `write_all` on stdin instead of
/// draining — would wedge both processes forever. Writing stdin inline on this
/// thread (a former fast path for small inputs) is unsound for exactly that
/// reason and must not be reintroduced.
pub(crate) fn format_via_tool(spec: &ToolSpec, src: &SourceFile, indent_width: usize) -> anyhow::Result<FormatOutput> {
    let format_binary = spec
        .format_binary
        .expect("format_via_tool called on a lint-only ToolSpec");

    let mut cmd = Command::new(format_binary);

    if spec.format_indent_flag {
        cmd.arg("-i");
        cmd.arg(indent_width.to_string());
    }
    if spec.edition_flag {
        cmd.arg("--edition");
        cmd.arg(super::edition::resolve_edition(&src.path));
    }
    if (spec.rustfmt_config_flag || spec.run_in_file_dir)
        && let Some(parent) = src.path.parent().filter(|p| p.is_dir())
    {
        cmd.current_dir(parent);
    }
    cmd.args(spec.format_args);

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn '{format_binary}'"))?;

    let content = std::sync::Arc::clone(&src.content);
    let mut stdin_handle = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("'{format_binary}' stdin pipe was not created"))?;

    let write_thread = thread::spawn(move || -> std::io::Result<()> { stdin_handle.write_all(content.as_bytes()) });

    let output = child
        .wait_with_output()
        .with_context(|| format!("'{format_binary}' wait_with_output failed"))?;

    let write_result = write_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdin write thread panicked for '{format_binary}'"))?;

    if !output.status.success() {
        return Ok(FormatOutput::Unchanged);
    }
    write_result.with_context(|| format!("failed to write to '{format_binary}' stdin"))?;

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

    /// Regression: stdin must be fed while stdout is drained, never before it.
    ///
    /// `tr a b` reads stdin and writes an equal-length transformed stream to
    /// stdout. A payload many times the OS pipe capacity (64 KiB on Linux) fills
    /// both pipes at once: unless this side drains stdout while a writer thread
    /// feeds stdin, the child blocks writing stdout, this side blocks writing
    /// stdin, and the two deadlock forever. The former "write small inputs inline
    /// before calling `wait_with_output`" fast path violated that invariant and
    /// could hang `poly hooks` intermittently; this locks in the fix.
    #[cfg(unix)]
    #[test]
    fn format_via_tool_drains_stdout_while_writing_stdin() {
        use std::sync::Arc;
        use std::sync::mpsc;
        use std::time::Duration;

        use super::super::spec::ToolSpec;
        use super::format_via_tool;
        use crate::engine::{FormatOutput, SourceFile};
        use crate::language::Language;

        static TR_SPEC: ToolSpec = ToolSpec {
            engine_name: "test-tr",
            format_binary: Some("tr"),
            format_args: &["a", "b"],
            format_indent_flag: false,
            lint_binary: None,
            lint_args: &[],
            version_binary: "tr",
            version_args: &[],
            default_on: false,
            edition_flag: false,
            rustfmt_config_flag: false,
            run_in_file_dir: false,
        };

        let size = 1 << 20; // 1 MiB — 16x the Linux pipe buffer, so both pipes fill.
        let content: Arc<str> = "a".repeat(size).into();
        let src = SourceFile {
            path: "big.txt".into(),
            language: Language::Rust,
            content,
        };

        // Run under a watchdog so a reintroduced deadlock fails the test instead
        // of hanging the whole suite.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(format_via_tool(&TR_SPEC, &src, 4));
        });
        let result = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("format_via_tool deadlocked: stdin was not drained concurrently with stdout")
            .expect("`tr a b` must succeed");

        match result {
            FormatOutput::Formatted(text) => {
                assert_eq!(text.len(), size, "the full transformed stream must be captured");
                assert!(
                    text.bytes().all(|byte| byte == b'b'),
                    "`tr a b` must transform every byte"
                );
            }
            FormatOutput::Unchanged => panic!("`tr a b` changes the content; expected Formatted"),
        }
    }
}
