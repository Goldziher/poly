//! File-pattern and tag-based filtering for hook execution.
//!
//! Ported from `polyhooks/src/cli/run/filter.rs` and the relevant sections
//! of `polyhooks/src/config.rs`. The `ProjectFiles`, `CollectOptions`,
//! `collect_run_input`, and related workspace-coupled helpers are intentionally
//! **not ported** — they belong to the B1 hook-runner phase.

use std::cell::OnceCell;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use polyhooks::identify::{TagSet, tags_from_path};
use tracing::error;

// ── GlobPatterns ─────────────────────────────────────────────────────────────

/// A compiled set of glob patterns that can match file paths.
#[derive(Clone)]
pub struct GlobPatterns {
    patterns: Vec<String>,
    set: GlobSet,
}

impl GlobPatterns {
    /// Compile a set of glob patterns.
    ///
    /// # Errors
    ///
    /// Returns `Err` if any pattern is invalid glob syntax.
    pub fn new(patterns: Vec<String>) -> Result<Self, globset::Error> {
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            builder.add(Glob::new(pattern)?);
        }
        let set = builder.build()?;
        Ok(Self { patterns, set })
    }

    /// Return `true` if the pattern list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Return `true` if `path` matches at least one glob in the set.
    #[must_use]
    pub fn is_match(&self, path: &Path) -> bool {
        self.set.is_match(path)
    }
}

impl std::fmt::Debug for GlobPatterns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobPatterns")
            .field("patterns", &self.patterns)
            .finish_non_exhaustive()
    }
}

// ── FilePattern ───────────────────────────────────────────────────────────────

/// A file-matching pattern: never match, a regex, or a set of globs.
#[derive(Debug, Clone)]
pub enum FilePattern {
    /// Never match any file.
    Never,
    /// Match via a regular expression (text match on the path string).
    ///
    /// Backed by the linear-time [`regex`] crate rather than `fancy_regex`: the
    /// patterns here come from user-supplied `files`/`exclude` config, and
    /// `fancy_regex`'s backtracking engine has exponential worst-case behaviour
    /// (`ReDoS`). `regex` is guaranteed linear and rejects lookaround/backrefs
    /// at compile time, so a hostile pattern cannot wedge the runner.
    Regex(regex::Regex),
    /// Match via one or more glob patterns.
    Glob(GlobPatterns),
}

impl FilePattern {
    /// Create a glob pattern from a list of glob strings.
    ///
    /// # Errors
    ///
    /// Returns `Err` if any pattern is invalid glob syntax.
    pub fn glob(patterns: Vec<String>) -> Result<Self, globset::Error> {
        Ok(Self::Glob(GlobPatterns::new(patterns)?))
    }

    /// Create a regex pattern.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the pattern is invalid regex syntax, or uses a feature
    /// (lookaround / backreferences) the linear-time engine rejects.
    pub fn regex(pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self::Regex(regex::Regex::new(pattern)?))
    }

    /// Return `true` if `path` matches this pattern.
    ///
    /// Regex patterns require the path to be valid UTF-8; non-UTF-8 paths
    /// return `false`. Glob patterns can match non-UTF-8 OS paths directly.
    #[must_use]
    pub fn is_match(&self, path: &Path) -> bool {
        match self {
            Self::Never => false,
            Self::Regex(regex) => path.to_str().is_some_and(|p| regex.is_match(p)),
            Self::Glob(globs) => globs.is_match(path),
        }
    }
}

impl std::fmt::Display for FilePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Never => f.write_str("never"),
            Self::Regex(regex) => write!(f, "regex: {}", regex.as_str()),
            Self::Glob(globs) => {
                let patterns = globs.patterns.join(", ");
                write!(f, "glob: [{patterns}]")
            }
        }
    }
}

// ── FilenameFilter ────────────────────────────────────────────────────────────

/// Filter filenames by optional include and exclude patterns.
///
/// A path passes if:
/// - `include` is `None` OR the path matches the include pattern, AND
/// - `exclude` is `None` OR the path does NOT match the exclude pattern.
pub struct FilenameFilter<'a> {
    include: Option<&'a FilePattern>,
    exclude: Option<&'a FilePattern>,
}

impl<'a> FilenameFilter<'a> {
    /// Create a filter from optional include and exclude patterns.
    #[must_use]
    pub fn new(include: Option<&'a FilePattern>, exclude: Option<&'a FilePattern>) -> Self {
        Self { include, exclude }
    }

    /// Return `true` if `filename` passes both the include and exclude filters.
    #[must_use]
    pub fn matches(&self, filename: &Path) -> bool {
        if let Some(pattern) = self.include {
            if !pattern.is_match(filename) {
                return false;
            }
        }
        if let Some(pattern) = self.exclude {
            if pattern.is_match(filename) {
                return false;
            }
        }
        true
    }
}

// ── FileTagFilter ─────────────────────────────────────────────────────────────

/// Filter files by tag intersection: `all`, `any`, and `exclude` tag sets.
pub struct FileTagFilter<'a> {
    /// ALL of these tags must be present.
    all: Option<&'a TagSet>,
    /// AT LEAST ONE of these tags must be present (ignored if empty).
    any: Option<&'a TagSet>,
    /// NONE of these tags may be present.
    exclude: Option<&'a TagSet>,
}

impl<'a> FileTagFilter<'a> {
    /// Create a tag filter from optional `all`/`any`/`exclude` tag sets.
    #[must_use]
    pub fn new(
        types: Option<&'a TagSet>,
        types_or: Option<&'a TagSet>,
        exclude_types: Option<&'a TagSet>,
    ) -> Self {
        Self {
            all: types,
            any: types_or,
            exclude: exclude_types,
        }
    }

    /// Return `true` if `file_types` passes all three tag constraints.
    #[must_use]
    pub fn matches(&self, file_types: &TagSet) -> bool {
        if self.all.is_some_and(|s| !s.is_subset(file_types)) {
            return false;
        }
        if self
            .any
            .is_some_and(|s| !s.is_empty() && s.is_disjoint(file_types))
        {
            return false;
        }
        if self.exclude.is_some_and(|s| !s.is_disjoint(file_types)) {
            return false;
        }
        true
    }
}

// ── HookFileFilter ────────────────────────────────────────────────────────────

/// Combined filename + tag filter for a hook's file selection criteria.
///
/// Unlike the upstream `polyhooks` version, this constructor takes explicit
/// optional patterns rather than a `&Hook` reference — the `Hook` model is
/// defined in the B1 phase.
pub struct HookFileFilter<'a> {
    filename: FilenameFilter<'a>,
    tags: FileTagFilter<'a>,
}

impl<'a> HookFileFilter<'a> {
    /// Build a filter from explicit file and tag selectors.
    ///
    /// All parameters are optional; `None` means "no constraint on this axis".
    #[must_use]
    pub fn new(
        files: Option<&'a FilePattern>,
        exclude: Option<&'a FilePattern>,
        types: Option<&'a TagSet>,
        types_or: Option<&'a TagSet>,
        exclude_types: Option<&'a TagSet>,
    ) -> Self {
        Self {
            filename: FilenameFilter::new(files, exclude),
            tags: FileTagFilter::new(types, types_or, exclude_types),
        }
    }

    /// Return `true` if `filename` passes the filename filter.
    #[must_use]
    pub fn matches_filename(&self, filename: &Path) -> bool {
        self.filename.matches(filename)
    }

    /// Return `true` if `tags` passes the tag filter.
    ///
    /// `None` (tags could not be determined) always returns `false` so that
    /// files whose type cannot be identified are excluded rather than
    /// accidentally included.
    #[must_use]
    pub fn matches_tags(&self, tags: Option<&TagSet>) -> bool {
        tags.is_some_and(|tags| self.tags.matches(tags))
    }

    /// Return `true` if `filename` passes both the filename and tag filters,
    /// looking up tags via `tag_cache`.
    pub fn matches<'p>(&self, filename: &'p Path, tag_cache: &FileTagCache<'p>) -> bool {
        if !self.matches_filename(filename) {
            return false;
        }
        self.matches_tags(tag_cache.tags_for(filename))
    }
}

// ── FileTagCache ──────────────────────────────────────────────────────────────

/// Per-file tag cache — computes tags lazily via [`tags_from_path`] and
/// memoises the result in a `OnceCell` for re-use within a single hook run.
#[derive(Default)]
pub struct FileTagCache<'a> {
    paths: Vec<&'a Path>,
    tags_by_file: Vec<OnceCell<Option<TagSet>>>,
}

impl<'a> FileTagCache<'a> {
    /// Build a cache over an ordered slice of paths.
    pub fn from_paths<I>(paths: I) -> Self
    where
        I: IntoIterator<Item = &'a Path>,
    {
        let paths = paths.into_iter().collect::<Vec<_>>();
        let tags_by_file = (0..paths.len()).map(|_| OnceCell::new()).collect();
        Self {
            paths,
            tags_by_file,
        }
    }

    /// Look up tags for `path`.
    ///
    /// Returns `None` if `path` is not in the cache or if tag identification
    /// failed.
    pub fn tags_for(&self, path: &Path) -> Option<&TagSet> {
        let idx = self.paths.iter().position(|&p| p == path)?;
        self.tags(idx)
    }

    /// Look up tags by index (the position used when the cache was built).
    pub fn tags(&self, file_idx: usize) -> Option<&TagSet> {
        self.tags_by_file[file_idx]
            .get_or_init(|| {
                let path = self.paths[file_idx];
                match tags_from_path(path) {
                    Ok(tags) => Some(tags),
                    Err(err) => {
                        error!(filename = ?path.display(), error = %err, "Failed to get tags");
                        None
                    }
                }
            })
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{FilePattern, FilenameFilter, GlobPatterns};

    fn glob(pattern: &str) -> FilePattern {
        FilePattern::Glob(GlobPatterns::new(vec![pattern.to_string()]).unwrap())
    }

    fn regex(pattern: &str) -> FilePattern {
        FilePattern::regex(pattern).unwrap()
    }

    #[test]
    fn filename_filter_glob_include_and_exclude() {
        let include = glob("src/**/*.rs");
        let exclude = glob("src/**/ignored.rs");
        let filter = FilenameFilter::new(Some(&include), Some(&exclude));

        assert!(filter.matches(Path::new("src/lib/main.rs")));
        assert!(!filter.matches(Path::new("src/lib/ignored.rs")));
        assert!(!filter.matches(Path::new("tests/main.rs")));
    }

    #[test]
    fn filename_filter_no_pattern_accepts_all() {
        let filter = FilenameFilter::new(None, None);
        assert!(filter.matches(Path::new("anything.py")));
    }

    #[test]
    fn filename_filter_regex_requires_utf8() {
        let include = regex(r".*\.py$");
        let filter = FilenameFilter::new(Some(&include), None);

        // A simple UTF-8 path must match.
        assert!(filter.matches(Path::new("foo.py")));
    }

    #[cfg(unix)]
    #[test]
    fn filename_filter_non_utf8_passes_when_no_pattern() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let path = Path::new(OsStr::from_bytes(b"bad-\xff.py"));
        let filter = FilenameFilter::new(None, None);
        assert!(filter.matches(path));
    }

    #[cfg(unix)]
    #[test]
    fn filename_filter_non_utf8_glob_matches() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let include = glob("**/*.py");
        let path = Path::new(OsStr::from_bytes(b"bad-\xff.py"));
        let filter = FilenameFilter::new(Some(&include), None);
        assert!(filter.matches(path));
    }

    #[cfg(unix)]
    #[test]
    fn filename_filter_non_utf8_regex_does_not_match() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let include = regex(r".*\.py$");
        let path = Path::new(OsStr::from_bytes(b"bad-\xff.py"));
        let filter = FilenameFilter::new(Some(&include), None);
        // Non-UTF-8 path → `to_str()` returns None → match returns false.
        assert!(!filter.matches(path));
    }

    #[test]
    fn file_pattern_never_matches_nothing() {
        let p = FilePattern::Never;
        assert!(!p.is_match(Path::new("anything")));
    }
}
