//! Native toolchain backends: thin wrappers around first-party / canonical
//! formatter and linter CLIs.
//!
//! ## Supported tools
//!
//! | Language | Tool        | Kind   | Default-on |
//! |----------|-------------|--------|------------|
//! | Go       | `gofmt`     | format | yes        |
//! | Rust     | `rustfmt`   | format | yes        |
//! | Zig      | `zig fmt`   | format | no         |
//! | Shell    | `shfmt`     | format | no         |
//! | Shell    | `shellcheck`| lint   | no         |
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
//! `shfmt` and `shellcheck` are **opt-in, off by default** because they are
//! third-party tools (not part of a canonical language toolchain). Enable them
//! via `poly.toml`:
//!
//! ```toml
//! [fmt.shell.shfmt]
//! enabled = true
//!
//! [lint.shell.shellcheck]
//! enabled = true
//! ```
//!
//! ## Registry slots
//!
//! Each `NativeToolEngine` instance occupies the registry slot that
//! `TreeSitterEngine` would otherwise hold for its language. For Shell, two
//! entries are registered: one for `shfmt` (format) and one for `shellcheck`
//! (lint). Internal delegation to `TreeSitterEngine` ensures exactly one
//! formatter and one linter always runs per file, regardless of tool
//! presence.
//!
//! ## Subprocess I/O safety
//!
//! A dedicated OS thread writes stdin while the main (rayon) worker thread
//! collects stdout via `wait_with_output`. This prevents the pipe-buffer
//! deadlock that can occur for source files larger than the OS pipe buffer
//! (~64 KB on Linux) when a formatter buffers all input before writing output.

use std::sync::{Once, OnceLock};

use tracing::info;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::engines::treesitter::TreeSitterEngine;
use crate::language::Language;

use self::format::format_via_tool;
use self::lint::lint_via_shellcheck;
use self::probe::probe_tool;
use self::spec::ToolSpec;
use self::spec::{
    GOFMT_KEY, GOFMT_NOTICE, GOFMT_PROBE, GOFMT_SPEC, RUSTFMT_KEY, RUSTFMT_NOTICE, RUSTFMT_PROBE, RUSTFMT_SPEC,
    SHELLCHECK_KEY, SHELLCHECK_PROBE, SHELLCHECK_SPEC, SHFMT_KEY, SHFMT_NOTICE, SHFMT_PROBE, SHFMT_SPEC, ZIGFMT_KEY,
    ZIGFMT_NOTICE, ZIGFMT_PROBE, ZIGFMT_SPEC,
};

mod edition;
mod format;
mod lint;
mod probe;
mod spec;

// ---------------------------------------------------------------------------
// Per-language static slices for Engine::languages
// ---------------------------------------------------------------------------

static GO_LANGUAGES: &[Language] = &[Language::Go];
static RUST_LANGUAGES: &[Language] = &[Language::Rust];
static ZIG_LANGUAGES: &[Language] = &[Language::Zig];
static SHELL_LANGUAGES: &[Language] = &[Language::Shell];

// ---------------------------------------------------------------------------
// NativeRole: which tool + capability this engine instance represents
// ---------------------------------------------------------------------------

/// Which native tool and role this `NativeToolEngine` instance plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeRole {
    /// `gofmt` for Go (format + lint-via-TS).
    GoFmt,
    /// `rustfmt` for Rust (format + lint-via-TS).
    Rustfmt,
    /// `zig fmt` for Zig (format + lint-via-TS).
    ZigFmt,
    /// `shfmt` for Shell (format only; lint is the Shellcheck entry).
    Shfmt,
    /// `shellcheck` for Shell (lint only; format is the Shfmt entry).
    Shellcheck,
}

// ---------------------------------------------------------------------------
// NativeToolEngine
// ---------------------------------------------------------------------------

/// Tier-3 opt-in native tool backend. One instance per tool per language;
/// see the module docs for the enabled/disabled/absent semantics.
pub struct NativeToolEngine {
    role: NativeRole,
}

impl NativeToolEngine {
    /// Construct the canonical format engine for Go, Rust, or Zig.
    ///
    /// # Panics
    ///
    /// Panics if `language` is not `Go`, `Rust`, or `Zig`. Use
    /// [`NativeToolEngine::shell_format`] / [`NativeToolEngine::shell_lint`]
    /// for `Language::Shell`.
    pub fn for_language(language: Language) -> Self {
        let role = match language {
            Language::Go => NativeRole::GoFmt,
            Language::Rust => NativeRole::Rustfmt,
            Language::Zig => NativeRole::ZigFmt,
            other => {
                panic!("NativeToolEngine::for_language only supports Go, Rust, Zig; got {other:?}")
            }
        };
        NativeToolEngine { role }
    }

    /// Construct the shfmt format engine for Shell.
    pub fn shell_format() -> Self {
        NativeToolEngine {
            role: NativeRole::Shfmt,
        }
    }

    /// Construct the shellcheck lint engine for Shell.
    pub fn shell_lint() -> Self {
        NativeToolEngine {
            role: NativeRole::Shellcheck,
        }
    }

    fn spec(&self) -> &'static ToolSpec {
        match self.role {
            NativeRole::GoFmt => &GOFMT_SPEC,
            NativeRole::Rustfmt => &RUSTFMT_SPEC,
            NativeRole::ZigFmt => &ZIGFMT_SPEC,
            NativeRole::Shfmt => &SHFMT_SPEC,
            NativeRole::Shellcheck => &SHELLCHECK_SPEC,
        }
    }

    fn probe_lock(&self) -> &'static OnceLock<Option<String>> {
        match self.role {
            NativeRole::GoFmt => &GOFMT_PROBE,
            NativeRole::Rustfmt => &RUSTFMT_PROBE,
            NativeRole::ZigFmt => &ZIGFMT_PROBE,
            NativeRole::Shfmt => &SHFMT_PROBE,
            NativeRole::Shellcheck => &SHELLCHECK_PROBE,
        }
    }

    fn key_lock(&self) -> &'static OnceLock<String> {
        match self.role {
            NativeRole::GoFmt => &GOFMT_KEY,
            NativeRole::Rustfmt => &RUSTFMT_KEY,
            NativeRole::ZigFmt => &ZIGFMT_KEY,
            NativeRole::Shfmt => &SHFMT_KEY,
            NativeRole::Shellcheck => &SHELLCHECK_KEY,
        }
    }

    /// Returns the tier-2 fallback notice guard for this role, or `None` when
    /// no notice applies (lint-only tools such as shellcheck do not emit a
    /// fallback notice when absent).
    fn notice_lock(&self) -> Option<&'static Once> {
        match self.role {
            NativeRole::GoFmt => Some(&GOFMT_NOTICE),
            NativeRole::Rustfmt => Some(&RUSTFMT_NOTICE),
            NativeRole::ZigFmt => Some(&ZIGFMT_NOTICE),
            NativeRole::Shfmt => Some(&SHFMT_NOTICE),
            // shellcheck absent → TS lint still runs; no fallback notice needed.
            NativeRole::Shellcheck => None,
        }
    }

    /// Whether the native tool is *wanted* for this run: the explicit
    /// `enabled = …` from user config if present, otherwise the tool's
    /// `default_on` policy.
    fn is_enabled(&self, cfg: &EngineConfig) -> bool {
        cfg.options
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(self.spec().default_on)
    }

    /// Emit the tier-2 fallback notice at most once per language per run.
    ///
    /// Only fires when the tool was *wanted* (enabled / default-on) but is
    /// absent from `PATH`. An explicit `enabled = false` is the user's choice
    /// and stays silent; presence of the tool means no fallback happens.
    fn notify_tier2_fallback(&self, cfg: &EngineConfig) {
        if should_notify_fallback(self.is_enabled(cfg), self.probed_version().is_some())
            && let Some(notice) = self.notice_lock()
        {
            let spec = self.spec();
            notice.call_once(|| {
                info!(
                    language = self.languages()[0].id(),
                    tool = spec.probe_binary(),
                    "{} not found on PATH; formatting via the generic tree-sitter tier (lower fidelity)",
                    spec.probe_binary()
                );
            });
        }
    }

    /// Returns the probed version string, or `None` when the tool is absent.
    ///
    /// Memoised in a static `OnceLock`; subsequent calls within the same
    /// process are free.
    fn probed_version(&self) -> Option<&'static str> {
        self.probe_lock().get_or_init(|| probe_tool(self.spec())).as_deref()
    }

    /// Whether the underlying native tool is installed on this host.
    pub fn is_available(&self) -> bool {
        self.probed_version().is_some()
    }
}

// ---------------------------------------------------------------------------
// Engine impl
// ---------------------------------------------------------------------------

impl Engine for NativeToolEngine {
    fn name(&self) -> &'static str {
        self.spec().engine_name
    }

    fn languages(&self) -> &'static [Language] {
        match self.role {
            NativeRole::GoFmt => GO_LANGUAGES,
            NativeRole::Rustfmt => RUST_LANGUAGES,
            NativeRole::ZigFmt => ZIG_LANGUAGES,
            NativeRole::Shfmt | NativeRole::Shellcheck => SHELL_LANGUAGES,
        }
    }

    /// Capability declaration:
    ///
    /// - Go/Rust/Zig format engines: both `lint` (delegated to TS) and
    ///   `format` (native tool or TS fallback).
    /// - Shell shfmt: `format` only (lint is the separate shellcheck entry).
    /// - Shell shellcheck: `lint` only (format is the separate shfmt entry).
    ///
    /// `format` is always `true` for Go/Rust/Zig because each holds the sole
    /// registry slot for its language; if `format` were `false` when disabled,
    /// the language would be left entirely unformatted (the TS engine is not
    /// separately registered for those languages). For Shell, two separate
    /// engines are registered so each declares only what it actually does.
    fn capabilities(&self) -> Capabilities {
        match self.role {
            NativeRole::GoFmt | NativeRole::Rustfmt | NativeRole::ZigFmt => Capabilities {
                lint: true,
                format: true,
                fix: false,
            },
            NativeRole::Shfmt => Capabilities {
                lint: false,
                format: true,
                fix: false,
            },
            NativeRole::Shellcheck => Capabilities {
                lint: true,
                format: false,
                fix: false,
            },
        }
    }

    /// Cache-key version string. Folds in BOTH the native tool version (or an
    /// `absent` sentinel) AND the tree-sitter engine version, because every
    /// disabled/absent path delegates to tier-2 — so a tier-2 upgrade must
    /// invalidate cached native-tool results.
    fn version(&self) -> &str {
        self.key_lock().get_or_init(|| {
            let ts = TreeSitterEngine.version();
            // Edition-aware tools (rustfmt) now pass `--edition`, which changes
            // their output relative to the prior edition-2015 default; mark the
            // key so previously cached results are invalidated.
            let edition_marker = if self.spec().edition_flag {
                " | edition-aware"
            } else {
                ""
            };
            // rustfmt now honours a project-level rustfmt.toml via --config-path
            // and otherwise defers to rustfmt's own defaults (no forced
            // max_width). Mark the key to invalidate caches built under the old
            // always-inject-max_width=120 behaviour.
            let config_path_marker = if self.spec().rustfmt_config_flag {
                " | rustfmt-defaults"
            } else {
                ""
            };
            match self.probed_version() {
                Some(tool) => format!("{tool} | ts:{ts}{edition_marker}{config_path_marker}"),
                None => format!("native-tool:absent | ts:{ts}{edition_marker}{config_path_marker}"),
            }
        })
    }

    /// Lint dispatch:
    ///
    /// - Go/Rust/Zig: always delegate to [`TreeSitterEngine`] (trailing
    ///   whitespace, etc.). These tools are format-only.
    /// - Shell shfmt: no-op (lint capability is `false`; this method is not
    ///   called by the runner, but returns empty for safety).
    /// - Shell shellcheck: run [`TreeSitterEngine`] lint first (always), then
    ///   append shellcheck diagnostics when the tool is enabled and present.
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match self.role {
            NativeRole::GoFmt | NativeRole::Rustfmt | NativeRole::ZigFmt => TreeSitterEngine.lint(src, cfg),
            NativeRole::Shfmt => Ok(Vec::new()),
            NativeRole::Shellcheck => {
                let mut diags = TreeSitterEngine.lint(src, cfg)?;
                if self.is_enabled(cfg) && self.probed_version().is_some() {
                    let sc_diags = lint_via_shellcheck(self.spec(), src)?;
                    diags.extend(sc_diags);
                }
                Ok(diags)
            }
        }
    }

    /// Format dispatch:
    ///
    /// - Go/Rust/Zig/Shfmt: native tool when enabled+present, else delegate
    ///   to [`TreeSitterEngine`] (tier-2 fallback).
    /// - Shell shellcheck: no-op (format capability is `false`).
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        match self.role {
            NativeRole::GoFmt | NativeRole::Rustfmt | NativeRole::ZigFmt | NativeRole::Shfmt => {
                if !self.is_enabled(cfg) || self.probed_version().is_none() {
                    self.notify_tier2_fallback(cfg);
                    return TreeSitterEngine.format(src, cfg);
                }
                format_via_tool(self.spec(), src, cfg.indent_width)
            }
            NativeRole::Shellcheck => Ok(FormatOutput::Unchanged),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helper (also tested directly)
// ---------------------------------------------------------------------------

/// Decide whether the tier-2 fallback info notice should fire: the tool was
/// wanted (`enabled` / default-on) but is not present on `PATH`. Pure so it
/// can be unit-tested without a real toolchain.
fn should_notify_fallback(wanted: bool, present: bool) -> bool {
    wanted && !present
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

    fn bool_cfg(enabled: bool) -> EngineConfig {
        let mut options = toml::Table::new();
        options.insert("enabled".to_string(), toml::Value::Boolean(enabled));
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options,
        }
    }

    fn disabled_cfg() -> EngineConfig {
        bool_cfg(false)
    }

    fn enabled_cfg() -> EngineConfig {
        bool_cfg(true)
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
    fn engine_metadata_shell_shfmt() {
        let engine = NativeToolEngine::shell_format();
        assert_eq!(engine.name(), "shfmt");
        assert_eq!(engine.languages(), &[Language::Shell]);
        assert!(engine.capabilities().format);
        assert!(!engine.capabilities().lint, "shfmt is format-only");
        assert!(!engine.capabilities().fix);
    }

    #[test]
    fn engine_metadata_shell_shellcheck() {
        let engine = NativeToolEngine::shell_lint();
        assert_eq!(engine.name(), "shellcheck");
        assert_eq!(engine.languages(), &[Language::Shell]);
        assert!(engine.capabilities().lint);
        assert!(!engine.capabilities().format, "shellcheck is lint-only");
        assert!(!engine.capabilities().fix);
    }

    // ---------------------------------------------------------------------------
    // Default-on / default-off policy
    // ---------------------------------------------------------------------------

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
            "zig fmt must stay opt-in"
        );
        assert!(
            !NativeToolEngine::shell_format().is_enabled(&default_cfg()),
            "shfmt must be opt-in (third-party tool)"
        );
        assert!(
            !NativeToolEngine::shell_lint().is_enabled(&default_cfg()),
            "shellcheck must be opt-in"
        );
    }

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
        assert!(
            NativeToolEngine::shell_format().is_enabled(&enabled_cfg()),
            "explicit enabled=true must opt shfmt in"
        );
        assert!(
            NativeToolEngine::shell_lint().is_enabled(&enabled_cfg()),
            "explicit enabled=true must opt shellcheck in"
        );
    }

    // ---------------------------------------------------------------------------
    // Fallback notice predicate
    // ---------------------------------------------------------------------------

    #[test]
    fn fallback_notice_fires_only_when_wanted_and_absent() {
        assert!(should_notify_fallback(true, false));
        assert!(!should_notify_fallback(true, true));
        assert!(!should_notify_fallback(false, false));
        assert!(!should_notify_fallback(false, true));
    }

    // ---------------------------------------------------------------------------
    // Lint delegation (Go + Shell)
    // ---------------------------------------------------------------------------

    #[test]
    fn lint_clean_go_produces_no_diags() {
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
        let engine = NativeToolEngine::for_language(Language::Go);
        let src = make_src("main.go", Language::Go, "package main   \nfunc main() {}\n");
        let diags = engine.lint(&src, &disabled_cfg()).unwrap();
        assert!(!diags.is_empty(), "trailing whitespace in Go source must be flagged");
        assert_eq!(diags[0].code.as_deref(), Some("trailing-whitespace"));
    }

    #[test]
    fn shellcheck_lint_disabled_uses_treesitter_only() {
        // When shellcheck is disabled, the engine must still return TS lint
        // results (e.g. trailing whitespace), NOT skip lint entirely.
        let engine = NativeToolEngine::shell_lint();
        let src = make_src("script.sh", Language::Shell, "#!/bin/bash\necho hello   \n");
        let diags = engine.lint(&src, &disabled_cfg()).unwrap();
        // TS flags trailing whitespace regardless of shellcheck state.
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("trailing-whitespace")),
            "disabled shellcheck must still surface TS trailing-whitespace diagnostic"
        );
        // No shellcheck diagnostics (SC*) when disabled.
        assert!(
            diags.iter().all(|d| !d.code.as_deref().unwrap_or("").starts_with("SC")),
            "no SC diagnostics expected when shellcheck is disabled"
        );
    }

    // ---------------------------------------------------------------------------
    // Disabled → tier-2 fallback (format)
    // ---------------------------------------------------------------------------

    #[test]
    fn disabled_go_delegates_to_tier2() {
        const SRC: &str = "package main\nimport \"fmt\"\nfunc main() {\nfmt.Println(\"hi\")\n}\n";
        let engine = NativeToolEngine::for_language(Language::Go);
        let src = make_src("main.go", Language::Go, SRC);

        let native_result = engine.format(&src, &disabled_cfg()).unwrap();
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

        assert_eq!(native_out, tier2_out);
    }

    #[test]
    fn disabled_shfmt_delegates_to_tier2() {
        const SRC: &str = "#!/bin/bash\nif [ \"$1\" = \"a\" ]; then\necho hello\nfi\n";
        let engine = NativeToolEngine::shell_format();
        let src = make_src("script.sh", Language::Shell, SRC);

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
            "disabled shfmt must produce byte-identical output to TreeSitterEngine"
        );
    }

    // ---------------------------------------------------------------------------
    // Default-on routing (Rust: depends on rustfmt presence)
    // ---------------------------------------------------------------------------

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
            assert!(
                out.contains("fn main() {"),
                "rustfmt should expand the signature; got: {out:?}"
            );
            assert!(
                !should_notify_fallback(engine.is_enabled(&default_cfg()), true),
                "no fallback notice when rustfmt is present"
            );
        } else {
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

    // ---------------------------------------------------------------------------
    // Native tool present → correct output (probe-gated)
    // ---------------------------------------------------------------------------

    #[test]
    fn go_native_formats_unformatted_source() {
        let engine = NativeToolEngine::for_language(Language::Go);
        if engine.probed_version().is_none() {
            eprintln!("gofmt not found on PATH — skipping go_native_formats_unformatted_source");
            return;
        }

        const UNFORMATTED: &str = "package main\nimport \"fmt\"\nfunc main() {\nfmt.Println(\"hello\")\n}\n";
        let src = make_src("main.go", Language::Go, UNFORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();

        let formatted = match result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => panic!("expected gofmt to reformat the unformatted source"),
        };

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

    #[test]
    fn rust_native_formats_unformatted_source() {
        let engine = NativeToolEngine::for_language(Language::Rust);
        if engine.probed_version().is_none() {
            eprintln!("rustfmt not found on PATH — skipping rust_native_formats_unformatted_source");
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

    #[test]
    fn zig_native_formats_unformatted_source() {
        let engine = NativeToolEngine::for_language(Language::Zig);
        if engine.probed_version().is_none() {
            eprintln!("zig not found on PATH — skipping zig_native_formats_unformatted_source");
            return;
        }

        const UNFORMATTED: &str = "const std = @import(\"std\");\npub fn main() void {\n_ = std;\n}\n";
        let src = make_src("main.zig", Language::Zig, UNFORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();

        let formatted = match result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => UNFORMATTED.to_string(),
        };

        insta::assert_snapshot!("zig_native_known_unformatted", formatted);
    }

    /// Known-unformatted shell: shfmt should add consistent indentation.
    /// Skipped when `shfmt` is not on PATH.
    #[test]
    fn shfmt_formats_unformatted_shell() {
        let engine = NativeToolEngine::shell_format();
        if engine.probed_version().is_none() {
            eprintln!("shfmt not found on PATH — skipping shfmt_formats_unformatted_shell");
            return;
        }

        // Unformatted: body of `if` is not indented.
        const UNFORMATTED: &str = "#!/bin/bash\nif [ \"$1\" = \"hello\" ]; then\necho \"world\"\nfi\n";
        let src = make_src("script.sh", Language::Shell, UNFORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();

        let formatted = match result {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => {
                panic!("expected shfmt to reformat the unformatted source")
            }
        };

        // shfmt with -i 4 should indent the body with 4 spaces.
        assert!(
            formatted.contains("    echo"),
            "shfmt output should use 4-space indentation; got:\n{formatted}"
        );

        insta::assert_snapshot!("shell_shfmt_known_unformatted", formatted);
    }

    /// shellcheck on a known-bad script produces SC-coded diagnostics.
    /// Skipped when `shellcheck` is not on PATH.
    #[test]
    fn shellcheck_lint_produces_sc_diagnostics() {
        let engine = NativeToolEngine::shell_lint();
        if engine.probed_version().is_none() {
            eprintln!("shellcheck not found on PATH — skipping shellcheck_lint_produces_sc_diagnostics");
            return;
        }

        // SC2086: unquoted variable — always flagged by shellcheck.
        const BAD: &str = "#!/bin/bash\nx=\"hello world\"\necho $x\n";
        let src = make_src("bad.sh", Language::Shell, BAD);
        let diags = engine.lint(&src, &enabled_cfg()).unwrap();

        let sc_diags: Vec<_> = diags
            .iter()
            .filter(|d| d.code.as_deref().unwrap_or("").starts_with("SC"))
            .collect();
        assert!(
            !sc_diags.is_empty(),
            "shellcheck should flag SC2086 (unquoted variable) in the known-bad script"
        );
        assert!(
            sc_diags.iter().any(|d| d.code.as_deref() == Some("SC2086")),
            "expected SC2086 in diagnostics; got: {sc_diags:?}"
        );
    }

    // ---------------------------------------------------------------------------
    // Idempotency
    // ---------------------------------------------------------------------------

    #[test]
    fn go_native_unchanged_on_already_formatted() {
        let engine = NativeToolEngine::for_language(Language::Go);
        if engine.probed_version().is_none() {
            eprintln!("gofmt not found on PATH — skipping go_native_unchanged_on_already_formatted");
            return;
        }

        const FORMATTED: &str = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n";
        let src = make_src("main.go", Language::Go, FORMATTED);
        let result = engine.format(&src, &enabled_cfg()).unwrap();
        assert!(
            matches!(result, FormatOutput::Unchanged),
            "gofmt must return Unchanged for already-formatted source"
        );
    }
}
