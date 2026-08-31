//! Real-code hardening harness.
//!
//! poly's own fixtures are small, hand-written and chosen to exercise a known
//! path. They cannot answer the questions that decide whether a release is safe
//! to ship: does poly error on anything in a large third-party tree, does
//! formatting converge, does the cache ever serve a wrong answer, and how many
//! findings does a rule actually produce on code nobody wrote for us.
//!
//! Driven by `scripts/harden.sh`, one process per root, which is what keeps a
//! panic in one repository from taking the rest of the run with it:
//!
//! ```sh
//! POLY_HARDEN_ROOT_PATH=/tmp/poly-harden/b/django \
//! POLY_HARDEN_ROOT_NAME=django \
//! POLY_HARDEN_CORPUS=b \
//! POLY_HARDEN_RESULTS=/tmp/poly-harden/results.ndjson \
//! cargo test -p poly-cli --test harden -- --ignored --nocapture --exact harden_root
//! ```
//!
//! The test is `#[ignore]`d rather than gated behind a feature so that a plain
//! `cargo test --workspace` still **compiles** it on every platform in CI. A
//! harness that only builds on the nightly schedule rots between runs, and the
//! run that discovers it is the one you needed to trust.
//!
//! Its record is written **before** the assertions run, so a failing root still
//! leaves its measurement on disk for whoever reads it next.

mod assertions;
mod corpus;
mod drive;
mod record;

use std::io::Write as _;

use record::{Corpus, RootRecord};

/// Wall-clock ceilings, set well above the measured time so they catch an
/// order-of-magnitude regression rather than runner jitter. Zero disables the
/// check, which is the right default for a root nobody has measured yet.
fn ceiling_ms(root: &str) -> u128 {
    match root {
        "typescript" => 900_000,
        "django" | "react" => 300_000,
        _ => 0,
    }
}

#[test]
#[ignore = "real-code hardening harness; invoke via scripts/harden.sh"]
fn harden_root() {
    let Some(root) = corpus::from_env() else {
        panic!("POLY_HARDEN_ROOT_PATH is unset; this test is driven by scripts/harden.sh");
    };
    assert!(
        root.path.is_dir(),
        "root {} does not exist; the orchestrator should have acquired it",
        root.path.display()
    );

    // Keep the harness's cache out of the developer's own *and* out of the tree
    // being measured, and make the cold run genuinely cold.
    let cache_home = drive::cache_home(&root.name);
    // SAFETY-equivalent note: this is a single-threaded test process whose whole
    // job is this one measurement, so there is no other thread to race with.
    unsafe { std::env::set_var("POLY_CACHE_HOME", &cache_home) };
    let _ = std::fs::remove_dir_all(&cache_home);

    let ceiling = ceiling_ms(&root.name);
    let (lint_run, lint_phase) = drive::lint_phase(&root.path, ceiling);
    let cache = drive::cache_behaviour(&root.path, &lint_run, lint_phase.elapsed_ms);

    // Formatting mutates, so it never touches the source tree — decisive for the
    // local corpus, which is the developer's own working trees.
    let scratch = tempfile::tempdir().expect("scratch directory");
    let copy = scratch.path().join("tree");
    corpus::disposable_copy(&root.path, &copy).expect("copy the tree to format against");
    let (fmt_phase, idempotence) = drive::idempotence(&copy, ceiling);

    let mut rules = Vec::new();
    for engine in ["astgrep", "quality"] {
        let yields = drive::rule_yield(&root.path, engine);
        rules.extend(drive::rule_records(root.corpus, &root.name, engine, yields));
    }

    let mut record = RootRecord {
        schema: RootRecord::SCHEMA,
        corpus: root.corpus,
        root: root.name.clone(),
        path: root.path.display().to_string(),
        commit: root.commit.clone(),
        dirty: root.dirty,
        dirty_after: corpus::is_dirty(&root.path),
        poly_version: env!("CARGO_PKG_VERSION").to_string(),
        native_tools_enabled: std::env::var("POLY_HARDEN_NATIVE_TOOLS").is_ok(),
        lint: lint_phase,
        fmt: fmt_phase,
        idempotence,
        cache,
        rules,
        failures: Vec::new(),
    };
    record.failures = assertions::check(&record);

    // Written before the verdict, so a failing root is still diagnosable.
    append_record(&record);

    if root.corpus == Corpus::A && root.dirty {
        eprintln!(
            "note: {} has uncommitted changes; its counts are not comparable",
            root.name
        );
    }

    assert!(
        record.failures.is_empty(),
        "{} failed {} check(s):\n  - {}",
        root.name,
        record.failures.len(),
        record.failures.join("\n  - ")
    );
    eprintln!(
        "{}: {} checked, {} skipped, {} rule row(s)",
        root.name,
        record.lint.checked,
        record.lint.skipped,
        record.rules.len()
    );
}

/// Append one NDJSON line to the results file named by the orchestrator.
///
/// Best-effort: a harness that cannot write its log must still report its
/// verdict, or a full disk turns a real failure into a confusing one.
fn append_record(record: &RootRecord) {
    let Ok(path) = std::env::var("POLY_HARDEN_RESULTS") else {
        return;
    };
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}
