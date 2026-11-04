//! Contract test: every [`Diagnostic`] the web family emits through the real pipeline conforms to the
//! standardised Diagnostic contract: non-empty `engine` and `title`, and structured backends set both
//! `code` and `span` on real findings.
//!
//! `poly_core::lint` is driven over `tests/fixtures/conformance/`, which holds one violation-bearing
//! file per backend:
//!
//!   `bad.toml` -> taplo (duplicate key), `bad.yaml` -> yaml (unclosed flow sequence)  [structured]
//!   `bad.js`   -> oxc (no-debugger rule)                                              [edge-case]
//!   `bad.md`   -> rumdl (MD018 no space after `#`)                                    [edge-case]
//!
//! oxc and rumdl are exempt from "every finding" because syntax-error findings can have `span = None`
//! and a minority of rumdl rules emit `code = None`; the contract is that at least one normal finding
//! carries both. The root workspace's `tests/diagnostic_conformance.rs` covers the remaining backends.

use std::collections::HashMap;
use std::path::PathBuf;

use poly_core::{Config, Diagnostic, RunOptions};

mod common;

fn conformance_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance")
}

/// Run the real lint pipeline over the conformance fixture tree and return all diagnostics grouped by
/// engine name. `no_cache: true` guarantees every engine actually runs.
fn run_and_group() -> HashMap<String, Vec<Diagnostic>> {
    common::register();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(2),
        explicit_config: true,
        ..RunOptions::default()
    };
    let results = poly_core::lint(&[conformance_fixtures_dir()], &Config::default(), &opts, false, false)
        .expect("conformance lint run must not fail");

    let mut by_engine: HashMap<String, Vec<Diagnostic>> = HashMap::new();
    for result in results {
        for diag in result.diagnostics {
            by_engine.entry(diag.engine.clone()).or_default().push(diag);
        }
    }
    by_engine
}

#[test]
fn diagnostic_contract_web_backends_conform() {
    let by_engine = run_and_group();

    for diag in by_engine.values().flatten() {
        assert!(
            !diag.engine.is_empty(),
            "Diagnostic.engine must never be empty; got: {diag:?}"
        );
        assert!(
            !diag.title.is_empty(),
            "Diagnostic.title must never be empty; got: {diag:?}"
        );
    }

    for backend in ["taplo", "yaml"] {
        let diags = by_engine
            .get(backend)
            .unwrap_or_else(|| panic!("structured backend '{backend}' produced no diagnostics"));
        assert!(
            !diags.is_empty(),
            "structured backend '{backend}' must have a diagnostic"
        );
        for diag in diags {
            assert!(
                diag.code.is_some(),
                "structured backend '{backend}': Diagnostic.code must be Some; got None in: {diag:?}"
            );
            assert!(
                diag.span.is_some(),
                "structured backend '{backend}': Diagnostic.span must be Some; got None in: {diag:?}"
            );
        }
    }

    let oxc_diags = by_engine
        .get("oxc")
        .unwrap_or_else(|| panic!("oxc produced no diagnostics; check tests/fixtures/conformance/bad.js"));
    assert!(
        oxc_diags.iter().any(|d| d.code.is_some() && d.span.is_some()),
        "oxc: expected a finding with both code and span on normal rule violations; got: {oxc_diags:?}"
    );

    let rumdl_diags = by_engine
        .get("rumdl")
        .unwrap_or_else(|| panic!("rumdl produced no diagnostics; check tests/fixtures/conformance/bad.md"));
    assert!(
        rumdl_diags.iter().any(|d| d.span.is_some()),
        "rumdl: expected a finding with a span; got: {rumdl_diags:?}"
    );
}
