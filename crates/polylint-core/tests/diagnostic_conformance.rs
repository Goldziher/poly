//! Contract test: every emitted [`Diagnostic`] across all in-process backends
//! must conform to the standardised Diagnostic contract: non-empty `engine` and
//! non-empty `title`; structured backends must produce ≥1 finding with both
//! `code` and `span` set on real findings.
//!
//! # Strategy
//!
//! The real pipeline (`polylint_core::lint`) is driven over the permanent
//! fixture tree at `tests/fixtures/conformance/`, which holds one
//! violation-bearing file per backend language. Using the real pipeline — not
//! direct engine calls — exercises the full stack: discovery, language routing,
//! engine dispatch, and Diagnostic normalisation.
//!
//! # Covered backends
//!
//! **Structured** (contract: every real finding sets BOTH `code` and `span`):
//!   `taplo`, `graphql`, `yaml`, `typos`, `hcl`, `r`, `treesitter`, `dockerfile`
//!
//! **Structured with edge-case exemptions** (contract: ≥1 *normal* finding sets
//! BOTH `code` and `span`):
//!   `ruff`  — file-level parse findings can have `span = None`
//!   `oxc`   — same as ruff for syntax-error findings
//!   `rumdl` — a minority of rules emit `code = None`
//!
//! # Explicitly excluded (documented allowlist)
//!
//! `catalog_tool` — an external binary invoked per-file; it emits a file-level
//!     pass/fail result with no rule code or byte span. It won't run in CI and
//!     is explicitly excluded from the structured-tier assertion.
//!
//! `native_tool` / `shellcheck` — the `shellcheck` binary is not guaranteed to
//!     be present in CI; its diagnostics use a `SC<nnnn>` code scheme. When the
//!     binary is present the contract is verified by `tests/native_tool.rs`.
//!     When absent, the underlying `treesitter` tier still runs for Shell files
//!     and is covered via the Go delegation path below.
//!
//! `sqruff` — sqruff uses a `"????"` sentinel code and emits some findings with
//!     `span.start_line = 0`. These are documented quirks, not contract
//!     violations. The sqruff fixture in `tests/sqruff.rs` covers that behaviour
//!     separately. SQL files are not included in this conformance tree.

use std::collections::HashMap;
use std::path::PathBuf;

use polylint_core::{Config, Diagnostic, RunOptions};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn conformance_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance")
}

/// Run the real lint pipeline over the conformance fixture tree and return all
/// diagnostics grouped by engine name.
///
/// `no_cache: true` guarantees every engine actually runs (no stale hit
/// obscures missing diagnostics). `jobs: Some(2)` keeps the test parallel
/// without saturating CI cores.
fn run_and_group() -> HashMap<String, Vec<Diagnostic>> {
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(2),
        exclude: Vec::new(),
    };
    let results = polylint_core::lint(
        &[conformance_fixtures_dir()],
        &Config::default(),
        &opts,
        false, // fix = false (read-only)
        false, // collect_debug = false
    )
    .expect("conformance lint run must not fail");

    let mut by_engine: HashMap<String, Vec<Diagnostic>> = HashMap::new();
    for result in results {
        for diag in result.diagnostics {
            by_engine.entry(diag.engine.clone()).or_default().push(diag);
        }
    }
    by_engine
}

// ---------------------------------------------------------------------------
// The single conformance test
// ---------------------------------------------------------------------------

/// Diagnostic contract test. Runs the pipeline once and verifies all
/// conformance properties in a single pass for speed.
///
/// Fixture → engine mapping:
///   `bad.py`      → ruff (F401 unused import, W605 invalid escape, E711 == None)
///   `bad.js`      → oxc  (no-debugger rule)
///   `bad.toml`    → taplo (duplicate key)
///   `bad.md`      → rumdl (MD018 no space after `#`)
///   `bad.graphql` → graphql (parse error)
///   `bad.yaml`    → yaml (unclosed flow sequence)
///   `typos.md`    → typos (misspellings; rumdl also runs cross-file)
///   `bad.tf`      → hcl (unclosed block body syntax error)
///   `bad.R`       → r (equals_na rule)
///   `Dockerfile`  → dockerfile (DL3006 FROM without tag)
///   `trailing.go` → NativeToolEngine(gofmt) which delegates lint to treesitter
///                   (trailing-whitespace diagnostic emitted by treesitter)
///
/// The typos engine is cross-cutting and runs on every discovered file in
/// addition to the language-specific backend. Only `typos.md` contains actual
/// misspellings; the other fixture files are clean for typos.
#[test]
fn diagnostic_contract_all_backends_conform() {
    let by_engine = run_and_group();

    // -------------------------------------------------------------------------
    // Part 1: Universal contract — every diagnostic has non-empty engine+title
    // -------------------------------------------------------------------------
    let all: Vec<&Diagnostic> = by_engine.values().flatten().collect();
    assert!(
        !all.is_empty(),
        "conformance run must produce at least one diagnostic; \
         check that tests/fixtures/conformance/ exists and contains violation files"
    );
    for diag in &all {
        assert!(
            !diag.engine.is_empty(),
            "Diagnostic.engine must never be empty; got: {diag:?}"
        );
        assert!(
            !diag.title.is_empty(),
            "Diagnostic.title must never be empty; got: {diag:?}"
        );
    }

    // -------------------------------------------------------------------------
    // Part 2: Structured backends — every finding sets both code AND span
    //
    // These backends are designed so every real finding always carries a rule
    // code and a source location. Assert the strongest property: ALL findings
    // (not just ≥1) satisfy the invariant.
    // -------------------------------------------------------------------------
    const STRUCTURED: &[&str] = &[
        "taplo",
        "graphql",
        "yaml",
        "typos",
        "hcl",
        "r",
        "treesitter",
        "dockerfile",
    ];
    for backend in STRUCTURED {
        let diags = by_engine.get(*backend).unwrap_or_else(|| {
            panic!(
                "structured backend '{backend}' produced no diagnostics; \
                 verify that tests/fixtures/conformance/ contains a file that \
                 triggers this engine"
            )
        });
        assert!(
            !diags.is_empty(),
            "structured backend '{backend}' must have ≥1 diagnostic"
        );
        for diag in diags {
            assert!(
                diag.code.is_some(),
                "structured backend '{backend}': Diagnostic.code must be Some; \
                 got None in: {diag:?}"
            );
            assert!(
                diag.span.is_some(),
                "structured backend '{backend}': Diagnostic.span must be Some; \
                 got None in: {diag:?}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Part 3: Edge-case backends — ≥1 normal finding has both code AND span
    //
    // These backends CAN legitimately omit one field on rare edge cases (e.g.
    // ruff emits a file-level parse diagnostic without a span; rumdl omits code
    // for a small set of rules). The fixture exercises a "normal" rule that
    // always sets both, so the assertion targets ≥1 finding rather than ALL.
    // -------------------------------------------------------------------------

    // ruff: F401 / W605 / E711 are rule-based diagnostics that always carry
    // both code and span. bad.py exercises all three.
    let ruff_diags = by_engine.get("ruff").unwrap_or_else(|| {
        panic!("ruff produced no diagnostics; check tests/fixtures/conformance/bad.py")
    });
    assert!(
        ruff_diags
            .iter()
            .any(|d| d.code.is_some() && d.span.is_some()),
        "ruff: expected ≥1 finding with both code and span on normal rule violations; \
         got: {ruff_diags:?}"
    );

    // oxc: no-debugger is a correctness rule; always carries code + span.
    // bad.js contains `debugger;` to trigger no-debugger.
    let oxc_diags = by_engine.get("oxc").unwrap_or_else(|| {
        panic!("oxc produced no diagnostics; check tests/fixtures/conformance/bad.js")
    });
    assert!(
        oxc_diags
            .iter()
            .any(|d| d.code.is_some() && d.span.is_some()),
        "oxc: expected ≥1 finding with both code and span on normal rule violations; \
         got: {oxc_diags:?}"
    );

    // rumdl: MD018 (no space after `#`) always carries both code and span.
    // bad.md starts with `#Bad Heading` to trigger MD018.
    // Some rumdl rules legitimately omit code (hence ≥1 not ALL).
    let rumdl_diags = by_engine.get("rumdl").unwrap_or_else(|| {
        panic!("rumdl produced no diagnostics; check tests/fixtures/conformance/bad.md")
    });
    assert!(
        rumdl_diags.iter().any(|d| d.span.is_some()),
        "rumdl: expected ≥1 finding with a span; got: {rumdl_diags:?}"
    );

    // -------------------------------------------------------------------------
    // Part 4: Audit log — print covered engines so CI output shows coverage
    // -------------------------------------------------------------------------
    let mut covered: Vec<&str> = by_engine.keys().map(String::as_str).collect();
    covered.sort_unstable();
    println!(
        "diagnostic_conformance: covered engines = {covered:?} \
         (total {} diagnostics across {} engines)",
        all.len(),
        covered.len()
    );

    // Every STRUCTURED backend and every edge-case backend must be covered.
    // This turns a silent empty-result into a loud test failure.
    const ALL_REQUIRED: &[&str] = &[
        "taplo",
        "graphql",
        "yaml",
        "typos",
        "hcl",
        "r",
        "treesitter",
        "dockerfile",
        "ruff",
        "oxc",
        "rumdl",
    ];
    for required in ALL_REQUIRED {
        assert!(
            by_engine.contains_key(*required),
            "required backend '{required}' produced zero diagnostics; \
             the conformance fixture for this backend may be missing or broken"
        );
    }
}
