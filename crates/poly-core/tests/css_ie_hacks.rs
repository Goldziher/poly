//! A stylesheet carrying IE `*property` hacks must finish, and must not fail `fmt --check`.
//!
//! The lint half of this regression (biome's CSS analyzer declining such a file) lives in
//! `poly-engines-biome`'s `tests/ie_hacks.rs`. `biome_css`'s
//! parser recovers from the hack in time exponential in the number of hacks
//! present: a synthetic file of ten took 0.46 s and twelve took 8.88 s. YUI's
//! `reset-fonts-grids.css` — vendored into Django's documentation theme, and
//! into thousands of other projects — carries 37, and `poly lint` on Django
//! grew to 27 GB and was OOM-killed after five minutes having reported nothing
//! at all. The whole run died, not just the file.
//!
//! The assertions below are about termination and accounting. The
//! time budget is deliberately generous — enough that ordinary CI contention
//! cannot trip it, and far below the point where the exponential blowup is
//! survivable, so a regression fails the test instead of hanging the suite.
#![cfg(feature = "all-engines")]

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
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    }
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
