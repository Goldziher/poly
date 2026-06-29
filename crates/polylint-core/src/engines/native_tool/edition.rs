//! Rust edition resolution for the `rustfmt` native-tool backend.
//!
//! `rustfmt` defaults to edition 2015 when invoked without `--edition`, so it
//! reformats edition-2024 source that `cargo fmt` (which always passes the
//! manifest edition) considers clean. To match `cargo fmt`, this module walks
//! up from a file to its nearest `Cargo.toml`, reads `package.edition`, and
//! follows `edition.workspace = true` inheritance up to the workspace root's
//! `[workspace.package] edition`.
//!
//! Resolution is cached by the starting directory in a process-wide
//! `Mutex<HashMap<…>>` so the manifest walk runs at most once per directory —
//! the engine runs inside a rayon `par_iter`, so the cache must be `Send + Sync`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Edition assumed when no `Cargo.toml` (or no edition declaration) can be
/// found for a file. Cargo itself defaults absent editions to 2015, but a
/// loose `.rs` file outside any crate is most plausibly modern, and 2021 is the
/// conservative choice that avoids spurious 2015-only reformatting.
const FALLBACK_EDITION: &str = "2021";

/// Process-wide cache mapping a starting directory to its resolved edition.
/// Keyed by the file's parent directory: every file in the same directory
/// shares one walk, so the manifest lookup is not repeated per file.
fn cache() -> &'static Mutex<HashMap<std::path::PathBuf, &'static str>> {
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, &'static str>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve the Rust edition that applies to `path` by walking up to the nearest
/// `Cargo.toml` and following workspace inheritance. Returns a `'static` edition
/// string (an interned, owned-by-the-cache value) so callers avoid an
/// allocation on the per-file hot path. Falls back to [`FALLBACK_EDITION`].
pub(crate) fn resolve_edition(path: &Path) -> &'static str {
    let start_dir = path.parent().unwrap_or(path);

    if let Some(found) = cache()
        .lock()
        .expect("edition cache mutex poisoned")
        .get(start_dir)
    {
        return found;
    }

    let resolved = intern(&compute_edition(start_dir));
    cache()
        .lock()
        .expect("edition cache mutex poisoned")
        .insert(start_dir.to_path_buf(), resolved);
    resolved
}

/// Intern an edition string into a `'static` slot. Only a handful of distinct
/// editions exist (2015/2018/2021/2024 + fallback), so the leak is bounded and
/// one-time per distinct value — never per file.
fn intern(edition: &str) -> &'static str {
    static INTERNED: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let table = INTERNED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = table.lock().expect("edition intern mutex poisoned");
    if let Some(existing) = guard.get(edition) {
        return existing;
    }
    let leaked: &'static str = Box::leak(edition.to_owned().into_boxed_str());
    guard.insert(edition.to_owned(), leaked);
    leaked
}

/// Walk from `start_dir` up through its ancestors, returning the first edition
/// we can resolve. A member crate's `package.edition` may be a concrete string
/// (returned directly) or `{ workspace = true }` / absent (keep walking until a
/// `[workspace.package] edition` is found).
fn compute_edition(start_dir: &Path) -> String {
    for dir in start_dir.ancestors() {
        let manifest = dir.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(table) = text.parse::<toml::Table>() else {
            continue;
        };

        // A concrete `[package] edition = "…"` wins immediately.
        if let Some(edition) = table
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|package| package.get("edition"))
            .and_then(toml::Value::as_str)
        {
            return edition.to_owned();
        }

        // Otherwise this manifest may be (or also be) the workspace root that
        // members inherit from via `edition.workspace = true`.
        if let Some(edition) = table
            .get("workspace")
            .and_then(toml::Value::as_table)
            .and_then(|workspace| workspace.get("package"))
            .and_then(toml::Value::as_table)
            .and_then(|package| package.get("edition"))
            .and_then(toml::Value::as_str)
        {
            return edition.to_owned();
        }
        // Member manifest with `edition.workspace = true` (or no edition at
        // all): fall through to keep walking up toward the workspace root.
    }
    FALLBACK_EDITION.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path inside this workspace must resolve to the workspace edition
    /// (`2024`): member crates declare `edition.workspace = true` and the root
    /// `Cargo.toml` sets `[workspace.package] edition = "2024"`.
    #[test]
    fn resolves_workspace_edition_to_2024() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
        assert_eq!(resolve_edition(&path), "2024");
    }

    /// A path with no enclosing `Cargo.toml` falls back to the default edition.
    #[test]
    fn falls_back_when_no_manifest_found() {
        // `/` has no Cargo.toml above it on any supported platform.
        let resolved = compute_edition(Path::new("/"));
        assert_eq!(resolved, FALLBACK_EDITION);
    }
}
