//! PHP format pass via [`mago_formatter::Formatter`].
//!
//! Config mapping:
//! - `cfg.globals.line_length` → `print_width` (default 120)
//! - `cfg.indent_width` → `tab_width` (default 4 for PHP)
//!
//! Returns [`FormatOutput::Unchanged`] on parse failure (the lint pass reports
//! the syntax error) and when the formatted output is identical to the input.

use std::borrow::Cow;

use mago_formatter::Formatter;
use mago_formatter::settings::FormatSettings;
use mago_php_version::PHPVersion;

use crate::config::EngineConfig;
use crate::engine::{FormatOutput, SourceFile};

/// Target PHP version for formatting rules.
const PHP_VERSION: PHPVersion = PHPVersion::PHP84;

/// Format a single PHP source file.
///
/// Creates a per-call arena and `Formatter`, avoiding any stored state so this
/// function is safe to call from multiple rayon threads concurrently.
pub(super) fn format_php(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let settings = FormatSettings {
        print_width: cfg.globals.line_length,
        tab_width: cfg.indent_width,
        ..FormatSettings::default()
    };

    // LocalArena is Send but !Sync; it is scoped to this call so the engine
    // struct itself remains Send + Sync.
    let arena = mago_allocator::LocalArena::new();
    let formatter = Formatter::new(&arena, PHP_VERSION, settings);

    // format_code creates an ephemeral File internally and returns the
    // formatted bytes borrowed from the arena.  On parse failure it returns
    // Err(ParseError) — we return Unchanged so the lint pass can surface the
    // error as a diagnostic.
    let name: Cow<'static, [u8]> = Cow::Borrowed(b"input.php");
    let code: Cow<'static, [u8]> = Cow::Owned(src.content.as_bytes().to_vec());

    let formatted_bytes = match formatter.format_code(name, code) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(FormatOutput::Unchanged),
    };

    // Copy the bytes out of the arena before it drops.
    let formatted = match std::str::from_utf8(formatted_bytes) {
        Ok(s) => s.to_owned(),
        // mago always produces valid UTF-8; fall back to Unchanged on the
        // (unreachable) off-chance that it doesn't.
        Err(_) => return Ok(FormatOutput::Unchanged),
    };

    if formatted == src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}
