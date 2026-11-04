//! The option keys the typos engine reads, which the host's config resolution merges.
//!
//! They live here rather than in the engine family so a core build, which serves typos from a
//! component, merges the same keys out of `poly.toml` without linking the engine.

/// The map-valued option keys `[lint.typos]` / `[lint.<lang>.typos]` accept.
///
/// One list, read twice: `Config::build_typos_options` merges exactly these keys
/// out of the user's tables (layering them over any native `_typos.toml`), and
/// `TyposEngine::option_keys` declares them as what the backend reads.
pub const MAP_OPTION_KEYS: &[&str] = &["extend_words", "extend_identifiers"];

/// The array-valued option keys, merged and declared like [`MAP_OPTION_KEYS`].
pub const ARRAY_OPTION_KEYS: &[&str] = &[
    "extend_exclude",
    "extend_ignore_words",
    "extend_ignore_re",
    "extend_ignore_words_re",
    "extend_ignore_identifiers_re",
];
