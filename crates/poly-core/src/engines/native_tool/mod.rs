//! Native toolchain backends: thin wrappers around first-party / canonical
//! formatter and linter CLIs.
//!
//! ## Supported tools
//!
//! | Language | Tool                  | Kind   | Default-on |
//! |----------|-----------------------|--------|------------|
//! | Go       | `gofmt`               | format | yes        |
//! | Rust     | `rustfmt`             | format | yes        |
//! | Zig      | `zig fmt`             | format | no         |
//! | Shell    | `shfmt`               | format | no         |
//! | Shell    | `shellcheck`          | lint   | no         |
//! | Java     | `google-java-format`  | format | no         |
//! | Kotlin   | `ktfmt`               | format | no         |
//! | R        | `Rscript` (styler)    | format | no         |
//! | Swift    | `swift-format`        | format | no         |
//! | Dart     | `dart format`         | format | no         |
//! | Gleam    | `gleam format`        | format | no         |
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
//! (lint). When a format tool is absent, formatting delegates to
//! `TreeSitterEngine` (the tier-2 fallback) so exactly one formatter always
//! runs per file. These wrappers carry no lint rules of their own — only
//! `shellcheck` produces lint diagnostics; the format-only roles declare
//! `lint: false`.
//!
//! ## Subprocess I/O safety
//!
//! A dedicated OS thread writes stdin while the main (rayon) worker thread
//! collects stdout via `wait_with_output`. This prevents the pipe-buffer
//! deadlock that can occur for source files larger than the OS pipe buffer
//! (~64 KB on Linux) when a formatter buffers all input before writing output.

use tracing::info;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::engines::treesitter::TreeSitterEngine;
use crate::language::Language;

use self::format::format_via_tool;
use self::lint::lint_via_shellcheck;
use self::probe::probe_tool;
use self::spec::NativeRole;

mod edition;
mod format;
mod lint;
mod probe;
mod spec;
#[cfg(test)]
mod tests;

static GO_LANGUAGES: &[Language] = &[Language::Go];
static RUST_LANGUAGES: &[Language] = &[Language::Rust];
static ZIG_LANGUAGES: &[Language] = &[Language::Zig];
static SHELL_LANGUAGES: &[Language] = &[Language::Shell];
static JAVA_LANGUAGES: &[Language] = &[Language::Java];
static KOTLIN_LANGUAGES: &[Language] = &[Language::Kotlin];
static R_LANGUAGES: &[Language] = &[Language::R];
static SWIFT_LANGUAGES: &[Language] = &[Language::Swift];
static DART_LANGUAGES: &[Language] = &[Language::Dart];
static GLEAM_LANGUAGES: &[Language] = &[Language::Gleam];

/// Tier-3 opt-in native tool backend. One instance per tool per language;
/// see the module docs for the enabled/disabled/absent semantics.
pub struct NativeToolEngine {
    role: NativeRole,
}

impl NativeToolEngine {
    /// Construct the format engine for the given language.
    ///
    /// Supported: Go, Rust, Zig, Java, Kotlin, R, Swift, Dart, Gleam.
    ///
    /// # Panics
    ///
    /// Panics if `language` is not one of the supported languages above. Use
    /// [`NativeToolEngine::shell_format`] / [`NativeToolEngine::shell_lint`]
    /// for `Language::Shell`.
    pub fn for_language(language: Language) -> Self {
        let role = match language {
            Language::Go => NativeRole::GoFmt,
            Language::Rust => NativeRole::Rustfmt,
            Language::Zig => NativeRole::ZigFmt,
            Language::Java => NativeRole::JavaFmt,
            Language::Kotlin => NativeRole::KtFmt,
            Language::R => NativeRole::RStyler,
            Language::Swift => NativeRole::SwiftFmt,
            Language::Dart => NativeRole::DartFmt,
            Language::Gleam => NativeRole::GleamFmt,
            other => {
                panic!(
                    "NativeToolEngine::for_language does not support {other:?}; \
                     supported: Go, Rust, Zig, Java, Kotlin, R, Swift, Dart, Gleam"
                )
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

    /// Whether the native tool is *wanted* for this run: the explicit
    /// `enabled = …` from user config if present, otherwise the tool's
    /// `default_on` policy.
    fn is_enabled(&self, cfg: &EngineConfig) -> bool {
        cfg.options
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(self.role.spec().default_on)
    }

    /// Emit the tier-2 fallback notice at most once per language per run.
    ///
    /// Only fires when the tool was *wanted* (enabled / default-on) but is
    /// absent from `PATH`. An explicit `enabled = false` is the user's choice
    /// and stays silent; presence of the tool means no fallback happens.
    fn notify_tier2_fallback(&self, cfg: &EngineConfig) {
        if should_notify_fallback(self.is_enabled(cfg), self.probed_version().is_some())
            && let Some(notice) = self.role.notice_lock()
        {
            let spec = self.role.spec();
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
        self.role
            .probe_lock()
            .get_or_init(|| probe_tool(self.role.spec()))
            .as_deref()
    }

    /// Whether the underlying native tool is installed on this host.
    pub fn is_available(&self) -> bool {
        self.probed_version().is_some()
    }
}

impl Engine for NativeToolEngine {
    fn name(&self) -> &'static str {
        self.role.spec().engine_name
    }

    fn languages(&self) -> &'static [Language] {
        match self.role {
            NativeRole::GoFmt => GO_LANGUAGES,
            NativeRole::Rustfmt => RUST_LANGUAGES,
            NativeRole::ZigFmt => ZIG_LANGUAGES,
            NativeRole::Shfmt | NativeRole::Shellcheck => SHELL_LANGUAGES,
            NativeRole::JavaFmt => JAVA_LANGUAGES,
            NativeRole::KtFmt => KOTLIN_LANGUAGES,
            NativeRole::RStyler => R_LANGUAGES,
            NativeRole::SwiftFmt => SWIFT_LANGUAGES,
            NativeRole::DartFmt => DART_LANGUAGES,
            NativeRole::GleamFmt => GLEAM_LANGUAGES,
        }
    }

    /// Capability declaration:
    ///
    /// - Go/Rust/Zig/… format engines: `format` only. These wrap format-only
    ///   native tools and delegate formatting to the tree-sitter tier when the
    ///   tool is absent; they carry no lint rules (trailing-whitespace is a
    ///   `fmt` concern, applied by `format`, not surfaced under `lint`).
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
            NativeRole::GoFmt
            | NativeRole::Rustfmt
            | NativeRole::ZigFmt
            | NativeRole::JavaFmt
            | NativeRole::KtFmt
            | NativeRole::RStyler
            | NativeRole::SwiftFmt
            | NativeRole::DartFmt
            | NativeRole::GleamFmt
            | NativeRole::Shfmt => Capabilities {
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

    /// A native tool only lints its language when it is both switched on and
    /// installed. `shellcheck` is opt-in and its [`Engine::lint`] returns no
    /// diagnostics when either is false, so claiming coverage from the
    /// capability alone would report an unexamined shell script as linted —
    /// precisely the silent under-checking this hook exists to prevent.
    fn provides_language_lint(&self, _language: &Language, cfg: &EngineConfig) -> bool {
        self.capabilities().lint && self.is_enabled(cfg) && self.probed_version().is_some()
    }

    /// Cache-key version string. Folds in BOTH the native tool version (or an
    /// `absent` sentinel) AND the tree-sitter engine version, because every
    /// disabled/absent path delegates to tier-2 — so a tier-2 upgrade must
    /// invalidate cached native-tool results.
    fn version(&self) -> &str {
        self.role.key_lock().get_or_init(|| {
            let ts = TreeSitterEngine.version();
            let edition_marker = if self.role.spec().edition_flag {
                " | edition-aware"
            } else {
                ""
            };
            let config_path_marker = if self.role.spec().rustfmt_config_flag {
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
    /// - Every format-only role (Go/Rust/Zig/…/shfmt): no-op. These wrap
    ///   format-only tools and declare `lint: false`; the tree-sitter tier they
    ///   fall back to carries no lint rules either.
    /// - Shell shellcheck: shellcheck diagnostics when the tool is enabled and
    ///   present; otherwise empty.
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match self.role {
            NativeRole::Shellcheck => {
                let mut diags = Vec::new();
                if self.is_enabled(cfg) && self.probed_version().is_some() {
                    diags.extend(lint_via_shellcheck(self.role.spec(), src)?);
                }
                Ok(diags)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Format dispatch:
    ///
    /// - Go/Rust/Zig/Shfmt: native tool when enabled+present, else delegate
    ///   to [`TreeSitterEngine`] (tier-2 fallback).
    /// - Shell shellcheck: no-op (format capability is `false`).
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        match self.role {
            NativeRole::GoFmt
            | NativeRole::Rustfmt
            | NativeRole::ZigFmt
            | NativeRole::Shfmt
            | NativeRole::JavaFmt
            | NativeRole::KtFmt
            | NativeRole::RStyler
            | NativeRole::SwiftFmt
            | NativeRole::DartFmt
            | NativeRole::GleamFmt => {
                if !self.is_enabled(cfg) || self.probed_version().is_none() {
                    self.notify_tier2_fallback(cfg);
                    return TreeSitterEngine.format(src, cfg);
                }
                format_via_tool(self.role.spec(), src, cfg.indent_width)
            }
            NativeRole::Shellcheck => Ok(FormatOutput::Unchanged),
        }
    }
}

/// Decide whether the tier-2 fallback info notice should fire: the tool was
/// wanted (`enabled` / default-on) but is not present on `PATH`. Pure so it
/// can be unit-tested without a real toolchain.
fn should_notify_fallback(wanted: bool, present: bool) -> bool {
    wanted && !present
}
