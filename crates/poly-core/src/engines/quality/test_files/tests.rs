//! Name classification for `uncollected-test-file`, per language.

use std::path::{Path, PathBuf};

use super::{TestFileSettings, Verdict, classify};
use crate::language::Language;

fn defaults() -> TestFileSettings {
    TestFileSettings::default()
}

fn verdict(path: &str, language: Language) -> Verdict {
    classify(Path::new(path), &language, &defaults()).expect("default patterns compile")
}

fn with(settings: &TestFileSettings, path: &str, language: Language) -> Verdict {
    classify(Path::new(path), &language, settings).expect("patterns compile")
}

fn is_uncollected(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Uncollected { .. })
}

#[test]
fn pytest_collects_its_default_names() {
    assert_eq!(verdict("tests/test_parser.py", Language::Python), Verdict::Collected);
    assert_eq!(verdict("tests/parser_test.py", Language::Python), Verdict::Collected);
    assert_eq!(
        verdict("tests/unit/test_models.py", Language::Python),
        Verdict::Collected
    );
}

#[test]
fn pytest_misses_dotted_hyphenated_and_plural_names() {
    for path in [
        "tests/parser.test.py",
        "tests/kebab-name.test.py",
        "tests/parser-test.py",
        "tests/test-parser.py",
        "tests/parser_tests.py",
        "scripts/tests/opencode.test.py",
    ] {
        assert!(is_uncollected(&verdict(path, Language::Python)), "{path}");
    }
}

#[test]
fn python_helpers_and_non_tests_are_not_tests() {
    for path in [
        "tests/conftest.py",
        "tests/__init__.py",
        "tests/helpers.py",
        "tests/factories.py",
        "tests/test-helpers.py",
        "tests/fixtures/bad.test.py",
        "tests/helpers/parser.test.py",
        "tests/lib/runner-test.py",
        "src/latest.py",
        "src/contest_entry.py",
        "src/attestation.py",
        // Joined names outside a test directory are scripts and tools as often as tests.
        "scripts/test-snippets.py",
        "scripts/check-integration-tests.py",
        "src/pkg/parser_tests.py",
        // `spec` is a specification in Python, not a test.
        "scripts/spec-coverage.py",
        "tests/parser.spec.py",
        "test.py",
        "tests.py",
        "stubs/test_api.pyi",
    ] {
        assert_eq!(verdict(path, Language::Python), Verdict::NotATest, "{path}");
    }
}

#[test]
fn go_test_requires_the_underscore_suffix() {
    assert!(!is_uncollected(&verdict("pkg/parser_test.go", Language::Go)));
    assert!(!is_uncollected(&verdict("test/parser_linux_test.go", Language::Go)));
    assert!(is_uncollected(&verdict("pkg/parser.test.go", Language::Go)));
    assert!(is_uncollected(&verdict("pkg/parser.tests.go", Language::Go)));
    // Go has no test-directory convention, so a joined name names source files.
    assert_eq!(verdict("pkg/parser-test.go", Language::Go), Verdict::NotATest);
    assert_eq!(verdict("test/parser-test.go", Language::Go), Verdict::NotATest);
    assert_eq!(
        verdict("internal/testprotos/test/test_public.pb.go", Language::Go),
        Verdict::NotATest
    );
    assert_eq!(verdict("interop/soak_tests.go", Language::Go), Verdict::NotATest);
    assert_eq!(verdict("interp/test_classic.go", Language::Go), Verdict::NotATest);
    assert_eq!(
        verdict("internal/verifiers/run_spec.go", Language::Go),
        Verdict::NotATest
    );
    assert_eq!(verdict("pkg/testdata/bad.test.go", Language::Go), Verdict::NotATest);
    assert_eq!(verdict("pkg/testutil.go", Language::Go), Verdict::NotATest);
    assert_eq!(verdict("pkg/test_helpers.go", Language::Go), Verdict::NotATest);
}

#[test]
fn ruby_accepts_rspec_and_minitest_names() {
    assert_eq!(verdict("spec/parser_spec.rb", Language::Ruby), Verdict::Collected);
    assert_eq!(verdict("test/parser_test.rb", Language::Ruby), Verdict::Collected);
    assert_eq!(verdict("test/test_parser.rb", Language::Ruby), Verdict::Collected);
    assert!(is_uncollected(&verdict("spec/parser.spec.rb", Language::Ruby)));
    assert!(is_uncollected(&verdict("spec/parser-spec.rb", Language::Ruby)));
    assert_eq!(verdict("spec/spec_helper.rb", Language::Ruby), Verdict::NotATest);
    assert_eq!(
        verdict("spec/support/matchers_spec.rb", Language::Ruby),
        Verdict::NotATest
    );
    assert_eq!(verdict("Rakefile.rb", Language::Ruby), Verdict::NotATest);
}

/// JS/TS runners disagree (Deno and `node --test` collect `*_test.ts`), so the
/// language is off unless enabled — but its patterns are Jest/Vitest's.
#[test]
fn javascript_is_off_by_default_and_uses_jest_vitest_patterns_when_on() {
    assert_eq!(verdict("src/parser_test.ts", Language::TypeScript), Verdict::Disabled);

    let on = TestFileSettings {
        enabled: Some(true),
        ..defaults()
    };
    assert_eq!(
        with(&on, "src/parser.test.ts", Language::TypeScript),
        Verdict::Collected
    );
    assert_eq!(with(&on, "src/parser.spec.tsx", Language::Tsx), Verdict::Collected);
    assert_eq!(
        with(&on, "src/__tests__/parser-test.js", Language::JavaScript),
        Verdict::Collected
    );
    assert!(is_uncollected(&with(&on, "test/parser_test.ts", Language::TypeScript)));
    assert!(is_uncollected(&with(&on, "test/test_parser.mjs", Language::JavaScript)));
    assert_eq!(with(&on, "src/test-utils.ts", Language::TypeScript), Verdict::NotATest);
    assert_eq!(
        with(&on, "types/parser.test.d.ts", Language::TypeScript),
        Verdict::NotATest
    );
}

/// Shell has no single runner, so nothing is checked until the repository says
/// what it collects.
#[test]
fn shell_is_checked_only_once_configured() {
    assert_eq!(verdict("tests/test-install.sh", Language::Shell), Verdict::Disabled);

    let patterns = TestFileSettings {
        patterns: Some(vec!["*_test.sh".to_owned()]),
        ..defaults()
    };
    assert_eq!(
        with(&patterns, "tests/install_test.sh", Language::Shell),
        Verdict::Collected
    );
    assert!(is_uncollected(&with(
        &patterns,
        "tests/test-install.sh",
        Language::Shell
    )));
}

#[test]
fn custom_patterns_replace_the_runner_defaults() {
    let settings = TestFileSettings {
        patterns: Some(vec!["*_tests.py".to_owned(), "*_test.py".to_owned()]),
        ..defaults()
    };
    // Not a pytest default, but this repository's `python_files` collects it.
    assert_eq!(
        with(&settings, "tests/parser_tests.py", Language::Python),
        Verdict::Collected
    );
    assert_eq!(
        with(&settings, "tests/parser_test.py", Language::Python),
        Verdict::Collected
    );
    // `test_*.py` is pytest's default but not this repository's.
    assert!(is_uncollected(&with(
        &settings,
        "tests/test_parser.py",
        Language::Python
    )));
    // Patterns decide collection; they never make a non-test name a test.
    assert_eq!(
        with(&settings, "tests/check_parser.py", Language::Python),
        Verdict::NotATest
    );
}

#[test]
fn a_pattern_with_a_slash_matches_the_whole_path() {
    let settings = TestFileSettings {
        patterns: Some(vec!["**/integration/*.py".to_owned()]),
        ..defaults()
    };
    assert_eq!(
        with(&settings, "tests/integration/parser.test.py", Language::Python),
        Verdict::Collected
    );
    assert!(is_uncollected(&with(
        &settings,
        "tests/unit/parser.test.py",
        Language::Python
    )));
}

#[test]
fn forbid_flags_every_test_shaped_file_of_the_language() {
    let settings = TestFileSettings {
        forbid: true,
        ..defaults()
    };
    assert_eq!(
        with(&settings, "tests/test-install.sh", Language::Shell),
        Verdict::Forbidden
    );
    assert_eq!(
        with(&settings, "tests/install_test.sh", Language::Shell),
        Verdict::Forbidden
    );
    assert_eq!(
        with(&settings, "tests/lib/common.sh", Language::Shell),
        Verdict::NotATest
    );
}

#[test]
fn explicit_false_turns_a_default_on_language_off() {
    let settings = TestFileSettings {
        enabled: Some(false),
        forbid: true,
        patterns: Some(vec!["*.py".to_owned()]),
    };
    assert_eq!(
        with(&settings, "tests/parser.test.py", Language::Python),
        Verdict::Disabled
    );
}

#[test]
fn an_invalid_pattern_is_an_error_not_a_silent_pass() {
    let settings = TestFileSettings {
        patterns: Some(vec!["test_[.py".to_owned()]),
        ..defaults()
    };
    let error = classify(Path::new("tests/parser.test.py"), &Language::Python, &settings).unwrap_err();
    assert!(error.to_string().contains("test_[.py"), "{error}");
}

/// Path components are read with `Path::components`, so a Windows path
/// classifies exactly like its forward-slash spelling.
#[test]
fn native_separators_classify_like_forward_slashes() {
    let path: PathBuf = ["tests", "fixtures", "bad.test.py"].iter().collect();
    assert_eq!(
        classify(&path, &Language::Python, &defaults()).unwrap(),
        Verdict::NotATest
    );
    let path: PathBuf = ["src", "__tests__", "parser_test.ts"].iter().collect();
    let on = TestFileSettings {
        enabled: Some(true),
        ..defaults()
    };
    assert_eq!(classify(&path, &Language::TypeScript, &on).unwrap(), Verdict::Collected);
}

#[test]
fn diagnostic_names_the_runner_and_expected_patterns() {
    let diagnostic = super::check(Path::new("tests/parser.test.py"), &Language::Python, &defaults())
        .unwrap()
        .expect("finding");
    assert_eq!(diagnostic.code.as_deref(), Some("uncollected-test-file"));
    assert_eq!(diagnostic.severity, crate::engine::Severity::Warning);
    assert_eq!(
        diagnostic.title,
        "`parser.test.py` looks like a test, but pytest will not collect it (expects `test_*.py`, `*_test.py`)"
    );
}
