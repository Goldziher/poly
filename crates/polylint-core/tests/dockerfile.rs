//! Insta snapshot fixture for the Dockerfile linter backend.
//!
//! A single known-bad Dockerfile exercises all eight rules implemented by
//! [`DockerfileEngine`], asserting exact (code, severity, line) tuples so any
//! regression in rule logic immediately breaks the snapshot.
//!
//! No known-unformatted fixture is needed: the engine is lint-only and returns
//! [`FormatOutput::Unchanged`] unconditionally.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::dockerfile::DockerfileEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

fn make_src(content: &str) -> SourceFile {
    SourceFile {
        path: "Dockerfile".into(),
        language: Language::Dockerfile,
        content: content.into(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture
//
// Every line is carefully chosen to trigger exactly one rule:
//
//   Line 1  FROM alpine          → DL3006 (no tag)
//   Line 2  FROM alpine:latest   → DL3007 (:latest)
//   Line 3  RUN apt-get …        → DL3009 (missing cache cleanup)
//                                   + wget/curl sighting for DL4001
//   Line 4  RUN echo hello       → DL3059 (consecutive RUN after line 3)
//   Line 5  MAINTAINER …         → DL4000 (deprecated)
//   Line 6  WORKDIR relative     → DL3000 (relative path)
//   Line 7  CMD echo hello       → DL3025 (shell form)
//   (end)                        → DL4001 (both wget+curl in RUN on line 3)
// ---------------------------------------------------------------------------

const KNOWN_BAD: &str = "\
FROM alpine
FROM alpine:latest
RUN apt-get install -y curl wget
RUN echo hello
MAINTAINER old@example.com
WORKDIR relative_dir
CMD echo hello
";

#[test]
fn known_bad_diagnostics() {
    let engine = DockerfileEngine;
    let src = make_src(KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected at least one diagnostic");

    // Summarise as (engine, code, severity, start_line) so the snapshot is
    // both human-readable and precisely bounded.
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.severity,
                d.span.map(|s| s.start_line),
            )
        })
        .collect();

    insta::assert_debug_snapshot!("known_bad_diagnostics", summary);
}

// ---------------------------------------------------------------------------
// Clean Dockerfile — zero diagnostics
// ---------------------------------------------------------------------------

const CLEAN: &str = "\
FROM alpine:3.20
RUN apk add --no-cache curl
WORKDIR /app
COPY . .
CMD [\"./my-app\"]
";

#[test]
fn clean_dockerfile_no_diagnostics() {
    let engine = DockerfileEngine;
    let src = make_src(CLEAN);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "expected no diagnostics for clean Dockerfile, got: {diags:#?}"
    );
}

// ---------------------------------------------------------------------------
// format() must always return Unchanged (lint-only engine)
// ---------------------------------------------------------------------------

#[test]
fn format_returns_unchanged() {
    let engine = DockerfileEngine;
    let src = make_src(KNOWN_BAD);
    let result = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "Dockerfile engine must return Unchanged from format()"
    );
}

// ---------------------------------------------------------------------------
// Individual rule smoke tests — each verifies one rule fires / does not fire
// ---------------------------------------------------------------------------

#[test]
fn dl3006_no_tag_fires() {
    let src = make_src("FROM alpine\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("DL3006")),
        "DL3006 must fire for FROM without tag"
    );
}

#[test]
fn dl3006_with_tag_ok() {
    let src = make_src("FROM alpine:3.20\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("DL3006")),
        "DL3006 must not fire when tag is present"
    );
}

#[test]
fn dl3007_latest_fires() {
    let src = make_src("FROM alpine:latest\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("DL3007")),
        "DL3007 must fire for :latest tag"
    );
}

#[test]
fn dl3009_apt_no_cleanup_fires() {
    let src = make_src("FROM alpine:3.20\nRUN apt-get install -y curl\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("DL3009")),
        "DL3009 must fire for apt-get without cache cleanup"
    );
}

#[test]
fn dl3009_apt_with_cleanup_ok() {
    let src = make_src("FROM alpine:3.20\nRUN apt-get install -y curl && rm -rf /var/lib/apt/lists/*\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("DL3009")),
        "DL3009 must not fire when cache is cleaned up"
    );
}

#[test]
fn dl3025_cmd_shell_form_fires() {
    let src = make_src("FROM alpine:3.20\nCMD echo hello\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("DL3025")),
        "DL3025 must fire for CMD in shell form"
    );
}

#[test]
fn dl3059_consecutive_run_fires() {
    let src = make_src("FROM alpine:3.20\nRUN echo a\nRUN echo b\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("DL3059")),
        "DL3059 must fire for consecutive RUN instructions"
    );
}

#[test]
fn dl4000_maintainer_fires() {
    let src = make_src("FROM alpine:3.20\nMAINTAINER dev@example.com\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("DL4000")),
        "DL4000 must fire for MAINTAINER"
    );
}

#[test]
fn dl4001_wget_and_curl_fires() {
    let src = make_src("FROM alpine:3.20\nRUN wget http://a.com && curl http://b.com\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("DL4001")),
        "DL4001 must fire when both wget and curl appear"
    );
}

#[test]
fn dl4001_only_wget_ok() {
    let src = make_src("FROM alpine:3.20\nRUN wget http://a.com\n");
    let diags = DockerfileEngine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("DL4001")),
        "DL4001 must not fire when only wget is used"
    );
}
