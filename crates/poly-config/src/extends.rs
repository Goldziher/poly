//! `extends` — sharing configuration from local and remote base configs.
//!
//! A `poly.toml` may declare a top-level `extends` list naming one or more base
//! configs it inherits from. Each entry is either a **local path** or a **pinned
//! remote git file**, using the same `path`/`git`/`revision` vocabulary as
//! `[[hooks.sources]]` (see [`crate::HookSource`]). Bases are deep-merged *under*
//! the declaring config at the raw [`toml::Table`] level, before typed
//! deserialization, so any subset of sections can be shared and the declaring
//! file (and its `poly.local.toml`) always wins on top.
//!
//! This crate stays network-free: it only ever reads **local file paths**. A
//! [`BaseConfigResolver`] injected by the caller maps each [`ExtendsSource`] to
//! an already-materialized local file. The default [`LocalPathResolver`] handles
//! `path` sources and rejects `git` sources with a clear "use the CLI" error; the
//! `poly` CLI supplies a resolver that fetches and caches remote git bases.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

/// The default file read from a base source when `file` is omitted.
pub const DEFAULT_BASE_FILE: &str = "poly.toml";

/// One local or pinned-remote base config that a `poly.toml` `extends`.
///
/// Field vocabulary mirrors [`crate::HookSource`] for consistency: exactly one of
/// `path` (local, mutually exclusive with `git`) or `git` (repository URL) must
/// be set; a `git` source requires a nonempty `revision`; a `path` source must
/// not set `revision`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ExtendsSource {
    /// Optional stable identifier used in diagnostics and the config lock file.
    #[serde(default)]
    pub id: Option<String>,
    /// Local base path (a file, or a directory joined with `file`); XOR `git`.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Git repository URL; XOR `path`.
    #[serde(default)]
    pub git: Option<String>,
    /// Pinned git revision; required (and nonempty) for a `git` source.
    #[serde(default)]
    pub revision: Option<String>,
    /// Path to the config file within the repository/directory. Defaults to
    /// [`DEFAULT_BASE_FILE`] (`poly.toml`).
    #[serde(default)]
    pub file: Option<String>,
}

impl ExtendsSource {
    /// The config file name to read from this source's directory.
    pub fn file_or_default(&self) -> &str {
        self.file.as_deref().unwrap_or(DEFAULT_BASE_FILE)
    }

    /// A human-readable identifier for diagnostics.
    pub fn display_id(&self) -> String {
        if let Some(id) = &self.id {
            return id.clone();
        }
        match (&self.path, &self.git) {
            (Some(path), _) => path.display().to_string(),
            (_, Some(git)) => git.clone(),
            _ => "<invalid extends source>".to_string(),
        }
    }

    /// Validate the path/git/revision invariants (same rules as
    /// [`crate::HookSource`] validation).
    fn validate(&self) -> anyhow::Result<()> {
        match (&self.path, &self.git) {
            (Some(_), Some(_)) => bail!(
                "extends source {:?} must set only one of `path` or `git`",
                self.display_id()
            ),
            (None, None) => bail!("extends source must set exactly one of `path` or `git`"),
            (Some(_), None) => {
                if self.revision.is_some() {
                    bail!(
                        "local extends source {:?} (`path`) must not set `revision`",
                        self.display_id()
                    );
                }
            }
            (None, Some(_)) => {
                if self.revision.as_deref().unwrap_or("").is_empty() {
                    bail!(
                        "git extends source {:?} requires a nonempty `revision`",
                        self.display_id()
                    );
                }
            }
        }
        // Reject values that could escape the base checkout or be parsed as a git
        // option (argument injection). `file` is always a path *within* the base,
        // so it must be relative and free of `..`; a leading `-` on any field
        // could be treated as a flag by git.
        if let Some(file) = &self.file {
            // ~keep Inspected as a string, not via `std::path`, because `Path` parses
            // per-host: `/etc/passwd` is rooted but NOT absolute on Windows (absolute
            // needs a drive or UNC prefix), so `is_absolute()` let it through there,
            // and conversely `C:\x` and `a\..\b` are a plain filename and a single
            // component on Unix. `file` is portable config resolved inside a checkout,
            // so the same value must be accepted or rejected on every host.
            if is_rooted(file) {
                bail!(
                    "extends source {:?} `file` must be relative, not {:?}",
                    self.display_id(),
                    file
                );
            }
            if file.split(['/', '\\']).any(|segment| segment == "..") {
                bail!(
                    "extends source {:?} `file` must not contain `..`: {:?}",
                    self.display_id(),
                    file
                );
            }
        }
        for (label, value) in [
            ("file", self.file.as_deref()),
            ("revision", self.revision.as_deref()),
            ("id", self.id.as_deref()),
        ] {
            if value.is_some_and(|v| v.starts_with('-')) {
                bail!(
                    "extends source {:?} `{label}` must not start with `-` (would be parsed as a git option)",
                    self.display_id()
                );
            }
        }
        Ok(())
    }
}

/// Whether `file` is rooted rather than relative, on any host: a POSIX or UNC root
/// (`/etc/passwd`, `\\server\share`) or a drive qualifier (`C:\x`, and also `C:x`,
/// which resolves against that drive's current directory rather than the checkout).
fn is_rooted(file: &str) -> bool {
    if file.starts_with('/') || file.starts_with('\\') {
        return true;
    }
    let mut chars = file.chars();
    matches!((chars.next(), chars.next()), (Some(drive), Some(':')) if drive.is_ascii_alphabetic())
}

/// Remove and parse the top-level `extends` key from a raw config table.
///
/// Returns the parsed, validated sources in declared order (empty when the key is
/// absent). Removing the key means it never reaches `RawPolyConfig`, so no schema
/// field is needed for it. Accepts a single path/table or an array of them.
pub fn take_extends(table: &mut toml::Table) -> anyhow::Result<Vec<ExtendsSource>> {
    let Some(value) = table.remove("extends") else {
        return Ok(Vec::new());
    };
    let entries = match value {
        toml::Value::Array(entries) => entries,
        other => vec![other],
    };
    let mut sources = Vec::with_capacity(entries.len());
    for entry in entries {
        let source = parse_entry(entry)?;
        source.validate()?;
        sources.push(source);
    }
    Ok(sources)
}

/// Parse one `extends` array entry. A string is a local-path shorthand; a table
/// is a full [`ExtendsSource`] (whose `deny_unknown_fields` yields a precise
/// "unknown field" error). Any other TOML type is rejected.
fn parse_entry(value: toml::Value) -> anyhow::Result<ExtendsSource> {
    match value {
        toml::Value::String(path) => Ok(ExtendsSource {
            path: Some(PathBuf::from(path)),
            ..ExtendsSource::default()
        }),
        table @ toml::Value::Table(_) => table.try_into().context("invalid `extends` table entry"),
        other => bail!(
            "`extends` entry must be a path string or a `{{ path | git }}` table, found {}",
            other.type_str()
        ),
    }
}

/// Maps an [`ExtendsSource`] to an already-materialized **local** config file.
///
/// This is the network boundary: `poly-config` calls it and merges only the local
/// path it returns. The default [`LocalPathResolver`] rejects `git` sources; the
/// CLI implements a resolver that fetches and caches remote git bases first.
///
/// The `Send + Sync + Debug` bounds let a resolver be stored behind an
/// `Arc<dyn BaseConfigResolver>` in `poly-core`'s run options.
pub trait BaseConfigResolver: Send + Sync + std::fmt::Debug {
    /// Resolve `source` to an existing local config file path. `relative_to` is
    /// the directory of the config that declared the `extends` entry.
    fn resolve(&self, source: &ExtendsSource, relative_to: &Path) -> anyhow::Result<PathBuf>;
}

/// The default resolver: resolves `path` sources against the declaring config's
/// directory and rejects `git` sources (which require the CLI's remote resolver).
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalPathResolver;

impl BaseConfigResolver for LocalPathResolver {
    fn resolve(&self, source: &ExtendsSource, relative_to: &Path) -> anyhow::Result<PathBuf> {
        let Some(path) = &source.path else {
            bail!(
                "remote git extends source {:?} requires the poly CLI resolver; \
                 loading remote configuration is not available in this context",
                source.display_id()
            );
        };
        let base = if path.is_absolute() {
            path.clone()
        } else {
            relative_to.join(path)
        };
        // Allow `path` to point either at the config file directly or at a
        // directory containing it (joined with `file`), mirroring how a git base
        // is a directory joined with `file`.
        let resolved = if base.is_dir() {
            base.join(source.file_or_default())
        } else {
            base
        };
        if !resolved.is_file() {
            bail!("extends base config not found: {}", resolved.display());
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(text: &str) -> anyhow::Result<Vec<ExtendsSource>> {
        let mut table: toml::Table = toml::from_str(text).unwrap();
        take_extends(&mut table)
    }

    #[test]
    fn absent_extends_yields_empty() {
        assert!(take("[defaults]\nline_length = 100\n").unwrap().is_empty());
    }

    #[test]
    fn string_shorthand_becomes_path_source() {
        let sources = take(r#"extends = "../shared/poly.toml""#).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].path, Some(PathBuf::from("../shared/poly.toml")));
        assert_eq!(sources[0].file_or_default(), "poly.toml");
    }

    #[test]
    fn array_of_mixed_entries_preserves_order() {
        let sources = take(
            r#"extends = [
                "./a.toml",
                { git = "https://example.com/base", revision = "abc123", file = "poly.base.toml" },
            ]"#,
        )
        .unwrap();
        assert_eq!(sources[0].path, Some(PathBuf::from("./a.toml")));
        assert_eq!(sources[1].git.as_deref(), Some("https://example.com/base"));
        assert_eq!(sources[1].file_or_default(), "poly.base.toml");
    }

    #[test]
    fn git_source_without_revision_is_rejected() {
        let error = take(r#"extends = [{ git = "https://example.com/base" }]"#).unwrap_err();
        assert!(error.to_string().contains("requires a nonempty `revision`"), "{error}");
    }

    #[test]
    fn path_source_with_revision_is_rejected() {
        let error = take(r#"extends = [{ path = "./a.toml", revision = "abc" }]"#).unwrap_err();
        assert!(error.to_string().contains("must not set `revision`"), "{error}");
    }

    #[test]
    fn source_with_both_path_and_git_is_rejected() {
        let error = take(r#"extends = [{ path = "./a.toml", git = "https://x/y", revision = "abc" }]"#).unwrap_err();
        assert!(error.to_string().contains("only one of"), "{error}");
    }

    #[test]
    fn file_with_parent_dir_is_rejected() {
        let error = take(r#"extends = [{ git = "https://x/y", revision = "deadbeef", file = "../../etc/passwd" }]"#)
            .unwrap_err();
        assert!(error.to_string().contains("must not contain `..`"), "{error}");
    }

    /// A backslash-separated escape is one opaque filename to `Path` on Unix, so the
    /// `..` guard only caught it on Windows.
    #[test]
    fn file_with_backslash_parent_dir_is_rejected() {
        let error =
            take(r#"extends = [{ git = "https://x/y", revision = "deadbeef", file = "a\\..\\..\\etc" }]"#).unwrap_err();
        assert!(error.to_string().contains("must not contain `..`"), "{error}");
    }

    /// Every spelling of "not relative" must be rejected on every host — a rooted
    /// POSIX path, a drive-qualified Windows path, and a UNC share. `/etc/passwd`
    /// is rooted but *not* absolute on Windows, so checking `is_absolute()` alone
    /// let it through there.
    #[test]
    fn absolute_file_is_rejected() {
        for file in [
            "/etc/passwd",
            r"C:\Windows\system.ini",
            r"C:relative",
            r"\\server\share\x",
        ] {
            let config = format!(r#"extends = [{{ git = "https://x/y", revision = "deadbeef", file = {file:?} }}]"#);
            let error = take(&config).unwrap_err();
            assert!(
                error.to_string().contains("must be relative"),
                "{file:?} must be rejected as non-relative, got: {error}"
            );
        }
    }

    #[test]
    fn revision_starting_with_dash_is_rejected() {
        let error = take(r#"extends = [{ git = "https://x/y", revision = "--upload-pack=/tmp/x" }]"#).unwrap_err();
        assert!(error.to_string().contains("must not start with `-`"), "{error}");
    }

    #[test]
    fn unknown_field_is_rejected() {
        let error = take(r#"extends = [{ path = "./a.toml", bogus = true }]"#).unwrap_err();
        let chain = format!("{error:#}");
        assert!(chain.contains("unknown field") || chain.contains("bogus"), "{chain}");
    }

    #[test]
    fn local_resolver_rejects_git_source() {
        let source = ExtendsSource {
            git: Some("https://example.com/base".into()),
            revision: Some("abc123".into()),
            ..ExtendsSource::default()
        };
        let error = LocalPathResolver.resolve(&source, Path::new("/tmp")).unwrap_err();
        assert!(error.to_string().contains("requires the poly CLI resolver"), "{error}");
    }

    #[test]
    fn local_resolver_errors_on_missing_file() {
        let source = ExtendsSource {
            path: Some(PathBuf::from("does-not-exist.toml")),
            ..ExtendsSource::default()
        };
        let error = LocalPathResolver.resolve(&source, Path::new("/tmp")).unwrap_err();
        assert!(error.to_string().contains("not found"), "{error}");
    }
}
