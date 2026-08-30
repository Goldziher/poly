//! A panic inside a wrapped upstream parser must not abort the run.
//!
//! poly compiles ~20 third-party parsers in-process. Their public APIs return
//! `Result`, but their internals assert: `biome_parser` panics on a GraphQL
//! fragment description (a real language feature, graphql-js #4482), and
//! `FluffConfig::from_source` panics on a malformed document. Any one of those
//! reaching a rayon worker takes the **whole process** down, so a repository
//! containing a single such file gets no findings at all — not a partial
//! report, not an exit code that means "some files failed", but nothing.
//!
//! Discovered on prettier's test corpus: `poly lint` produced a **0-byte**
//! report for the entire repository and exited 101, because of one three-line
//! `.graphql` file.
//!
//! `Engine::lint`/`format` already have a per-file error outcome and poly
//! already exits 2 for "the run verified less than it claims". These tests
//! pin that a parser panic lands there instead of killing the run.

use poly_core::{Config, RunOptions};

/// A GraphQL fragment description. Valid GraphQL; `biome_graphql` asserts on it.
const PANICKING_GRAPHQL: &str = "\"Fragment description\" fragment Foo on Bar { baz }\n";

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        explicit_config: true,
        ..RunOptions::default()
    }
}

#[test]
fn a_parser_panic_becomes_a_per_file_error_not_a_dead_run() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let bad = directory.path().join("fragment.graphql");
    std::fs::write(&bad, PANICKING_GRAPHQL).expect("write panicking fixture");

    let run = poly_core::lint_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .expect("a parser panic must not abort the run");

    // `LintRun::errors` is the channel for "poly failed on a file it took on",
    // as opposed to `skipped`, which is poly declining a file it does not
    // handle. A backend that panicked took the file on and did not check it.
    let failure = run
        .errors
        .iter()
        .find(|e| e.path == bad)
        .expect("the panicking file must be reported as a failure, not silently dropped");
    assert!(
        failure.message.contains("panicked"),
        "the failure should say the backend panicked so it can be reported upstream, got {:?}",
        failure.message
    );
    assert!(
        failure.message.contains("biome"),
        "the failure should name the backend that panicked, got {:?}",
        failure.message
    );
}

/// The failure that actually costs a user: everything *else* in the repository
/// silently disappearing. Asserted with a file that has a real, known finding,
/// so a run that returned an empty report cannot pass this.
#[test]
fn files_beside_a_panicking_one_are_still_linted() {
    let directory = tempfile::tempdir().expect("temporary repository");
    std::fs::write(directory.path().join("fragment.graphql"), PANICKING_GRAPHQL).expect("write fixture");
    let good = directory.path().join("good.py");
    std::fs::write(&good, "import os  # noqa\n").expect("write clean fixture");

    let run = poly_core::lint_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .expect("a parser panic must not abort the run");

    let result = run
        .results
        .iter()
        .find(|r| r.path == good)
        .expect("a file next to the panicking one must still be linted");
    assert!(
        !result.diagnostics.is_empty(),
        "the neighbouring file's findings were lost: {result:?}"
    );
}
