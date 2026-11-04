//! A stylesheet carrying IE `*property` hacks must finish formatting, and must not be reported as an
//! error or as drift: malva's syntax error on a legacy stylesheet is declined, not failed.
//!
//! The lint side of the same regression (the biome parser's exponential recovery) lives in
//! `crates/poly-core/tests/css_ie_hacks.rs`. The time budget is deliberately generous.

use std::time::{Duration, Instant};

use poly_core::{Config, RunOptions};

mod common;

/// Well under the ~4x-per-hack growth curve: the pre-fix binary passed 8 s at twelve hacks, and this
/// fixture carries sixteen.
const BUDGET: Duration = Duration::from_secs(20);

/// The shape of the YUI reset stylesheet that triggered it, minified the same way and carrying enough
/// hacks to be firmly inside the exponential region.
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
        explicit_config: true,
        ..RunOptions::default()
    }
}

#[test]
fn a_stylesheet_of_ie_hacks_is_skipped_by_format_instead_of_erroring() {
    common::register();
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
