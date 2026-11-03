//! Tests for static config validation. They name no linked engine: the verdicts come from the
//! compiled component index, so they hold in a core-only build and with nothing installed.

use super::*;

fn messages(report: &ValidationReport) -> Vec<String> {
    report
        .problems
        .iter()
        .map(|p| format!("{}: {}", p.path, p.message))
        .collect()
}

#[test]
fn valid_config_with_nothing_installed_passes() {
    let report = validate_source(
        r#"
[engines.mago.lint]
php_version = "8.3"

[engines.ruff]
select = ["E"]
"#,
    );
    assert!(report.is_valid(), "{:?}", messages(&report));
    assert!(report.partial.is_empty(), "{:?}", report.partial);
}

#[test]
fn empty_config_is_valid() {
    assert!(validate_source("").is_valid());
}

#[test]
fn typo_in_mago_lint_table_is_caught_without_the_php_component() {
    let report = validate_source(
        r#"
[engines.mago.lint]
php_verison = "8.3"
"#,
    );
    let found = messages(&report);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("engines.mago.lint: unknown option `php_verison`"),
        "{found:?}"
    );
    assert!(found[0].contains("Did you mean `php_version`?"), "{found:?}");
}

#[cfg(not(feature = "engine-php"))]
#[test]
fn mago_is_not_linked_in_a_core_only_build() {
    let linked = linked_engines();
    assert!(linked.iter().all(|(name, _)| name != "mago"));
}

#[test]
fn typo_in_a_flat_engine_table_is_caught() {
    let report = validate_source("[engines.ruff]\nselct = [\"E\"]\n");
    let found = messages(&report);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("unknown option `selct`") && found[0].contains("`select`"),
        "{found:?}"
    );
}

#[test]
fn wrong_value_type_is_reported() {
    let report = validate_source("[engines.mago.lint]\nphp_version = 8\n");
    let found = messages(&report);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("invalid value for `php_version`"), "{found:?}");
}

#[cfg(not(feature = "engine-php"))]
#[test]
fn derived_keys_are_accepted_and_reported_as_partial() {
    let report = validate_source("[engines.mago]\nsome_formatter_key = true\n");
    assert!(report.is_valid(), "{:?}", messages(&report));
    assert_eq!(
        report.partial,
        vec![PartialValidation {
            component: "php".to_string(),
            engines: vec!["mago".to_string()],
        }]
    );
}

#[cfg(feature = "engine-php")]
#[test]
fn a_linked_engine_checks_its_derived_keys_in_process() {
    let ok = validate_source("[engines.mago.format]\nprint-width = 100\nsort-uses = true\n");
    assert!(ok.is_valid(), "{:?}", messages(&ok));
    assert!(ok.partial.is_empty(), "{:?}", ok.partial);

    let typo = validate_source("[engines.mago.format]\nsort-use = true\n");
    let found = messages(&typo);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("unknown option `sort-use`"), "{found:?}");
}

#[test]
fn engine_of_a_disabled_component_is_an_error() {
    let report = validate_source("[components]\ndisable = [\"php\"]\n\n[engines.mago]\nenabled = true\n");
    let found = messages(&report);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("engines.mago:") && found[0].contains("`php` is disabled"),
        "{found:?}"
    );
}

#[test]
fn a_disabled_preset_disables_its_components() {
    let report = validate_source("[components]\ndisable = [\"all\"]\n\n[engines.ruff]\nenabled = true\n");
    assert_eq!(report.problems.len(), 1, "{:?}", messages(&report));
}

#[test]
fn disabling_one_component_leaves_the_others_usable() {
    let report = validate_source("[components]\ndisable = [\"php\"]\n\n[engines.ruff]\nenabled = true\n");
    assert!(report.is_valid(), "{:?}", messages(&report));
}

#[test]
fn unknown_component_names_are_reported() {
    let report = validate_source("[components]\nenable = [\"pythn\"]\ndisable = [\"nope\"]\n");
    let paths: Vec<&str> = report.problems.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(paths, ["components.enable", "components.disable"]);
}

#[test]
fn astgrep_rule_dirs_are_accepted() {
    let report = validate_source("[engines.astgrep]\nrule_dirs = [\".poly/rules\"]\nbuiltin_pack = false\n");
    assert!(report.is_valid(), "{:?}", messages(&report));
}

#[test]
fn near_miss_engine_name_is_reported() {
    let report = validate_source("[engines.magoo]\nenabled = true\n");
    let found = messages(&report);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("did you mean `mago`"), "{found:?}");
}

#[test]
fn v1_table_of_an_unlinked_engine_is_checked_through_the_index() {
    let report = validate_source("[lint.php.mago]\nphp_verison = \"8.3\"\n");
    let found = messages(&report);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("unknown option `php_verison`"), "{found:?}");
}

#[test]
fn invalid_toml_and_unknown_top_level_keys_are_reported() {
    assert_eq!(validate_source("[engines").problems.len(), 1);
    assert_eq!(validate_source("bogus = 1\n").problems.len(), 1);
}
