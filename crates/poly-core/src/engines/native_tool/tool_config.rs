//! Content fingerprints of the *tool's own* config files.
//!
//! A native-toolchain backend shells out to a first-party CLI, and several of
//! those CLIs read their own project config from disk before they format or
//! lint anything. That config is a real input to the tool's output, but poly's
//! result cache never saw it: the cache key folds in the engine id, its
//! [`Engine::version`], the TOML-serialized poly-side args and the file's
//! path + bytes — and a native tool takes *no* poly-side args (`enabled` is its
//! only key). Editing `rustfmt.toml` therefore changed nothing in the key, and
//! the next run served the previous run's formatting. This module computes a
//! blake3 fingerprint of those files so a config edit invalidates the cache,
//! mirroring what `astgrep`'s `rules_hash` does for user rule directories.
//!
//! ## Which tools read a config file
//!
//! Verified empirically against the installed binaries, invoked exactly the way
//! [`super::format::format_via_tool`] / [`super::lint::lint_via_shellcheck`]
//! invoke them (stdin → stdout, poly's argv):
//!
//! | Tool                 | Reads config | Files                             |
//! |----------------------|--------------|-----------------------------------|
//! | `rustfmt`            | yes          | `.rustfmt.toml`, `rustfmt.toml`   |
//! | `shellcheck`         | yes          | `.shellcheckrc`, `shellcheckrc`   |
//! | `swift-format`       | yes          | `.swift-format`                   |
//! | `gofmt`              | no           | — (flags only, no config file)    |
//! | `zig fmt`            | no           | —                                 |
//! | `shfmt`              | no           | — (see below)                     |
//! | `google-java-format` | no           | —                                 |
//! | `ktfmt`              | no           | —                                 |
//! | `Rscript` / styler   | no           | — (`--vanilla`, no `.Rprofile`)   |
//! | `dart format`        | no           | — (see below)                     |
//! | `gleam format`       | no           | —                                 |
//!
//! Two "no" entries depend on *how* poly invokes the tool and would flip if the
//! invocation changed:
//!
//! - **`shfmt`** consults `.editorconfig` only when it is given a filename.
//!   poly pipes stdin without `--filename` and always passes `-i <width>`, and
//!   under that argv `.editorconfig` is provably inert. Adding `--filename`
//!   to [`super::spec::SHFMT_SPEC`] would make `.editorconfig` an input and
//!   require listing it here.
//! - **`dart format`** reads `formatter: page_width:` from
//!   `analysis_options.yaml` only when formatting a named file; over stdin the
//!   setting has no effect (the `--page-width` flag does, which is how the
//!   check was falsified).
//!
//! ## Bounded scope — read this before trusting the fingerprint
//!
//! The fingerprint is anchored at the **process's current directory** and walks
//! *upward* through its ancestors. It is **not** anchored per source file, even
//! though `rustfmt` and `swift-format` themselves search upward from the file's
//! own directory (poly runs those two with `current_dir` set to the file's
//! parent). Two structural facts force this:
//!
//! 1. [`Engine::version`] returns a borrowed `&str` and is called **once per
//!    file per engine** inside the runner's rayon `par_iter`. Anything computed
//!    there must be memoised, so it cannot vary per file.
//! 2. The format cache key hashes the file's **content only** — no path (see
//!    `ResultCache::single_file_digest`) — so a per-directory value could not be
//!    expressed in it even if it were free to compute.
//!
//! What follows from that:
//!
//! - **Covered:** a config file at or above the directory poly was invoked from
//!   — the overwhelmingly common `poly fmt .` at the repo root. For
//!   `shellcheck` this is *exactly* right: poly does not re-anchor the child, so
//!   shellcheck resolves `.shellcheckrc` from poly's own cwd.
//! - **Not covered:** a config file in a subdirectory *below* the invocation
//!   directory that only governs part of a monorepo. Editing it does not
//!   invalidate the cache. Scanning the whole tree for them would cost a full
//!   directory walk on every run, and a single run-wide hash still could not
//!   tell the runner which files it governs.
//! - **Not covered:** an edit made during the lifetime of one process. The
//!   fingerprint is computed once per process (it lives inside the `OnceLock`
//!   behind [`Engine::version`]), so a long-lived host — `poly mcp` — keeps the
//!   fingerprint it read at startup. Every `poly` CLI invocation is a fresh
//!   process and so always reads fresh.
//!
//! [`Engine::version`]: crate::engine::Engine::version
//! [`Engine`]: crate::engine::Engine

use std::path::Path;

/// Config files `rustfmt` reads, in the order it probes them within one
/// directory (the dot form wins on a tie). Both names are hashed when both
/// exist, so which one rustfmt picks does not affect invalidation.
pub(crate) const RUSTFMT_CONFIG_FILES: &[&str] = &[".rustfmt.toml", "rustfmt.toml"];

/// Config files `shellcheck` reads. Both the dotted and undotted spellings are
/// honoured by shellcheck itself.
pub(crate) const SHELLCHECK_CONFIG_FILES: &[&str] = &[".shellcheckrc", "shellcheckrc"];

/// Config file `swift-format` reads.
pub(crate) const SWIFT_FORMAT_CONFIG_FILES: &[&str] = &[".swift-format"];

/// Fingerprint returned when no candidate config file exists anywhere on the
/// search path.
///
/// It must be distinct from the fingerprint of an *empty* config file: an empty
/// file still contributes its own path to the hash, so it produces a blake3 hex
/// digest, never this sentinel. "config absent" and "config present but empty"
/// are different tool inputs and must key differently.
const NO_CONFIG: &str = "absent";

/// Fingerprint returned when the process has no readable current directory
/// (deleted cwd, or a permission error). Distinct from [`NO_CONFIG`] so the two
/// states never share a cache key.
const NO_ANCHOR: &str = "unanchored";

/// Fingerprint the config files named by `names`, searching upward from the
/// process's current directory.
///
/// Returns [`NO_ANCHOR`] when the current directory cannot be read. See the
/// module docs for what this does and does not cover.
///
/// # Cost
///
/// One `open` attempt per (ancestor, name) pair plus a read of each file that
/// exists — tens of microseconds, paid **once per process** because the only
/// caller stores the result in the `OnceLock` behind
/// [`Engine::version`](crate::engine::Engine::version). It must never be called
/// on the per-file path.
pub(crate) fn fingerprint(names: &[&str]) -> String {
    match std::env::current_dir() {
        Ok(anchor) => fingerprint_at(&anchor, names),
        Err(_) => NO_ANCHOR.to_owned(),
    }
}

/// [`fingerprint`] with an explicit anchor directory, so the search is testable
/// without mutating the process's current directory.
///
/// Hashes the absolute path **and** the bytes of every existing candidate found
/// in `anchor` and each of its ancestors, nearest first. Hashing the path as
/// well as the content means moving a config file up or down the tree changes
/// the fingerprint even when its bytes do not — the tool would resolve a
/// different file, so the cache must too. Hashing *every* match rather than
/// stopping at the first only ever over-invalidates, which is the safe
/// direction.
pub(crate) fn fingerprint_at(anchor: &Path, names: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut found_any = false;

    for dir in anchor.ancestors() {
        for name in names {
            let candidate = dir.join(name);
            let Ok(bytes) = std::fs::read(&candidate) else {
                continue;
            };
            hasher.update(candidate.to_string_lossy().as_bytes());
            hasher.update(b"\0");
            hasher.update(&bytes);
            hasher.update(b"\0");
            found_any = true;
        }
    }

    if found_any {
        hasher.finalize().to_hex().to_string()
    } else {
        NO_CONFIG.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A directory with no candidate config file yields the absent sentinel,
    /// not a hash.
    #[test]
    fn missing_config_yields_the_absent_sentinel() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(fingerprint_at(dir.path(), RUSTFMT_CONFIG_FILES), NO_CONFIG);
    }

    /// An *empty* config file is a different tool input from no config file at
    /// all, so it must not collide with the absent sentinel.
    #[test]
    fn empty_config_hashes_distinctly_from_a_missing_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = fingerprint_at(dir.path(), RUSTFMT_CONFIG_FILES);
        fs::write(dir.path().join("rustfmt.toml"), "").expect("write empty config");
        let empty = fingerprint_at(dir.path(), RUSTFMT_CONFIG_FILES);

        assert_eq!(missing, NO_CONFIG);
        assert_ne!(empty, NO_CONFIG, "an empty config file must still hash");
        assert_ne!(empty, missing);
    }

    /// The whole point: editing the config content changes the fingerprint.
    #[test]
    fn editing_the_config_changes_the_fingerprint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rustfmt.toml");
        fs::write(&path, "max_width = 120\n").expect("write config");
        let before = fingerprint_at(dir.path(), RUSTFMT_CONFIG_FILES);
        fs::write(&path, "max_width = 40\n").expect("rewrite config");
        let after = fingerprint_at(dir.path(), RUSTFMT_CONFIG_FILES);

        assert_ne!(before, after, "a config edit must change the fingerprint");
    }

    /// The same content in a different directory of the search path is a
    /// different resolution for the tool, so it must hash differently.
    #[test]
    fn the_same_content_at_a_different_depth_hashes_differently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).expect("mkdir");

        fs::write(dir.path().join("rustfmt.toml"), "max_width = 40\n").expect("write parent config");
        let parent_only = fingerprint_at(&nested, RUSTFMT_CONFIG_FILES);

        fs::write(nested.join("rustfmt.toml"), "max_width = 40\n").expect("write nested config");
        let both = fingerprint_at(&nested, RUSTFMT_CONFIG_FILES);

        assert_ne!(parent_only, both, "a nearer config file must change the fingerprint");
    }

    /// A config file above the anchor is found: the search walks ancestors, as
    /// `rustfmt` / `shellcheck` / `swift-format` all do.
    #[test]
    fn a_config_file_in_an_ancestor_is_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let deep = dir.path().join("a").join("b");
        fs::create_dir_all(&deep).expect("mkdir -p");
        fs::write(dir.path().join(".shellcheckrc"), "disable=SC2086\n").expect("write config");

        assert_ne!(
            fingerprint_at(&deep, SHELLCHECK_CONFIG_FILES),
            NO_CONFIG,
            "an ancestor's config file must be part of the fingerprint"
        );
    }

    /// Each tool only hashes the files it actually reads: a `rustfmt.toml`
    /// must not perturb shellcheck's fingerprint.
    #[test]
    fn an_unrelated_tools_config_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let before = fingerprint_at(dir.path(), SHELLCHECK_CONFIG_FILES);
        fs::write(dir.path().join("rustfmt.toml"), "max_width = 40\n").expect("write config");

        assert_eq!(fingerprint_at(dir.path(), SHELLCHECK_CONFIG_FILES), before);
    }

    /// A tool with no config files is a constant: no filesystem access, always
    /// the absent sentinel.
    #[test]
    fn a_tool_with_no_config_files_is_always_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("rustfmt.toml"), "max_width = 40\n").expect("write config");
        assert_eq!(fingerprint_at(dir.path(), &[]), NO_CONFIG);
    }
}
