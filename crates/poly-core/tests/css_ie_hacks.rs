//! A stylesheet carrying IE `*property` hacks must finish, and must say it was
//! not checked.
//!
//! This is a regression test for a hang, not for a finding. `biome_css`'s
//! parser recovers from the hack in time exponential in the number of hacks
//! present: a synthetic file of ten took 0.46 s and twelve took 8.88 s. YUI's
//! `reset-fonts-grids.css` — vendored into Django's documentation theme, and
//! into thousands of other projects — carries 37, and `poly lint` on Django
//! grew to 27 GB and was OOM-killed after five minutes having reported nothing
//! at all. The whole run died, not just the file.
//!
//! The assertions below are therefore about termination and accounting. The
//! time budget is deliberately generous — enough that ordinary CI contention
//! cannot trip it, and far below the point where the exponential blowup is
//! survivable, so a regression fails the test instead of hanging the suite.

use std::time::{Duration, Instant};

use poly_core::{Config, RunOptions};

/// Well under the ~4x-per-hack growth curve: the pre-fix binary passed 8 s at
/// twelve hacks, and this fixture carries sixteen.
const BUDGET: Duration = Duration::from_secs(20);

/// The shape of the YUI reset stylesheet that triggered it, minified the same
/// way and carrying enough hacks to be firmly inside the exponential region.
fn ie_hack_stylesheet() -> String {
    let mut css = String::from("/* Copyright (c) 2008, Yahoo! Inc.; licensed *see the file */\n");
    css.push_str("html{color:#000;background:#FFF;}");
    for index in 0..16 {
        css.push_str(&format!(".col{index}{{float:left;*width:100px;*margin-left:0;}}"));
    }
    css
}

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

#[test]
fn a_stylesheet_of_ie_hacks_is_skipped_by_lint_instead_of_hanging_the_run() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("reset.css");
    std::fs::write(&path, ie_hack_stylesheet()).expect("write stylesheet");

    let started = Instant::now();
    let run = poly_core::lint_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .expect("lint run completes");
    let elapsed = started.elapsed();

    assert!(
        elapsed < BUDGET,
        "linting a stylesheet of IE hacks took {elapsed:?}; the parser blowup is back"
    );
    assert!(run.errors.is_empty(), "declining a file is not a failure to lint");
    assert!(
        run.results.iter().all(|result| result.diagnostics.is_empty()),
        "the CSS backend declined the file, so it has nothing to report"
    );

    // Accounting note, asserted so a future change to it is deliberate: the
    // runner consults `Engine::skip_reason` on the format path only, so a lint
    // backend declining a file leaves no skip record and the file still counts
    // as checked. That is the pre-existing contract every content-scanning lint
    // backend already follows (`yaml` on Go-templated documents); this fixture
    // pins termination, which is what was broken, not that gap.
    assert_eq!(run.checked, 1);
    assert!(run.skipped.is_empty());
}

#[test]
fn a_stylesheet_of_ie_hacks_is_skipped_by_format_instead_of_erroring() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("reset.css");
    let original = ie_hack_stylesheet();
    std::fs::write(&path, &original).expect("write stylesheet");

    let started = Instant::now();
    let run = poly_core::format_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        true,
        false,
    )
    .expect("format run completes");
    assert!(started.elapsed() < BUDGET);

    assert!(
        run.errors.is_empty(),
        "malva's syntax error on a legacy stylesheet must not fail `fmt --check`"
    );
    assert!(
        run.results.iter().all(|result| !result.changed),
        "a skipped file is not drift"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read fixture"),
        original,
        "the stylesheet is untouched"
    );
}

/// The guard must not swallow ordinary stylesheets: the universal selector is
/// valid CSS and appears in a large share of real files, so matching it would
/// trade a hang for silently unchecked stylesheets everywhere.
#[test]
fn an_ordinary_stylesheet_using_the_universal_selector_is_still_checked() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("site.css");
    std::fs::write(&path, "*{margin:0;padding:0}\n.a > *{color:red}\n").expect("write stylesheet");

    let run = poly_core::lint_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .expect("lint run completes");

    assert!(
        run.skipped.is_empty(),
        "the universal selector is not the IE hack: {:?}",
        run.skipped
    );
    assert_eq!(run.checked, 1, "an ordinary stylesheet must still be linted");
}
