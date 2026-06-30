//! Static tool specs and per-tool state (probe locks, version key locks, notice guards).
//!
//! Each tool is described by a [`ToolSpec`]. Per-tool probe results, version key
//! strings, and fallback notice guards are stored in `OnceLock`/`Once` statics
//! to be computed at most once per process.

use std::sync::{Once, OnceLock};

// ---------------------------------------------------------------------------
// ToolSpec
// ---------------------------------------------------------------------------

/// Static description of one native CLI tool's contract.
///
/// A spec can declare format capability, lint capability, or (theoretically)
/// both. Current implementations are either format-only (gofmt, rustfmt, zig
/// fmt, shfmt) or lint-only (shellcheck).
pub(crate) struct ToolSpec {
    /// Stable engine id used in config table keys and the cache key.
    pub(crate) engine_name: &'static str,
    /// Binary for format operations. `None` for lint-only tools.
    pub(crate) format_binary: Option<&'static str>,
    /// Arguments prepended before reading stdin for the format operation.
    pub(crate) format_args: &'static [&'static str],
    /// Whether to prepend `-i {indent_width}` to format args. Used by shfmt.
    pub(crate) format_indent_flag: bool,
    /// Binary for lint operations. `None` for format-only tools.
    pub(crate) lint_binary: Option<&'static str>,
    /// Arguments for the lint operation (e.g. `--format=json1 -`).
    pub(crate) lint_args: &'static [&'static str],
    /// Binary used for the version probe (may differ from the format binary).
    pub(crate) version_binary: &'static str,
    /// Arguments for the version probe command.
    pub(crate) version_args: &'static [&'static str],
    /// Whether the tool is **default-on** when detected on `PATH`. The
    /// canonical first-party formatters (`rustfmt`, `gofmt`) set this; other
    /// tools (e.g. `zig fmt`, `shfmt`, `shellcheck`) stay opt-in (`false`).
    pub(crate) default_on: bool,
    /// Whether the tool accepts an `--edition <year>` flag whose value is
    /// resolved from the file's `Cargo.toml`. Only `rustfmt` sets this: without
    /// it rustfmt defaults to edition 2015 and reformats edition-2024 source
    /// that `cargo fmt` (which passes the manifest edition) considers clean.
    /// `gofmt` / `zig fmt` / `shfmt` have no edition concept (`false`).
    pub(crate) edition_flag: bool,
    /// Whether to inject `--config max_width=<line_length>` before the static
    /// format args. Only `rustfmt` sets this: without it rustfmt defaults to
    /// 100 columns, violating poly's opinionated 120-column policy.
    pub(crate) max_width_flag: bool,
}

impl ToolSpec {
    /// The binary to spawn for the initial presence-probe.
    pub(crate) fn probe_binary(&self) -> &'static str {
        self.format_binary
            .or(self.lint_binary)
            .expect("ToolSpec must have at least one binary (format or lint)")
    }
}

// ---------------------------------------------------------------------------
// Per-tool specs
// ---------------------------------------------------------------------------

/// `gofmt`: reads stdin unconditionally; no flags needed. Canonical Go
/// formatter — **default-on** when found on `PATH`.
pub(crate) static GOFMT_SPEC: ToolSpec = ToolSpec {
    engine_name: "gofmt",
    format_binary: Some("gofmt"),
    format_args: &[],
    format_indent_flag: false,
    lint_binary: None,
    lint_args: &[],
    // gofmt has no --version flag; use `go version` which ships alongside gofmt.
    version_binary: "go",
    version_args: &["version"],
    default_on: true,
    edition_flag: false,
    max_width_flag: false,
};

/// `rustfmt --emit=stdout`: reads stdin, writes to stdout. Canonical Rust
/// formatter — **default-on** when found on `PATH`.
pub(crate) static RUSTFMT_SPEC: ToolSpec = ToolSpec {
    engine_name: "rustfmt",
    format_binary: Some("rustfmt"),
    format_args: &["--emit=stdout"],
    format_indent_flag: false,
    lint_binary: None,
    lint_args: &[],
    version_binary: "rustfmt",
    version_args: &["--version"],
    default_on: true,
    edition_flag: true,
    // Inject --config max_width=<line_length> to honour poly's 120-column
    // opinionated default; rustfmt's own default is 100.
    max_width_flag: true,
};

/// `zig fmt --stdin`: reads stdin, writes to stdout. Opt-in (off by default).
pub(crate) static ZIGFMT_SPEC: ToolSpec = ToolSpec {
    engine_name: "zigfmt",
    format_binary: Some("zig"),
    format_args: &["fmt", "--stdin"],
    format_indent_flag: false,
    lint_binary: None,
    lint_args: &[],
    version_binary: "zig",
    version_args: &["version"],
    default_on: false,
    edition_flag: false,
    max_width_flag: false,
};

/// `shfmt -`: reads stdin, writes formatted shell source to stdout. Opt-in
/// (off by default). Third-party tool (mvdan.cc/sh) — not a first-party
/// canonical toolchain — so it mirrors zig fmt's opt-in policy rather than
/// gofmt/rustfmt's default-on policy.
pub(crate) static SHFMT_SPEC: ToolSpec = ToolSpec {
    engine_name: "shfmt",
    format_binary: Some("shfmt"),
    // `-` tells shfmt to read from stdin; `-i N` is prepended dynamically
    // via format_indent_flag when format_via_tool is called.
    format_args: &["-"],
    format_indent_flag: true,
    lint_binary: None,
    lint_args: &[],
    version_binary: "shfmt",
    version_args: &["--version"],
    default_on: false,
    edition_flag: false,
    max_width_flag: false,
};

/// `shellcheck --format=json1 -`: reads shell source from stdin, emits a
/// JSON1 object `{ "comments": [...] }` to stdout. Opt-in (off by default).
/// Exit 0 → no issues; exit 1 → issues found; exit 2+ → tool error.
pub(crate) static SHELLCHECK_SPEC: ToolSpec = ToolSpec {
    engine_name: "shellcheck",
    format_binary: None,
    format_args: &[],
    format_indent_flag: false,
    lint_binary: Some("shellcheck"),
    // `--format=json1` → JSON output; `-` → read from stdin.
    lint_args: &["--format=json1", "-"],
    version_binary: "shellcheck",
    version_args: &["--version"],
    default_on: false,
    edition_flag: false,
    max_width_flag: false,
};

// ---------------------------------------------------------------------------
// Per-tool probe caches (process lifetime, one per tool)
// ---------------------------------------------------------------------------

/// `Some(version)` = `gofmt` found on PATH; `None` = absent.
pub(crate) static GOFMT_PROBE: OnceLock<Option<String>> = OnceLock::new();
/// `Some(version)` = `rustfmt` found on PATH; `None` = absent.
pub(crate) static RUSTFMT_PROBE: OnceLock<Option<String>> = OnceLock::new();
/// `Some(version)` = `zig` found on PATH; `None` = absent.
pub(crate) static ZIGFMT_PROBE: OnceLock<Option<String>> = OnceLock::new();
/// `Some(version)` = `shfmt` found on PATH; `None` = absent.
pub(crate) static SHFMT_PROBE: OnceLock<Option<String>> = OnceLock::new();
/// `Some(version)` = `shellcheck` found on PATH; `None` = absent.
pub(crate) static SHELLCHECK_PROBE: OnceLock<Option<String>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Version cache-key strings (per tool)
// ---------------------------------------------------------------------------

/// Folds in the native tool version AND the tree-sitter engine version, because
/// the disabled/absent path delegates to tier-2.
pub(crate) static GOFMT_KEY: OnceLock<String> = OnceLock::new();
pub(crate) static RUSTFMT_KEY: OnceLock<String> = OnceLock::new();
pub(crate) static ZIGFMT_KEY: OnceLock<String> = OnceLock::new();
pub(crate) static SHFMT_KEY: OnceLock<String> = OnceLock::new();
/// Folds in the shellcheck version AND the tree-sitter engine version, because
/// the lint path always includes a TreeSitterEngine.lint() delegation.
pub(crate) static SHELLCHECK_KEY: OnceLock<String> = OnceLock::new();

// ---------------------------------------------------------------------------
// Tier-2 fallback notice guards (format-only engines)
// ---------------------------------------------------------------------------

// Guards the "falling back to the generic tier" info notice so it fires at most
// once per language per process run, never once per file (the format() path runs
// inside the per-file rayon loop).
//
// Lint-only engines (shellcheck) do not emit a fallback notice because absent
// shellcheck silently omits shell-specific diagnostics — the TS tier still runs
// for whitespace/generic checks. There is nothing unexpected about a machine
// without shellcheck installed.

pub(crate) static GOFMT_NOTICE: Once = Once::new();
pub(crate) static RUSTFMT_NOTICE: Once = Once::new();
pub(crate) static ZIGFMT_NOTICE: Once = Once::new();
pub(crate) static SHFMT_NOTICE: Once = Once::new();

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustfmt_spec_has_max_width_flag() {
        // Verifies that the max_width_flag is correctly set so that
        // format_via_tool injects --config max_width=<line_length> for rustfmt.
        assert!(
            RUSTFMT_SPEC.max_width_flag,
            "RUSTFMT_SPEC.max_width_flag must be true to enforce the 120-column policy"
        );
    }

    #[test]
    fn other_specs_have_no_max_width_flag() {
        assert!(
            !GOFMT_SPEC.max_width_flag,
            "gofmt does not support max_width_flag"
        );
        assert!(
            !ZIGFMT_SPEC.max_width_flag,
            "zigfmt does not support max_width_flag"
        );
        assert!(
            !SHFMT_SPEC.max_width_flag,
            "shfmt does not support max_width_flag"
        );
        assert!(
            !SHELLCHECK_SPEC.max_width_flag,
            "shellcheck does not support max_width_flag"
        );
    }
}
