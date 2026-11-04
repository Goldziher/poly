//! A stylesheet carrying IE `*property` hacks must finish, and must be declined.
//!
//! This is a regression test for a hang, not for a finding. `biome_css`'s
//! parser recovers from the hack in time exponential in the number of hacks
//! present: a synthetic file of ten took 0.46 s and twelve took 8.88 s. YUI's
//! `reset-fonts-grids.css` — vendored into Django's documentation theme, and
//! into thousands of other projects — carries 37, and `poly lint` on Django
//! grew to 27 GB and was OOM-killed after five minutes having reported nothing
//! at all. The whole run died, not just the file.
//!
//! The time budget is deliberately generous — enough that ordinary CI contention
//! cannot trip it, and far below the point where the exponential blowup is
//! survivable, so a regression fails the test instead of hanging the suite.

use std::time::{Duration, Instant};

use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::css_hacks::IE_PROPERTY_HACK_SKIP;
use poly_engine_api::engine::{Engine, SourceFile};
use poly_engine_api::language::Language;
use poly_engines_biome::BiomeCssEngine;

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

fn cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn css(content: String) -> SourceFile {
    SourceFile {
        path: "reset.css".into(),
        language: Language::Css,
        content: content.as_str().into(),
    }
}

#[test]
fn a_stylesheet_of_ie_hacks_is_declined_instead_of_hanging_the_run() {
    let src = css(ie_hack_stylesheet());

    let started = Instant::now();
    let diagnostics = BiomeCssEngine.lint(&src, &cfg()).expect("lint completes");
    let elapsed = started.elapsed();

    assert!(
        elapsed < BUDGET,
        "linting a stylesheet of IE hacks took {elapsed:?}; the parser blowup is back"
    );
    assert!(
        diagnostics.is_empty(),
        "the CSS backend declined the file, so it has nothing to report"
    );
    assert_eq!(
        BiomeCssEngine.skip_reason(&src).as_deref(),
        Some(IE_PROPERTY_HACK_SKIP),
        "the format path must be told the file is skipped"
    );
}

/// The guard must not swallow ordinary stylesheets: the universal selector is
/// valid CSS and appears in a large share of real files, so matching it would
/// trade a hang for silently unchecked stylesheets everywhere.
#[test]
fn an_ordinary_stylesheet_using_the_universal_selector_is_still_checked() {
    let src = css("*{margin:0;padding:0}\n.a > *{color:red}\n".to_owned());
    assert_eq!(
        BiomeCssEngine.skip_reason(&src),
        None,
        "the universal selector is not the IE hack"
    );

    let bad = css("*{colr:red}\n".to_owned());
    let diagnostics = BiomeCssEngine.lint(&bad, &cfg()).expect("lint completes");
    assert!(
        !diagnostics.is_empty(),
        "an ordinary stylesheet must still be linted, not declined"
    );
}
