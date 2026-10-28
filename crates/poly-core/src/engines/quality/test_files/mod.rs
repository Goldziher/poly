//! `uncollected-test-file`: a file named like a test that the language's test
//! runner will not collect (ADR 0036).
//!
//! A green test run cannot show a test that never ran. pytest collects only
//! `test_*.py` / `*_test.py` by default, so a suite of `parser.test.py` files
//! passes `pytest` by running none of them. This rule reads the path alone —
//! no parse, no file content — and reports such a name at lint time.
//!
//! # What counts as a test
//!
//! A file is a candidate only when its **name** says so, in one of two shapes:
//!
//! - **Dotted**, anywhere: the stem ends in `.test` or `.spec` (`parser.test.py`).
//!   That shape is a test-file convention and nothing else.
//! - **Joined with `_` or `-`**, only inside a test directory (`test/`, `tests/`,
//!   `spec/`, `specs/`, `__tests__/`): the first or last part of the stem is a
//!   test word (`tests/parser-test.py`, `tests/test-parser.py`). Outside one the
//!   same shape names tools too often to be read as a test — a
//!   `scripts/test-snippets.py` that runs something, a gRPC `soak_tests.go` that
//!   defines test *cases* for a client binary, an `integration-tests.py` checker.
//!
//! Go reads only the dotted shape. It has no test-directory convention — a
//! `test/` directory is an ordinary package, and protobuf's own
//! `internal/testprotos/test/test_public.pb.go` is source, not a test — so a
//! joined name there says nothing about intent.
//!
//! The test words depend on the language: Python and Go use `test` / `tests`
//! only, since `spec` there means a specification (`spec_types.go`,
//! `spec-coverage.py`); Ruby, JavaScript, TypeScript and configured languages
//! add `spec` / `specs`.
//!
//! Sitting under `tests/` without a test word is not enough, because that is
//! where helper modules (`tests/factories.py`) live too, and the runner is
//! right not to collect them. A name containing a helper word (`helper`,
//! `utils`, `setup`, `fixtures`, …) is not a test, nor is a file under a helper
//! directory (`fixtures/`, `testdata/`, `__mocks__/`, `support/`, …, plus `lib/`,
//! `utils/` and the like when they sit inside a test directory).
//!
//! # Per-language defaults
//!
//! | Language | Collected by default | On by default? |
//! |---|---|---|
//! | Python | `test_*.py`, `*_test.py` (pytest) | yes |
//! | Go | `*_test.go` (`go test`) | yes |
//! | Ruby | `*_spec.rb`, `test_*.rb`, `*_test.rb` (RSpec, Minitest) | yes |
//! | JS / TS | `*.test.*`, `*.spec.*`, `**/__tests__/**` (Jest, Vitest) | no |
//! | anything else | — | no |
//!
//! JavaScript and TypeScript ship off because their runners disagree: Deno and
//! `node --test` collect `*_test.ts`, which Jest and Vitest do not, so no one
//! default is right for the ecosystem. Every other language has no default
//! pattern at all; configuring `test_file_patterns` or `forbid_test_files` for
//! it is what switches the rule on.

use std::path::{Component, Path};

use anyhow::Context as _;
use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::engine::{Diagnostic, Severity, Span};
use crate::language::Language;

/// The diagnostic code this rule reports.
pub const CODE: &str = "uncollected-test-file";

/// The resolved `uncollected-test-file` options for one file's language.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestFileSettings {
    /// `uncollected_test_file`. `None` means "the language's default", which is
    /// on where a runner default exists and is unambiguous (see the module docs),
    /// and on anywhere `patterns` or `forbid` were configured.
    pub enabled: Option<bool>,
    /// `test_file_patterns`: the globs the runner collects, replacing the
    /// language default. A pattern without `/` matches the file name, one with
    /// `/` the whole forward-slash path.
    pub patterns: Option<Vec<String>>,
    /// `forbid_test_files`: no test file of this language is allowed at all
    /// (e.g. a repository whose shell tests must be `.bats`).
    pub forbid: bool,
}

/// How [`classify`] judged one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The rule is off for this language.
    Disabled,
    /// The name does not look like a test, or it is a helper.
    NotATest,
    /// A test the runner collects.
    Collected,
    /// A test the runner will not collect.
    Uncollected {
        /// What the runner is: `pytest`, or `the test runner` for configured patterns.
        runner: &'static str,
        /// The patterns the name failed to match.
        expected: Vec<String>,
    },
    /// A test of a language whose tests `forbid_test_files` rules out.
    Forbidden,
}

/// The built-in knowledge for a language with a well-known runner.
struct Profile {
    runner: &'static str,
    /// Extensions the runner's patterns are about; a `.pyi` stub is Python but
    /// never a test.
    extensions: &'static [&'static str],
    patterns: &'static [&'static str],
    /// Stem parts that mark a test name in this language.
    test_words: &'static [&'static str],
    /// Whether a `_`/`-` joined name inside a test directory is a candidate, or
    /// only the dotted shape is (see the module docs).
    joined_names: bool,
    default_on: bool,
}

const JS_EXTENSIONS: &[&str] = &["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"];
const JS_PATTERNS: &[&str] = &["*.test.*", "*.spec.*", "**/__tests__/**"];

fn profile(language: &Language) -> Option<Profile> {
    let profile = match language {
        Language::Python => Profile {
            runner: "pytest",
            extensions: &["py"],
            patterns: &["test_*.py", "*_test.py"],
            test_words: TEST_ONLY_WORDS,
            joined_names: true,
            default_on: true,
        },
        Language::Go => Profile {
            runner: "go test",
            extensions: &["go"],
            patterns: &["*_test.go"],
            test_words: TEST_ONLY_WORDS,
            joined_names: false,
            default_on: true,
        },
        Language::Ruby => Profile {
            runner: "RSpec/Minitest",
            extensions: &["rb"],
            patterns: &["*_spec.rb", "test_*.rb", "*_test.rb"],
            test_words: TEST_WORDS,
            joined_names: true,
            default_on: true,
        },
        Language::JavaScript | Language::TypeScript | Language::Jsx | Language::Tsx => Profile {
            runner: "Jest/Vitest",
            extensions: JS_EXTENSIONS,
            patterns: JS_PATTERNS,
            test_words: TEST_WORDS,
            joined_names: true,
            default_on: false,
        },
        _ => return None,
    };
    Some(profile)
}

/// Stem parts that make a name a test candidate, for a language where `spec`
/// names a test (RSpec, Jest) and for any language without a profile.
const TEST_WORDS: &[&str] = &["test", "tests", "spec", "specs"];

/// The test words for a language where `spec` means a specification.
const TEST_ONLY_WORDS: &[&str] = &["test", "tests"];

/// Stem parts that mark a support file rather than a test, wherever they sit.
const HELPER_WORDS: &[&str] = &[
    "helper",
    "helpers",
    "util",
    "utils",
    "support",
    "setup",
    "teardown",
    "fixture",
    "fixtures",
    "factory",
    "factories",
    "mock",
    "mocks",
    "stub",
    "stubs",
    "data",
    "config",
    "harness",
    "conftest",
];

/// Directories whose contents are inputs or support code, never tests.
const HELPER_DIRS: &[&str] = &[
    "testdata",
    "test_data",
    "fixtures",
    "fixture",
    "__fixtures__",
    "__mocks__",
    "__snapshots__",
    "snapshots",
    "mocks",
    "helpers",
    "support",
    "factories",
    "node_modules",
    "vendor",
];

/// Directory names that are helper directories only inside a test directory:
/// `src/lib/` is source code, `tests/lib/` is test support.
const NESTED_HELPER_DIRS: &[&str] = &["lib", "utils", "util", "common", "shared", "data", "resources"];

/// Directory names that root a test tree.
const TEST_DIRS: &[&str] = &["test", "tests", "spec", "specs", "__tests__"];

/// The `uncollected-test-file` finding for `path`, if it has one.
///
/// # Errors
///
/// A configured `test_file_patterns` entry that is not a valid glob. That is a
/// configuration mistake, reported rather than treated as "matches nothing",
/// which would flag every test in the repository.
pub fn check(path: &Path, language: &Language, settings: &TestFileSettings) -> anyhow::Result<Option<Diagnostic>> {
    let title = match classify(path, language, settings)? {
        Verdict::Disabled | Verdict::NotATest | Verdict::Collected => return Ok(None),
        Verdict::Uncollected { runner, expected } => {
            let expected = expected.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ");
            format!(
                "`{}` looks like a test, but {runner} will not collect it (expects {expected})",
                file_name(path)
            )
        }
        Verdict::Forbidden => format!(
            "`{}` is a {} test, which this repository forbids (`forbid_test_files`)",
            file_name(path),
            language.display_name()
        ),
    };
    Ok(Some(Diagnostic {
        engine: "quality".to_owned(),
        code: Some(CODE.to_owned()),
        severity: Severity::Warning,
        title,
        description: None,
        span: Some(Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 1,
        }),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }))
}

/// Judge `path` against the runner conventions for `language`.
///
/// # Errors
///
/// See [`check`].
pub fn classify(path: &Path, language: &Language, settings: &TestFileSettings) -> anyhow::Result<Verdict> {
    let profile = profile(language);
    let configured = settings.patterns.is_some() || settings.forbid;
    let enabled = settings
        .enabled
        .unwrap_or(configured || profile.as_ref().is_some_and(|p| p.default_on));
    if !enabled {
        return Ok(Verdict::Disabled);
    }
    if !looks_like_test(path, profile.as_ref()) {
        return Ok(Verdict::NotATest);
    }
    if settings.forbid {
        return Ok(Verdict::Forbidden);
    }

    let (runner, expected): (&'static str, Vec<String>) = match (&settings.patterns, &profile) {
        (Some(patterns), _) => ("the test runner", patterns.clone()),
        (None, Some(profile)) => (
            profile.runner,
            profile.patterns.iter().map(|p| (*p).to_owned()).collect(),
        ),
        // Switched on with neither a runner default nor patterns to compare
        // against: there is nothing to be uncollected by.
        (None, None) => return Ok(Verdict::NotATest),
    };
    if matches_any(path, &expected)? {
        Ok(Verdict::Collected)
    } else {
        Ok(Verdict::Uncollected { runner, expected })
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The directory names above the file, outermost first, as spelled on disk.
fn directories(path: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    dirs.pop();
    dirs
}

fn looks_like_test(path: &Path, profile: Option<&Profile>) -> bool {
    let name = file_name(path).to_lowercase();
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, extension),
        _ => (name.as_str(), ""),
    };
    if let Some(profile) = profile
        && !profile.extensions.contains(&extension)
    {
        return false;
    }
    let words = profile.map_or(TEST_WORDS, |profile| profile.test_words);
    let parts: Vec<&str> = stem.split(['.', '_', '-']).filter(|part| !part.is_empty()).collect();
    if parts.iter().any(|part| HELPER_WORDS.contains(part)) {
        return false;
    }
    let dirs = directories(path);
    if in_helper_directory(&dirs) {
        return false;
    }
    let dotted = stem
        .rsplit_once('.')
        .is_some_and(|(head, word)| !head.is_empty() && words.contains(&word));
    if dotted {
        return true;
    }
    if profile.is_some_and(|profile| !profile.joined_names) {
        return false;
    }
    // `test.py`, `tests.py`, `spec.rb`: a single word is a module named after
    // testing (Django's `tests.py`, a scratch `test.go`), not a naming convention.
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return false;
    };
    parts.len() >= 2 && (words.contains(first) || words.contains(last)) && in_test_directory(&dirs)
}

fn in_test_directory(dirs: &[String]) -> bool {
    dirs.iter().any(|dir| TEST_DIRS.contains(&dir.to_lowercase().as_str()))
}

fn in_helper_directory(dirs: &[String]) -> bool {
    let mut inside_test_dir = false;
    for dir in dirs {
        let dir = dir.to_lowercase();
        if HELPER_DIRS.contains(&dir.as_str()) || (inside_test_dir && NESTED_HELPER_DIRS.contains(&dir.as_str())) {
            return true;
        }
        inside_test_dir |= TEST_DIRS.contains(&dir.as_str());
    }
    false
}

/// Whether `path` matches any of `patterns`: a pattern containing `/` against
/// the whole path (forward slashes on every platform), any other against the
/// file name alone, the way `.gitignore` reads its patterns.
fn matches_any(path: &Path, patterns: &[String]) -> anyhow::Result<bool> {
    let (name_patterns, path_patterns): (Vec<&String>, Vec<&String>) =
        patterns.iter().partition(|pattern| !pattern.contains('/'));
    let name = file_name(path);
    if !name_patterns.is_empty() && compile(&name_patterns)?.is_match(&name) {
        return Ok(true);
    }
    if path_patterns.is_empty() {
        return Ok(false);
    }
    let mut slashed = directories(path).join("/");
    slashed.push('/');
    slashed.push_str(&name);
    Ok(compile(&path_patterns)?.is_match(&slashed))
}

fn compile(patterns: &[&String]) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).with_context(|| format!("invalid `test_file_patterns` glob `{pattern}`"))?);
    }
    Ok(builder.build()?)
}

#[cfg(test)]
mod tests;
