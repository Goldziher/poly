//! Native toolchain backends: thin wrappers around first-party formatter CLIs
//! (`gofmt` for Go, `rustfmt` for Rust, `zig fmt` for Zig).
//!
//! ## Default-on for canonical toolchains (ADR 0014 amendment)
//!
//! The **canonical** first-party formatters — `rustfmt` (Rust) and `gofmt`
//! (Go) — are **default-on when the tool is detected on `PATH`**. When present,
//! `poly fmt` formats those languages through the real tool instead of the
//! lower-fidelity tree-sitter generic tier; when absent, the language falls
//! through to the generic tier and an **info-level** notice is emitted once per
//! language per run. This preserves the zero-system-dependency guarantee (a
//! missing toolchain is never an error) while fixing the measured tier-2 churn
//! against `rustfmt`.
//!
//! Other tools (currently `zig fmt`) remain **opt-in, off by default**; a user
//! enables them per language in `poly.toml`. The default-on tools can also be
//! forced off the same way:
//!
//! ```toml
//! # force a canonical tool off (fall back to the generic tier)
//! [fmt.rust.rustfmt]
//! enabled = false
//!
//! # opt a non-canonical tool in
//! [fmt.zig.zigfmt]
//! enabled = true
//! ```
//!
//! When disabled or when the tool is not found on `PATH`, this engine
//! transparently delegates to the tree-sitter generic tier — so the
//! zero-dependency guarantee is preserved and the output is byte-identical to
//! the tier-2 behaviour. When active and the tool is present, the engine pipes
//! the file content through the tool's `stdin → stdout` interface and returns
//! the formatted result.
//!
//! ## Design rationale: registry slot vs. sequence position
//!
//! `NativeToolEngine` is registered in the registry *instead of*
//! `TreeSitterEngine` for its three languages. Placing both engines in the
//! sequence would cause double-formatting when the native tool is active (the
//! runner iterates all format-capable engines in order). Internal delegation
//! avoids this: when disabled or absent, `format()` calls
//! `TreeSitterEngine::format()` directly, so exactly one formatter always runs
//! per file.
//!
//! `capabilities().format` is always `true` for the same reason: if it were
//! `false` when disabled, the runner would skip this engine but
//! `TreeSitterEngine` would not be in the sequence either, leaving the language
//! unformatted.
//!
//! ## Subprocess I/O safety
//!
//! A dedicated OS thread writes stdin while the main (rayon) worker thread
//! collects stdout via `wait_with_output`. This prevents the pipe-buffer
//! deadlock that can occur for source files larger than the OS pipe buffer
//! (~64 KB on Linux) when a formatter buffers all input before writing output.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Once, OnceLock};
use std::thread;

use anyhow::Context;
use tracing::info;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::engines::treesitter::TreeSitterEngine;
use crate::language::Language;

// ---------------------------------------------------------------------------
// Tool spec
// ---------------------------------------------------------------------------

/// Static description of one native CLI formatter's contract.
struct ToolSpec {
    /// Stable engine id, used in config-table keys and the cache key.
    engine_name: &'static str,
    /// The executable to invoke for formatting; must accept source on stdin
    /// and write formatted output to stdout.
    format_binary: &'static str,
    /// Arguments prepended before reading stdin (e.g. `--emit=stdout`).
    format_args: &'static [&'static str],
    /// Executable to run for the version probe.
    version_binary: &'static str,
    /// Arguments for the version probe.
    version_args: &'static [&'static str],
    /// Whether this tool is **default-on** when detected on `PATH`. The
    /// canonical first-party formatters (`rustfmt`, `gofmt`) set this; other
    /// tools (e.g. `zig fmt`) stay opt-in (`false`). A user can always override
    /// either way via `[fmt.<lang>.<tool>] enabled = …`.
    default_on: bool,
}

// ---------------------------------------------------------------------------
// Per-tool specs
// ---------------------------------------------------------------------------

/// `gofmt`: reads stdin unconditionally; no flags needed. Canonical Go
/// formatter → **default-on** when found on `PATH`.
static GOFMT_SPEC: ToolSpec = ToolSpec {
    engine_name: "gofmt",
    format_binary: "gofmt",
    format_args: &[],
    // gofmt has no --version flag; use `go version` which ships alongside gofmt.
    version_binary: "go",
    version_args: &["version"],
    default_on: true,
};

/// `rustfmt --emit=stdout`: reads stdin, writes to stdout. Canonical Rust
/// formatter → **default-on** when found on `PATH`.
static RUSTFMT_SPEC: ToolSpec = ToolSpec {
    engine_name: "rustfmt",
    format_binary: "rustfmt",
    format_args: &["--emit=stdout"],
    version_binary: "rustfmt",
    version_args: &["--version"],
    default_on: true,
};

/// `zig fmt --stdin`: reads stdin, writes to stdout. Opt-in (off by default).
static ZIGFMT_SPEC: ToolSpec = ToolSpec {
    engine_name: "zigfmt",
    format_binary: "zig",
    format_args: &["fmt", "--stdin"],
    version_binary: "zig",
    version_args: &["version"],
    default_on: false,
};

// ---------------------------------------------------------------------------
// Per-tool probe caches (process lifetime, one per tool)
// ---------------------------------------------------------------------------

/// `Some(version)` = `gofmt` found on PATH; `None` = absent.
static GOFMT_PROBE: OnceLock<Option<String>> = OnceLock::new();
/// `Some(version)` = `rustfmt` found on PATH; `None` = absent.
static RUSTFMT_PROBE: OnceLock<Option<String>> = OnceLock::new();
/// `Some(version)` = `zig` found on PATH; `None` = absent.
static ZIGFMT_PROBE: OnceLock<Option<String>> = OnceLock::new();

// Cache-key version strings (per tool). These fold in the tree-sitter engine's
// version because every disabled/absent path delegates formatting to it, so a
// tier-2 upgrade must invalidate cached native-tool results too.
static GOFMT_KEY: OnceLock<String> = OnceLock::new();
static RUSTFMT_KEY: OnceLock<String> = OnceLock::new();
static ZIGFMT_KEY: OnceLock<String> = OnceLock::new();

// Guards the "falling back to the generic tier" info notice so it fires at most
// once per language per process run, never once per file (the format() path runs
// inside the per-file rayon loop).
static GOFMT_NOTICE: Once = Once::new();
static RUSTFMT_NOTICE: Once = Once::new();
static ZIGFMT_NOTICE: Once = Once::new();

// ---------------------------------------------------------------------------
// Per-language static slices for Engine::languages
// ---------------------------------------------------------------------------

static GO_LANGUAGES: &[Language] = &[Language::Go];
static RUST_LANGUAGES: &[Language] = &[Language::Rust];
static ZIG_LANGUAGES: &[Language] = &[Language::Zig];

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Tier-3 opt-in native tool formatter. One instance is registered per
/// language; see the module docs for the enabled/disabled/absent semantics.
pub struct NativeToolEngine {
    language: Language,
}

impl NativeToolEngine {
    /// Construct a `NativeToolEngine` for `language`.
    ///
    /// # Panics
    ///
    /// Panics if `language` is not one of `Go`, `Rust`, or `Zig`; those are
    /// the only languages this backend supports.
    pub fn for_language(language: Language) -> Self {
        assert!(
            matches!(language, Language::Go | Language::Rust | Language::Zig),
            "NativeToolEngine only supports Go, Rust, and Zig; got {:?}",
            language
        );
        NativeToolEngine { language }
    }

    fn spec(&self) -> &'static ToolSpec {
        match &self.language {
            Language::Go => &GOFMT_SPEC,
            Language::Rust => &RUSTFMT_SPEC,
            Language::Zig => &ZIGFMT_SPEC,
            _ => unreachable!("NativeToolEngine only handles Go, Rust, and Zig"),
        }
    }

    fn probe_lock(&self) -> &'static OnceLock<Option<String>> {
        match &self.language {
            Language::Go => &GOFMT_PROBE,
            Language::Rust => &RUSTFMT_PROBE,
            Language::Zig => &ZIGFMT_PROBE,
            _ => unreachable!("NativeToolEngine only handles Go, Rust, and Zig"),
        }
    }

    fn key_lock(&self) -> &'static OnceLock<String> {
        match &self.language {
            Language::Go => &GOFMT_KEY,
            Language::Rust => &RUSTFMT_KEY,
            Language::Zig => &ZIGFMT_KEY,
            _ => unreachable!("NativeToolEngine only handles Go, Rust, and Zig"),
        }
    }

    fn notice_lock(&self) -> &'static Once {
        match &self.language {
            Language::Go => &GOFMT_NOTICE,
            Language::Rust => &RUSTFMT_NOTICE,
            Language::Zig => &ZIGFMT_NOTICE,
            _ => unreachable!("NativeToolEngine only handles Go, Rust, and Zig"),
        }
    }

    /// Whether the native tool is *wanted* for this run: the user's explicit
    /// `enabled = …` if present, otherwise the tool's `default_on` policy
    /// (`true` for the canonical `rustfmt` / `gofmt`, `false` for opt-in tools).
    fn is_enabled(&self, cfg: &EngineConfig) -> bool {
        cfg.options
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(self.spec().default_on)
    }

    /// Emit the tier-2 fallback notice at most once per language per run.
    ///
    /// Only fires when the tool was *wanted* (enabled / default-on) but is
    /// absent from `PATH` — the measured pain case. An explicit `enabled =
    /// false` is the user's choice and stays silent; presence of the tool means
    /// no fallback happens at all.
    fn notify_tier2_fallback(&self, cfg: &EngineConfig) {
        if should_notify_fallback(self.is_enabled(cfg), self.probed_version().is_some()) {
            let spec = self.spec();
            self.notice_lock().call_once(|| {
                info!(
                    language = self.language.id(),
                    tool = spec.format_binary,
                    "{} not found on PATH; formatting via the generic tree-sitter tier (lower fidelity)",
                    spec.format_binary
                );
            });
        }
    }

    /// Returns the probed version string, or `None` when the tool is absent.
    ///
    /// The result is memoised in a static `OnceLock`; subsequent calls within
    /// the same process are free.
    fn probed_version(&self) -> Option<&'static str> {
        self.probe_lock()
            .get_or_init(|| probe_tool(self.spec()))
            .as_deref()
    }

    /// Whether the underlying native tool is installed on this host. When
    /// `false`, the engine delegates to the tier-2 tree-sitter formatter.
    pub fn is_available(&self) -> bool {
        self.probed_version().is_some()
    }
}

impl Engine for NativeToolEngine {
    fn name(&self) -> &'static str {
        self.spec().engine_name
    }

    fn languages(&self) -> &'static [Language] {
        match &self.language {
            Language::Go => GO_LANGUAGES,
            Language::Rust => RUST_LANGUAGES,
            Language::Zig => ZIG_LANGUAGES,
            _ => unreachable!(),
        }
    }

    /// Both `lint` and `format` capabilities are always `true`.
    ///
    /// `NativeToolEngine` holds the sole registry slot for its language (no
    /// `TreeSitterEngine` in the sequence). It delegates both `lint` and
    /// `format` to `TreeSitterEngine` when the native tool is disabled or
    /// absent, so the language is never left without either capability.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: false,
        }
    }

    /// Cache-key version. Folds in BOTH the native tool's version (or an
    /// `absent` sentinel) AND the tree-sitter engine's version, because the
    /// disabled/absent path delegates formatting to tier-2 — so a tier-2 upgrade
    /// must invalidate cached native-tool results just as a tool upgrade does.
    fn version(&self) -> &str {
        self.key_lock().get_or_init(|| {
            let ts = TreeSitterEngine.version();
            match self.probed_version() {
                Some(tool) => format!("{tool} | ts:{ts}"),
                None => format!("native-tool:absent | ts:{ts}"),
            }
        })
    }

    /// Lint by delegating unconditionally to [`TreeSitterEngine`].
    ///
    /// The native tools in this tier (gofmt, rustfmt, zig fmt) are
    /// format-only. Textual checks (trailing whitespace, etc.) are provided by
    /// the tier-2 tree-sitter engine via delegation.
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        TreeSitterEngine.lint(src, cfg)
    }

    /// Format via the native tool when it is wanted and present on `PATH`;
    /// otherwise delegate to [`TreeSitterEngine`] (tier-2 fallback).
    ///
    /// "Wanted" defaults to the tool's `default_on` policy: the canonical
    /// `rustfmt` / `gofmt` are on by default, opt-in tools are off. The user
    /// overrides this via `[fmt.<lang>.<tool>] enabled = …` in `poly.toml`.
    ///
    /// When a wanted tool is absent, this is never an error: the language
    /// degrades to the generic tier and an info-level notice is emitted once
    /// per language per run (see [`NativeToolEngine::notify_tier2_fallback`]).
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        // Degrade gracefully: not wanted OR tool not on PATH → tier-2.
        if !self.is_enabled(cfg) || self.probed_version().is_none() {
            self.notify_tier2_fallback(cfg);
            return TreeSitterEngine.format(src, cfg);
        }

        format_via_tool(self.spec(), src)
    }
}

/// Decide whether the tier-2 fallback info notice should fire: the tool was
/// wanted (`enabled` / default-on) but is not present on `PATH`. Pure so it can
/// be unit-tested without a real toolchain.
fn should_notify_fallback(wanted: bool, present: bool) -> bool {
    wanted && !present
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// Determine if the format binary exists on `PATH` and return its version.
///
/// Returns `Some(version_string)` on success, `None` when the binary cannot
/// be spawned.
fn probe_tool(spec: &ToolSpec) -> Option<String> {
    // Spawn the format binary with all I/O null to verify presence.
    // gofmt / rustfmt / `zig fmt` all exit cleanly on empty (EOF) stdin, so
    // this produces no side effects.
    let mut child = Command::new(spec.format_binary)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?; // None → binary not on PATH
    let _ = child.wait(); // Reap the child to avoid zombies

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
        .unwrap_or_else(|| format!("{}:found", spec.format_binary));

    Some(raw)
}

// ---------------------------------------------------------------------------
// Subprocess I/O
// ---------------------------------------------------------------------------

/// Pipe `src.content` through `spec.format_binary` (stdin → stdout).
///
/// Spawns the tool, writes source bytes from a dedicated thread, and collects
/// the formatted output via `wait_with_output`. Returning
/// [`FormatOutput::Unchanged`] on a non-zero exit code ensures a source file
/// with a syntax error is never corrupted.
fn format_via_tool(spec: &ToolSpec, src: &SourceFile) -> anyhow::Result<FormatOutput> {
    let mut child = Command::new(spec.format_binary)
        .args(spec.format_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Suppress tool diagnostics: non-zero exit is the failure signal.
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn '{}'", spec.format_binary))?;

    // Clone the Arc<str> — a reference-count bump, not a copy of the bytes.
    let content = std::sync::Arc::clone(&src.content);
    let mut stdin_handle = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("'{}' stdin pipe was not created", spec.format_binary))?;

    // Write in a separate thread to prevent a deadlock that would occur if the
    // child's stdout pipe fills before we have read any of it.
    let write_thread = thread::spawn(move || -> std::io::Result<()> {
        stdin_handle.write_all(content.as_bytes())
        // stdin_handle is dropped here, sending EOF to the child.
    });

    // Collect all stdout while the write thread is running.
    let output = child
        .wait_with_output()
        .with_context(|| format!("'{}' wait_with_output failed", spec.format_binary))?;

    // Check exit status BEFORE the write-thread join. A non-zero exit (e.g.
    // `zig fmt --stdin` on a syntax error) can close the child's stdin before the
    // write finishes, so the write thread sees a broken pipe — that is not a real
    // error, it is the tool rejecting input. Reap the thread without propagating
    // and preserve the file unchanged rather than risk data loss.
    if !output.status.success() {
        let _ = write_thread.join();
        return Ok(FormatOutput::Unchanged);
    }

    // Exit was clean — a write error here is genuinely unexpected, so surface it.
    write_thread
        .join()
        .map_err(|_| anyhow::anyhow!("stdin write thread panicked for '{}'", spec.format_binary))?
        .with_context(|| format!("failed to write to '{}' stdin", spec.format_binary))?;

    let formatted = String::from_utf8(output.stdout)
        .with_context(|| format!("'{}' produced non-UTF-8 output", spec.format_binary))?;

    if formatted == src.content.as_ref() {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language,
            content: content.into(),
        }
    }

    /// Empty options → the tool's `default_on` policy decides (canonical tools
    /// on, opt-in tools off). This is the out-of-the-box config.
    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::Table::new(),
        }
    }

    /// Explicit `enabled = false` — forces tier-2 fallback for any tool,
    /// including the default-on canonical ones.
    fn disabled_cfg() -> EngineConfig {
        bool_cfg(false)
    }

    /// Explicit `enabled = true` — forces the native tool on (subject to it
    /// being present on `PATH`).
    fn enabled_cfg() -> EngineConfig {
        bool_cfg(true)
    }

    fn bool_cfg(enabled: bool) -> EngineConfig {
        let mut options = toml::Table::new();
        options.insert("enabled".to_string(), toml::Value::Boolean(enabled));
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options,
        }
    }

    // ---------------------------------------------------------------------------
    // Metadata checks
    // ---------------------------------------------------------------------------

    #[test]
    fn engine_metadata_go() {
        let engine = NativeToolEngine::for_language(Language::Go);
        assert_eq!(engine.name(), "gofmt");
        assert_eq!(engine.languages(), &[Language::Go]);
        assert!(engine.capabilities().format);
        // lint delegates to TreeSitterEngine
        assert!(engine.capabilities().lint);
        assert!(!engine.capabilities().fix);
    }

    #[test]
    fn engine_metadata_rust() {
        let engine = NativeToolEngine::for_language(Language::Rust);
        assert_eq!(engine.name(), "rustfmt");
        assert_eq!(engine.languages(), &[Language::Rust]);
        assert!(engine.capabilities().format);
    }

    #[test]
    fn engine_metadata_zig() {
        let engine = NativeToolEngine::for_language(Language::Zig);
        assert_eq!(engine.name(), "zigfmt");
        assert_eq!(engine.languages(), &[Language::Zig]);
        assert!(engine.capabilities().format);
    }

    #[test]
    fn lint_clean_go_produces_no_diags() {
        // lint() delegates to TreeSitterEngine; clean Go (no trailing whitespace,
        // no issues) should produce no diagnostics.
        let engine = NativeToolEngine::for_language(Language::Go);
        let src = make_src("main.go", Language::Go, "package main\n");
        let diags = engine.lint(&src, &disabled_cfg()).unwrap();
        assert!(
            diags.is_empty(),
            "clean Go source should produce no diagnostics via tree-sitter delegation"
        );
    }

    #[test]
    fn lint_go_with_trailing_whitespace_flagged() {
        // TreeSitterEngine detects trailing whitespace; delegation must propagate it.
        let engine = NativeToolEngine::for_language(Language::Go);
        let src = make_src("main.go", Language::Go, "package main   \nfunc main() {}\n");
        let diags = engine.lint(&src, &disabled_cfg()).unwrap();
        assert!(
            !diags.is_empty(),
            "trailing whitespace in Go source must be flagged via tree-sitter delegation"
        );
        assert_eq!(diags[0].code.as_deref(), Some("trailing-whitespace"));
    }

    // ---------------------------------------------------------------------------
    // Disabled/absent → tier-2 fallback (default-off invariant)
    // ---------------------------------------------------------------------------

    /// When the engine is disabled (default), `format()` must delegate to
    /// `TreeSitterEngine`. The output must be byte-identical to calling
    /// `TreeSitterEngine::format()` directly — no double-format, no diff.
    #[test]
    fn disabled_go_delegates_to_tier2() {
        const SRC: &str = "package main\nimport \"fmt\"\nfunc main() {\nfmt.Println(\"hi\")\n}\n";
        let engine = NativeToolEngine::for_language(Language::Go);
        let src = make_src("main.go", Language::Go, SRC);

        // NativeToolEngine with enabled=false
        let native_result = engine.format(&src, &disabled_cfg()).unwrap();

        // Direct TreeSitterEngine call (the canonical tier-2 output)
        let ts_result = TreeSitterEngine.format(&src, &disabled_cfg()).unwrap();

        let native_out = match native_result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => SRC.to_string(),
        };
        let ts_out = match ts_result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => SRC.to_string(),
        };

        assert_eq!(
            native_out, ts_out,
            "disabled NativeToolEngine must produce byte-identical output to TreeSitterEngine"
        );
    }

    // ---------------------------------------------------------------------------
    // Enabled + tool present → native output
    // ---------------------------------------------------------------------------

    /// Known-unformatted Go: gofmt should add tabs and blank lines.
    /// Skipped when `gofmt` is not on PATH so CI without the Go toolchain passes.
    #[test]
    fn go_native_formats_unformatted_source() {
        let engine = NativeToolEngine::for_language(Language::Go);
        if engine.probed_version().is_none() {
            eprintln!("gofmt not found on PATH — skipping go_native_formats_unformatted_source");
            return;
        }

        const UNFORMATTED: &str =
            "package main\nimport \"fmt\"\nfunc main() {\nfmt.Println(\"hello\")\n}\n";
        let src = make_src("main.go", Language::Go, UNFORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();

        let formatted = match result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => {
                panic!("expected gofmt to reformat the unformatted source")
            }
        };

        // gofmt must add a blank line between the import and func declaration
        // and indent the body with a tab.
        assert!(
            formatted.contains("\nfunc main()"),
            "gofmt output should contain a blank line before func"
        );
        assert!(
            formatted.contains("\tfmt.Println"),
            "gofmt output should use tab indentation"
        );

        insta::assert_snapshot!("go_native_known_unformatted", formatted);
    }

    /// Known-unformatted Rust: rustfmt should add spaces and normalize braces.
    /// Skipped when `rustfmt` is not on PATH.
    #[test]
    fn rust_native_formats_unformatted_source() {
        let engine = NativeToolEngine::for_language(Language::Rust);
        if engine.probed_version().is_none() {
            eprintln!(
                "rustfmt not found on PATH — skipping rust_native_formats_unformatted_source"
            );
            return;
        }

        const UNFORMATTED: &str = "fn main(){println!(\"hello\");let x=1+2;}\n";
        let src = make_src("main.rs", Language::Rust, UNFORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();

        let formatted = match result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => {
                panic!("expected rustfmt to reformat the unformatted source")
            }
        };

        assert!(
            formatted.contains("fn main() {"),
            "rustfmt output should expand the function signature"
        );

        insta::assert_snapshot!("rust_native_known_unformatted", formatted);
    }

    /// Known-unformatted Zig: zig fmt should add consistent indentation.
    /// Skipped when `zig` is not on PATH.
    #[test]
    fn zig_native_formats_unformatted_source() {
        let engine = NativeToolEngine::for_language(Language::Zig);
        if engine.probed_version().is_none() {
            eprintln!("zig not found on PATH — skipping zig_native_formats_unformatted_source");
            return;
        }

        const UNFORMATTED: &str =
            "const std = @import(\"std\");\npub fn main() void {\n_ = std;\n}\n";
        let src = make_src("main.zig", Language::Zig, UNFORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();

        let formatted = match result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => {
                // zig fmt might produce Unchanged if the source is already
                // acceptable — snapshot either way
                UNFORMATTED.to_string()
            }
        };

        insta::assert_snapshot!("zig_native_known_unformatted", formatted);
    }

    // ---------------------------------------------------------------------------
    // Idempotency: already-formatted input → Unchanged
    // ---------------------------------------------------------------------------

    /// Running the native tool on already-formatted source must return Unchanged.
    #[test]
    fn go_native_unchanged_on_already_formatted() {
        let engine = NativeToolEngine::for_language(Language::Go);
        if engine.probed_version().is_none() {
            eprintln!(
                "gofmt not found on PATH — skipping go_native_unchanged_on_already_formatted"
            );
            return;
        }

        // This is the canonical gofmt output for a minimal main package.
        const FORMATTED: &str =
            "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n";
        let src = make_src("main.go", Language::Go, FORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();
        assert!(
            matches!(result, FormatOutput::Unchanged),
            "gofmt must return Unchanged for already-formatted source"
        );
    }

    // ---------------------------------------------------------------------------
    // Default-on resolution policy (ADR 0014 amendment)
    // ---------------------------------------------------------------------------

    /// Canonical toolchains (rustfmt, gofmt) are wanted by default; opt-in tools
    /// (zig fmt) are not. The empty/default config drives this.
    #[test]
    fn default_policy_canonical_on_option_off() {
        assert!(
            NativeToolEngine::for_language(Language::Rust).is_enabled(&default_cfg()),
            "rustfmt must be default-on (canonical toolchain)"
        );
        assert!(
            NativeToolEngine::for_language(Language::Go).is_enabled(&default_cfg()),
            "gofmt must be default-on (canonical toolchain)"
        );
        assert!(
            !NativeToolEngine::for_language(Language::Zig).is_enabled(&default_cfg()),
            "zig fmt must stay opt-in (off by default)"
        );
    }

    /// An explicit `enabled = false` overrides the default-on policy for the
    /// canonical tools, and `enabled = true` opts a default-off tool in.
    #[test]
    fn explicit_config_overrides_default_policy() {
        assert!(
            !NativeToolEngine::for_language(Language::Rust).is_enabled(&disabled_cfg()),
            "explicit enabled=false must force rustfmt off"
        );
        assert!(
            !NativeToolEngine::for_language(Language::Go).is_enabled(&disabled_cfg()),
            "explicit enabled=false must force gofmt off"
        );
        assert!(
            NativeToolEngine::for_language(Language::Zig).is_enabled(&enabled_cfg()),
            "explicit enabled=true must opt zig fmt in"
        );
    }

    /// The info-notice trigger is a pure predicate: notify iff the tool was
    /// wanted but is absent from PATH. This is the rustfmt/gofmt-absent path.
    #[test]
    fn fallback_notice_fires_only_when_wanted_and_absent() {
        // wanted + absent → notify (route to tree-sitter + info path)
        assert!(should_notify_fallback(true, false));
        // wanted + present → native tool runs, no fallback
        assert!(!should_notify_fallback(true, true));
        // not wanted (explicitly disabled) → silent tier-2, the user's choice
        assert!(!should_notify_fallback(false, false));
        assert!(!should_notify_fallback(false, true));
    }

    /// Default config (no explicit `enabled`) must route to exactly the right
    /// formatter for Rust depending only on rustfmt's presence — never error.
    ///
    /// - rustfmt present → native-tool output (differs from tier-2 on this input).
    /// - rustfmt absent  → byte-identical to the tree-sitter generic tier, and
    ///   the fallback predicate is satisfied (info path).
    ///
    /// Does not require rustfmt to be installed: both branches are asserted.
    #[test]
    fn default_rust_routes_by_rustfmt_presence() {
        const UNFORMATTED: &str = "fn main(){let x=1+2;println!(\"{x}\");}\n";
        let engine = NativeToolEngine::for_language(Language::Rust);
        let src = make_src("main.rs", Language::Rust, UNFORMATTED);

        let result = engine.format(&src, &default_cfg()).unwrap();
        let out = match result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => UNFORMATTED.to_string(),
        };

        let tier2 = match TreeSitterEngine.format(&src, &default_cfg()).unwrap() {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => UNFORMATTED.to_string(),
        };

        if engine.probed_version().is_some() {
            // rustfmt present + default-on → native tool wins over tier-2.
            assert!(
                out.contains("fn main() {"),
                "rustfmt should expand the signature; got: {out:?}"
            );
            assert!(
                !should_notify_fallback(engine.is_enabled(&default_cfg()), true),
                "no fallback notice when rustfmt is present"
            );
        } else {
            // rustfmt absent → generic tier, byte-identical, info path armed.
            assert_eq!(
                out, tier2,
                "absent rustfmt must fall back to byte-identical tree-sitter output"
            );
            assert!(
                should_notify_fallback(engine.is_enabled(&default_cfg()), false),
                "absent default-on rustfmt must arm the tier-2 fallback notice"
            );
        }
    }

    /// Explicitly disabling a canonical tool always routes to the generic tier,
    /// byte-identical to a direct `TreeSitterEngine` call — independent of
    /// whether rustfmt is installed (simulates the absent case via config).
    #[test]
    fn disabled_rust_delegates_to_tier2() {
        const SRC: &str = "fn main(){let x=1+2;println!(\"{x}\");}\n";
        let engine = NativeToolEngine::for_language(Language::Rust);
        let src = make_src("main.rs", Language::Rust, SRC);

        let native_out = match engine.format(&src, &disabled_cfg()).unwrap() {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => SRC.to_string(),
        };
        let tier2_out = match TreeSitterEngine.format(&src, &disabled_cfg()).unwrap() {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => SRC.to_string(),
        };

        assert_eq!(
            native_out, tier2_out,
            "explicitly disabled rustfmt must produce byte-identical tree-sitter output"
        );
    }
}
