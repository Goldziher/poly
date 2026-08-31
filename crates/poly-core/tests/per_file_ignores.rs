//! End-to-end per-file-ignores: a glob-matched rule is suppressed from the
//! report AND skipped by `--fix`, while non-matching files are unaffected.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use poly_core::runner::SuppressionReason;
use poly_core::{Config, LintRun, RunOptions, lint};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn config_ignoring(glob: &str, rules: &[&str]) -> Config {
    let mut per_file_ignores = BTreeMap::new();
    per_file_ignores.insert(glob.to_string(), rules.iter().map(|r| r.to_string()).collect());
    Config {
        per_file_ignores,
        ..Config::default()
    }
}

fn opts() -> RunOptions {
    RunOptions {
        force_exclude: false,
        fix_generated: false,
        generated: None,
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
    }
}

/// `import os` with no use is ruff F401 (unused import), and the autofix removes
/// the line. A per-file-ignore for that path must suppress the diagnostic AND
/// leave the file untouched under `--fix`.
#[test]
fn fix_does_not_rewrite_a_per_file_ignored_rule() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let ignored = root.join("tests/unused.py");
    let active = root.join("src/unused.py");
    write(&ignored, "import os\n");
    write(&active, "import os\n");

    let config = config_ignoring("tests/**", &["F401"]);
    let results = lint(&[root.to_path_buf()], &config, &opts(), true, false).unwrap();

    assert_eq!(
        fs::read_to_string(&ignored).unwrap(),
        "import os\n",
        "a per-file-ignored rule must not be auto-fixed"
    );
    assert!(
        !results.iter().any(|r| r.path == ignored),
        "the ignored file produces no reported diagnostics"
    );

    assert_ne!(
        fs::read_to_string(&active).unwrap(),
        "import os\n",
        "a non-ignored file is still auto-fixed"
    );
}

/// A Rust stub whose `todo!()` is exactly what `placeholder-implementation`
/// exists to find.
const RUST_STUB: &str = "fn stub() -> u32 {\n    todo!()\n}\n";

const PLACEHOLDER: &str = "placeholder-implementation";

/// Turn the built-in pack's `placeholder-implementation` rule on — it ships
/// `severity: off` — without touching any other rule.
fn config_enabling_placeholder(per_file_ignores: BTreeMap<String, Vec<String>>) -> Config {
    let mut astgrep = toml::Table::new();
    astgrep.insert(
        "extend_select".to_string(),
        toml::Value::Array(vec![toml::Value::String(PLACEHOLDER.to_string())]),
    );
    let mut lint = toml::Table::new();
    lint.insert("astgrep".to_string(), toml::Value::Table(astgrep));
    Config {
        lint,
        per_file_ignores,
        ..Config::default()
    }
}

/// Every `(path, code)` pair the run still reports.
fn reported(run: &LintRun) -> Vec<(std::path::PathBuf, String)> {
    let mut pairs: Vec<_> = run
        .results
        .iter()
        .flat_map(|result| {
            result
                .diagnostics
                .iter()
                .filter_map(|d| d.code.clone().map(|code| (result.path.clone(), code)))
        })
        .collect();
    pairs.sort();
    pairs
}

/// The canonical case the hardcoded `NOISY_PATH_EXCLUSIONS` table used to
/// carry, now declared as `ignores:` in `placeholder-implementation.yml`: the
/// rule never fires inside a `*_generated.rs` file, and still fires in
/// ordinary source. Both halves matter — asserting only the exclusion would
/// pass just as well against a rule that had stopped matching anything.
#[test]
fn a_pack_rule_default_path_exclusion_survives_in_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("src/frb_generated.rs"), RUST_STUB);
    write(&root.join("src/lib.rs"), RUST_STUB);

    let run = poly_core::lint_run(
        &[root.to_path_buf()],
        &config_enabling_placeholder(BTreeMap::new()),
        &opts(),
        false,
        false,
    )
    .unwrap();

    let hits: Vec<_> = reported(&run)
        .into_iter()
        .filter(|(_, code)| code == PLACEHOLDER)
        .collect();
    assert_eq!(
        hits,
        vec![(root.join("src/lib.rs"), PLACEHOLDER.to_string())],
        "the rule fires in ordinary source and nowhere else"
    );
}

/// The exclusion is no longer silent: the dropped finding is named in the
/// run's `suppressed` list, with the mechanism that dropped it.
#[test]
fn a_default_path_exclusion_names_itself_in_suppressed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("src/frb_generated.rs"), RUST_STUB);

    let run = poly_core::lint_run(
        &[root.to_path_buf()],
        &config_enabling_placeholder(BTreeMap::new()),
        &opts(),
        false,
        false,
    )
    .unwrap();

    let entries: Vec<_> = run
        .suppressed
        .iter()
        .filter(|s| s.code.as_deref() == Some(PLACEHOLDER))
        .collect();
    assert_eq!(entries.len(), 1, "one suppressed finding, got {:?}", run.suppressed);
    assert_eq!(entries[0].path, root.join("src/frb_generated.rs"));
    assert_eq!(entries[0].reason, SuppressionReason::DefaultPathExclusion);
}

/// Precedence is **replace, not union** — the same rule a user rule with a
/// pack rule's `id` follows. Naming the rule in `[per-file-ignores]` drops its
/// YAML-declared defaults outright, so the generated file's finding comes
/// back and the user's own glob is the only one in force.
#[test]
fn a_user_per_file_ignore_replaces_the_rules_yaml_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("src/frb_generated.rs"), RUST_STUB);
    write(&root.join("src/hand_written.rs"), RUST_STUB);

    let mut ignores = BTreeMap::new();
    ignores.insert("src/hand_written.rs".to_string(), vec![PLACEHOLDER.to_string()]);
    let run = poly_core::lint_run(
        &[root.to_path_buf()],
        &config_enabling_placeholder(ignores),
        &opts(),
        false,
        false,
    )
    .unwrap();

    let hits: Vec<_> = reported(&run)
        .into_iter()
        .filter(|(_, code)| code == PLACEHOLDER)
        .collect();
    assert_eq!(
        hits,
        vec![(root.join("src/frb_generated.rs"), PLACEHOLDER.to_string())],
        "the YAML default is replaced, so only the user's own glob suppresses"
    );
    assert!(
        run.suppressed
            .iter()
            .any(|s| s.reason == SuppressionReason::PerFileIgnore && s.path == root.join("src/hand_written.rs")),
        "the user's own entry is reported as a per-file-ignore: {:?}",
        run.suppressed
    );
}

/// The invariant that makes the `suppressed` list worth having: the
/// unfiltered finding set is reconstructible from a **single** run, without
/// re-running poly with the filters off.
#[test]
fn reported_plus_suppressed_reconstructs_the_unfiltered_findings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("tests/unused.py"), "import os\n");
    write(&root.join("src/unused.py"), "import os\n");

    let filtered = poly_core::lint_run(
        &[root.to_path_buf()],
        &config_ignoring("tests/**", &["F401"]),
        &opts(),
        false,
        false,
    )
    .unwrap();
    let raw = poly_core::lint_run(&[root.to_path_buf()], &Config::default(), &opts(), false, false).unwrap();

    let mut reconstructed = reported(&filtered);
    reconstructed.extend(
        filtered
            .suppressed
            .iter()
            .filter_map(|s| s.code.clone().map(|code| (s.path.clone(), code))),
    );
    reconstructed.sort();

    assert!(
        !filtered.suppressed.is_empty(),
        "the fixture must actually suppress something"
    );
    assert_eq!(
        reconstructed,
        reported(&raw),
        "reported + suppressed must equal the unfiltered run"
    );
}

/// `suppressed` is additive detail, not a coverage claim, so a run that
/// suppressed nothing keeps exactly the JSON shape consumers already parse.
#[test]
fn the_suppressed_array_is_omitted_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("src/clean.py"), "x = 1\n");

    let run = poly_core::lint_run(&[root.to_path_buf()], &Config::default(), &opts(), false, false).unwrap();
    let json = serde_json::to_value(poly_core::report::LintDocument::from_run(&run)).unwrap();
    assert!(
        json.get("suppressed").is_none(),
        "an empty suppressed list is omitted: {json}"
    );

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("tests/unused.py"), "import os\n");
    let run = poly_core::lint_run(
        &[root.to_path_buf()],
        &config_ignoring("tests/**", &["F401"]),
        &opts(),
        false,
        false,
    )
    .unwrap();
    let json = serde_json::to_value(poly_core::report::LintDocument::from_run(&run)).unwrap();
    let suppressed = json.get("suppressed").expect("suppressed present");
    assert_eq!(
        suppressed[0]["reason"], "per-file-ignore",
        "the reason is serialized in kebab-case: {json}"
    );
    assert_eq!(suppressed[0]["code"], "F401");
}
