//! An unknown **top-level** table in `poly.toml` is reported.
//!
//! The third case in the report behind `unknown-config-key` and
//! `invalid-config-value`: those two catch a bad key *inside* a table poly
//! recognises, but an entire table poly has never heard of was still accepted in
//! silence. `RawPolyConfig` carries `#[serde(default)]` and no
//! `deny_unknown_fields`, so `[this_whole_table_is_nonsense]` deserializes to
//! nothing at all and the run proceeds as if the file had not mentioned it.
//!
//! That is the most complete way to have a config do nothing: a misspelled
//! `[discovry]` takes every exclusion in it out of the run, and the only visible
//! symptom is that poly checks more files than expected — which reads as poly
//! being wrong rather than the config being wrong.
//!
//! The keys that must NOT be reported are as interesting as the ones that must.
//! `extends` and `exclude_mode` never reach the typed schema — one is consumed
//! by the `extends` resolver and the other is a merge directive stripped before
//! deserialization — so a naive "is this a field of `RawPolyConfig`?" check
//! reports both, on configs that are entirely correct.

use poly_core::{Config, RunOptions};

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    }
}

/// Lint a `poly.toml` containing `body`, returning every finding's `(code, title)`.
fn findings(body: &str) -> Vec<(String, String)> {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("poly.toml");
    std::fs::write(&path, body).expect("write config");

    let run = poly_core::lint_run(&[path], &Config::default(), &options(), false, false).expect("lint run completes");
    assert!(
        run.errors.is_empty(),
        "linting a config file must not fail: {:?}",
        run.errors
    );

    run.results
        .iter()
        .flat_map(|result| &result.diagnostics)
        .map(|d| (d.code.clone().unwrap_or_default(), d.title.clone()))
        .collect()
}

#[test]
fn an_unknown_top_level_table_is_reported() {
    let found = findings("[this_whole_table_is_nonsense]\nfoo = \"bar\"\n");

    assert!(
        found
            .iter()
            .any(|(code, title)| code == "unknown-config-key" && title.contains("this_whole_table_is_nonsense")),
        "an entire unrecognised table must be reported, got {found:?}"
    );
}

/// The realistic case, and the one that actually costs coverage: a near-miss on
/// a real table name rather than obvious nonsense.
#[test]
fn a_misspelled_known_table_is_reported() {
    let found = findings("[discovry]\nexclude = [\"vendor/**\"]\n");

    assert!(
        found
            .iter()
            .any(|(code, title)| code == "unknown-config-key" && title.contains("discovry")),
        "a misspelled table name must be reported, got {found:?}"
    );
}

#[test]
fn every_recognised_top_level_table_is_left_alone() {
    // Written as one config so a key wrongly rejected shows up here rather than
    // in whichever single-table test happened to run first.
    let body = "\
[defaults]
line_length = 100

[discovery]
exclude = [\"vendor/**\"]

[lint.python.ruff]
select = [\"E\"]

[fmt.python.ruff]
line_length = 100

[commit]
[hooks]
[cache]
[tools]
[workspace]
[rules]

[per-file-ignores]
\"tests/**\" = [\"E501\"]
";
    let found = findings(body);
    assert!(
        found.is_empty(),
        "no recognised top-level table may be reported, got {found:?}"
    );
}

/// `extends` and `exclude_mode` are consumed before the typed schema ever sees
/// the table, so they are absent from `RawPolyConfig`'s fields. Reporting them
/// would fire on correct configs — including every config that uses ADR 0020
/// shared bases.
#[test]
fn keys_consumed_before_deserialization_are_not_reported() {
    let found = findings("extends = []\nexclude_mode = \"replace\"\n\n[discovery]\nexclude = [\"a/**\"]\n");

    assert!(
        found.is_empty(),
        "`extends` and `exclude_mode` are valid config, not unknown keys: {found:?}"
    );
}
