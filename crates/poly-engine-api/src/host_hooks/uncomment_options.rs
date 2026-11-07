//! The option keys the uncomment engine reads, which the host's config resolution merges.
//!
//! They live here rather than in the engine family so a core build, which serves uncomment from a
//! component, merges the same keys out of `poly.toml` without linking the engine.

/// The boolean option keys `[lint.uncomment]` / `[lint.<lang>.uncomment]`
/// accept.
///
/// One list, read twice: `Config::build_uncomment_options` merges exactly these
/// keys out of the user's tables, and `UncommentEngine::option_keys` declares
/// them as what the backend reads.
pub const BOOL_OPTION_KEYS: &[&str] = &[
    "enabled",
    "remove_todos",
    "remove_fixme",
    "remove_docs",
    "use_default_ignores",
    "code_only",
];

/// The array-valued option keys, merged and declared like [`BOOL_OPTION_KEYS`].
pub const ARRAY_OPTION_KEYS: &[&str] = &["preserve_patterns"];
