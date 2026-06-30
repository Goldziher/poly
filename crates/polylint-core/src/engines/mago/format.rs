//! PHP format pass via [`mago_formatter::Formatter`].
//!
//! ## Settings layering
//!
//! | Priority | Source |
//! |----------|--------|
//! | 3 (highest) | user `[fmt.php.mago]` table keys |
//! | 2 | polylint opinionated defaults (`line_length` → `print_width`, `indent_width` → `tab_width`) |
//! | 1 (lowest) | mago's own `FormatSettings::default()` |
//!
//! The user can override any key that [`mago_formatter::settings::FormatSettings`]
//! exposes (kebab-case).  Unknown keys are silently ignored.
//!
//! Returns [`FormatOutput::Unchanged`] on parse failure (the lint pass reports
//! the syntax error) and when the formatted output is identical to the input.

use std::borrow::Cow;

use mago_formatter::Formatter;
use mago_formatter::settings::{FormatSettings, RawFormatSettings};
use mago_php_version::PHPVersion;

use crate::config::EngineConfig;
use crate::engine::{FormatOutput, SourceFile};

/// Polylint-default PHP version for formatting rules.
const PHP_VERSION: PHPVersion = PHPVersion::PHP84;

/// Format a single PHP source file.
///
/// Creates a per-call arena and `Formatter`, avoiding any stored state so this
/// function is safe to call from multiple rayon threads concurrently.
pub(super) fn format_php(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    // Honor the configured target so the formatter does not emit syntax (e.g.
    // PHP 8.4 `new` without parens) the user's runtime cannot parse.
    let php_version = super::rules::parse_php_version(cfg)?.unwrap_or(PHP_VERSION);
    let settings = build_format_settings(cfg);

    let arena = mago_allocator::LocalArena::new();
    let formatter = Formatter::new(&arena, php_version, settings);

    let name: Cow<'static, [u8]> = Cow::Borrowed(b"input.php");
    let code: Cow<'static, [u8]> = Cow::Owned(src.content.as_bytes().to_vec());

    let formatted_bytes = match formatter.format_code(name, code) {
        Ok(bytes) => bytes,
        // Parse failure: return Unchanged; the lint pass surfaces the error.
        Err(_) => return Ok(FormatOutput::Unchanged),
    };

    let formatted = match std::str::from_utf8(formatted_bytes) {
        Ok(s) => s.to_owned(),
        // mago always produces valid UTF-8; unreachable in practice.
        Err(_) => return Ok(FormatOutput::Unchanged),
    };

    if formatted == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}

// ── Settings construction ─────────────────────────────────────────────────────

/// Build [`FormatSettings`] by layering user options over polylint opinionated
/// defaults over mago's own defaults.
///
/// 1. Start with `FormatSettings::default()` (mago's defaults).
/// 2. Apply poly's `line_length` and `indent_width` as the opinionated base.
/// 3. Deserialise `cfg.options` into [`RawFormatSettings`] (all-`Option`).
///    Only fields the user explicitly set are `Some`; the rest are `None`.
/// 4. `raw.merge_with(base)` — user's `Some` fields override the base; `None`
///    fields keep the base value.
fn build_format_settings(cfg: &EngineConfig) -> FormatSettings {
    // Step 2: apply polylint opinionated defaults over mago's defaults.
    let base = FormatSettings {
        print_width: cfg.globals.line_length,
        tab_width: cfg.indent_width,
        ..FormatSettings::default()
    };

    if cfg.options.is_empty() {
        return base;
    }

    // Step 3: deserialise the user's options table into RawFormatSettings.
    // Unknown keys are silently ignored (RawFormatSettings has no
    // `deny_unknown_fields`), so keys meant for [lint.php.mago] won't cause
    // errors here.
    let raw: RawFormatSettings = toml::Value::Table(cfg.options.clone())
        .try_into()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "[fmt.php.mago] options could not be parsed; using defaults");
            RawFormatSettings::default()
        });

    // Step 4: merge — user's Some fields win; None keeps the base value.
    raw.merge_with(base)
}
