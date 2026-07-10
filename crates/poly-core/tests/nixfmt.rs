//! Insta snapshot fixtures for the nixpkgs-fmt Nix backend.
//!
//! - `known_unformatted_snapshot` — a known-unformatted Nix expression asserts
//!   the exact formatted output produced by nixpkgs-fmt.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::nixfmt::NixFmtEngine,
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
        path: "known_unformatted.nix".into(),
        language: Language::Nix,
        content: content.into(),
    }
}

/// Known-unformatted Nix: dense attribute set, missing spaces around `=`,
/// multiple attrs crammed onto one line — nixpkgs-fmt should expand and align.
const KNOWN_UNFORMATTED: &str = "\
{pkgs ? import <nixpkgs> {}}: pkgs.mkShell {buildInputs=[pkgs.git pkgs.curl];shellHook=\"echo hello\";}\
";

#[test]
fn known_unformatted_snapshot() {
    let engine = NixFmtEngine;
    let src = make_src(KNOWN_UNFORMATTED);
    let result = engine.format(&src, &engine_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => KNOWN_UNFORMATTED.to_string(),
    };

    insta::assert_snapshot!("nixfmt_known_unformatted_output", formatted);
}
