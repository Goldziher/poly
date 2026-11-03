//! Tests for the v1 to v2 rewrite. Snapshots cover representative v1 configs; the rest pin the
//! properties the snapshots cannot: idempotence, comment survival and that the output is a v2 config.

use std::fs;

use poly_config::v2::{PolyConfigV2, detect_v1};
use toml_edit::DocumentMut;

use super::{V2Options, apply_to_plan, lock_file_notes, migrate_document};
use crate::migrate::build_plan;

fn options() -> V2Options {
    V2Options {
        required_version: Some(">=2.0.0".to_string()),
        keep_typos_default: true,
    }
}

fn migrate(source: &str) -> String {
    let mut doc: DocumentMut = source.parse().expect("fixture is valid TOML");
    migrate_document(&mut doc, &options()).expect("fixture migrates");
    doc.to_string()
}

fn parsed(output: &str) -> PolyConfigV2 {
    toml::from_str(output).expect("output parses as a v2 config")
}

const PER_LANGUAGE: &str = r#"#:schema https://goldziher.github.io/poly/poly.schema.json

[discovery]
exclude = ["target/**"]

# Python lint settings
[lint.python.ruff]
# the rule families we enforce
select = ["E", "F"]
line_length = 120
target_version = "py312"

[lint.python.ruff.rules.ANN201]
level = "error"

[fmt.python.ruff]
line_length = 120
quote_style = "double"

[lint.typescript.oxc]
categories = ["correctness"]
max_warnings = 0

[lint.javascript.oxc]
categories = ["correctness"]
max_warnings = 10

[fmt.typescript.oxc]
semi = true
print_width = 100

[fmt.javascript.oxc]
semi = true
print_width = 80
"#;

const CROSS_CUTTING: &str = r#"[lint.typos]
extend_ignore_words = ["teh"]

[lint.python.typos]
extend_ignore_words = ["certifi"]

# quality thresholds
[lint.quality]
function_too_long_lines = 100
cyclomatic_complexity_max = 15

[lint.uncomment]
enabled = true
remove_todos = false

[lint.astgrep.rules.no-unwrap]
level = "error"
"#;

const RULES_TABLE: &str = r#"[rules]
# user rule directories
dirs = [".poly/rules", "tools/rules"]
builtin = false

[lint.astgrep]
select = ["no-unwrap"]
"#;

const WORKSPACE_KEYS: &str = r#"[workspace]
# stop the config cascade here
root = true

[lint]
# the whole-project phase is slow in this repo
workspace = false

[lint.python.ruff]
select = ["E"]
"#;

const PER_FILE_IGNORES: &str = r#"[lint.python.ruff]
select = ["E"]

# tests may be loose
[per-file-ignores]
"tests/**" = ["F401"]
"**/generated/**" = ["comment"]
"#;

#[test]
fn rewrites_this_repos_own_poly_toml() {
    let output = migrate(include_str!("../../../../../poly.toml"));
    insta::assert_snapshot!("repo_poly_toml", output);
    parsed(&output);
}

#[test]
fn per_language_and_per_phase_tables_are_hoisted() {
    let output = migrate(PER_LANGUAGE);
    insta::assert_snapshot!("per_language_per_phase", output);
    parsed(&output);
}

#[test]
fn cross_cutting_tables_become_engine_tables() {
    let output = migrate(CROSS_CUTTING);
    insta::assert_snapshot!("cross_cutting", output);
    parsed(&output);
}

#[test]
fn rules_table_becomes_the_astgrep_engine() {
    let output = migrate(RULES_TABLE);
    insta::assert_snapshot!("rules_table", output);
    let config = parsed(&output);
    let astgrep = &config.engines["astgrep"];
    assert!(astgrep.options.contains_key("rule_dirs"));
    assert_eq!(astgrep.options["builtin_pack"].as_bool(), Some(false));
}

#[test]
fn workspace_keys_move_to_v2_spellings() {
    let output = migrate(WORKSPACE_KEYS);
    insta::assert_snapshot!("workspace_keys", output);
    let config = parsed(&output);
    assert!(config.root);
    assert_eq!(config.workspace.run, poly_config::v2::RunMode::Never);
}

#[test]
fn per_file_ignores_is_renamed() {
    let output = migrate(PER_FILE_IGNORES);
    insta::assert_snapshot!("per_file_ignores", output);
    let config = parsed(&output);
    assert_eq!(config.per_file_ignores["tests/**"], ["F401"]);
}

#[test]
fn typos_enabled_is_written_when_v1_relied_on_the_default() {
    let output = migrate("[lint.python.ruff]\nselect = [\"E\"]\n");
    insta::assert_snapshot!("typos_default", output);
    let config = parsed(&output);
    assert_eq!(config.engines["typos"].options.get("enabled"), None);
    assert_eq!(config.engines["typos"].enabled, Some(true));
}

#[test]
fn typos_stays_off_when_v1_disabled_it() {
    let output = migrate("[lint.typos]\nenabled = false\n");
    insta::assert_snapshot!("typos_disabled", output);
    let config = parsed(&output);
    assert_eq!(config.engines["typos"].enabled, Some(false));
}

#[test]
fn a_language_level_typos_switch_does_not_disable_the_engine() {
    let output = migrate("[lint.markdown.typos]\nenabled = false\n");
    let config = parsed(&output);
    assert_eq!(config.engines["typos"].enabled, Some(true));
    assert_eq!(config.engines["typos"].lang["markdown"].enabled, Some(false));
}

#[test]
fn a_second_run_changes_nothing() {
    for source in [
        PER_LANGUAGE,
        CROSS_CUTTING,
        RULES_TABLE,
        WORKSPACE_KEYS,
        PER_FILE_IGNORES,
        include_str!("../../../../../poly.toml"),
    ] {
        let once = migrate(source);
        let mut doc: DocumentMut = once.parse().unwrap();
        let outcome = migrate_document(&mut doc, &options()).unwrap();
        assert!(!outcome.changed, "the migrated config still reads as v1");
        assert_eq!(doc.to_string(), once);
    }
}

#[test]
fn a_config_without_v1_markers_is_untouched() {
    let source = "# already v2\nrequired_version = \">=2.0.0\"\n\n[engines.ruff]\nselect = [\"E\"]\n";
    let mut doc: DocumentMut = source.parse().unwrap();
    let outcome = migrate_document(&mut doc, &options()).unwrap();
    assert!(!outcome.changed);
    assert_eq!(doc.to_string(), source);
}

#[test]
fn output_has_no_v1_markers() {
    for source in [
        PER_LANGUAGE,
        CROSS_CUTTING,
        RULES_TABLE,
        WORKSPACE_KEYS,
        PER_FILE_IGNORES,
    ] {
        let table: toml::Table = migrate(source).parse().unwrap();
        assert!(detect_v1(&table).is_empty());
    }
}

#[test]
fn a_comment_above_a_moved_key_survives() {
    let output = migrate(PER_LANGUAGE);
    let lines: Vec<&str> = output.lines().collect();
    let at = lines
        .iter()
        .position(|line| *line == "# the rule families we enforce")
        .expect("key comment is kept");
    assert!(lines[at + 1].starts_with("select"), "comment must stay on its key");
    assert!(output.contains("# Python lint settings"));
}

#[test]
fn comments_on_workspace_keys_survive() {
    let output = migrate(WORKSPACE_KEYS);
    assert!(output.contains("# stop the config cascade here\nroot = true"));
    assert!(output.contains("# the whole-project phase is slow in this repo\nrun = \"never\""));
}

#[test]
fn the_schema_comment_stays_first_and_points_at_v2() {
    let output = migrate(PER_LANGUAGE);
    assert_eq!(
        output.lines().next(),
        Some("#:schema https://goldziher.github.io/poly/poly-v2.schema.json")
    );
}

#[test]
fn required_version_is_added_once() {
    let output = migrate(PER_FILE_IGNORES);
    assert_eq!(output.matches("required_version").count(), 1);
    assert_eq!(parsed(&output).required_version.as_deref(), Some(">=2.0.0"));
    assert_eq!(
        V2Options::for_poly_toml().required_version.as_deref(),
        Some(concat!(">=", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn single_language_engines_hoist_and_multi_language_engines_never_widen() {
    let config = parsed(&migrate(PER_LANGUAGE));
    let ruff = &config.engines["ruff"];
    assert!(
        ruff.options.contains_key("line_length"),
        "equal in both phases: engine level"
    );
    assert!(
        ruff.lint.as_ref().unwrap().options.contains_key("select"),
        "lint-only: phase level"
    );
    assert!(ruff.rules.contains_key("ANN201"));
    let oxc = &config.engines["oxc"];
    assert!(oxc.options.is_empty() && oxc.lint.is_none() && oxc.format.is_none());
    for language in ["typescript", "javascript"] {
        let lang = &oxc.lang[language];
        assert!(lang.lint.as_ref().unwrap().options.contains_key("categories"));
        assert!(lang.format.as_ref().unwrap().options.contains_key("semi"));
    }
}

#[test]
fn a_value_set_for_every_served_language_is_hoisted() {
    let served = |engine: &str| {
        assert_eq!(engine, "oxc");
        (
            vec!["typescript".to_string(), "javascript".to_string()],
            vec!["typescript".to_string(), "javascript".to_string()],
        )
    };
    let source = "[lint.typescript.oxc]\ncategories = [\"correctness\"]\nmax_warnings = 0\n\n\
                  [lint.javascript.oxc]\ncategories = [\"correctness\"]\nmax_warnings = 10\n";
    let doc: DocumentMut = source.parse().unwrap();
    let mut notes = Vec::new();
    let collected = super::collect::collect(doc.as_table(), &mut notes).unwrap();
    let placed = super::place::place(&collected, false, &served, &mut notes).unwrap();
    let oxc = &placed["oxc"];
    assert!(oxc.phases[&super::collect::Phase::Lint].contains_key("categories"));
    assert!(!oxc.phases[&super::collect::Phase::Lint].contains_key("max_warnings"));
    assert!(oxc.langs["typescript"].phases[&super::collect::Phase::Lint].contains_key("max_warnings"));
}

#[test]
fn a_mixed_config_with_unplaceable_keys_is_an_error() {
    let mut doc: DocumentMut = "[lint]\nunknown = 1\n".parse().unwrap();
    assert!(migrate_document(&mut doc, &options()).is_err());
}

#[test]
fn local_override_is_rewritten_and_v1_lock_files_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("poly.toml"), "[lint.python.ruff]\nselect = [\"E\"]\n").unwrap();
    fs::write(
        dir.path().join("poly.local.toml"),
        "[lint.python.ruff]\nselect = [\"E\", \"F\"]\n",
    )
    .unwrap();
    fs::write(dir.path().join("poly-config.lock"), "").unwrap();
    let mut plan = build_plan(dir.path()).unwrap();
    apply_to_plan(&mut plan).unwrap();
    assert!(plan.has_v2_changes());
    let local = plan.local_v2.as_ref().expect("local override rewritten");
    let local_text = local.doc.to_string();
    assert!(!local_text.contains("required_version"));
    assert!(!local_text.contains("typos"));
    parsed(&local_text);
    let notes = &plan.v2.as_ref().unwrap().notes;
    assert!(notes.iter().any(|note| note.contains("poly lock")), "{notes:?}");
    assert_eq!(lock_file_notes(dir.path()).len(), 1);
}
