//! Engine-identity contract: an engine's [`Engine::name`] is the tool it wraps, not its Rust type.
//!
//! The biome pair, which shares the name `"biome"`, is covered by `poly-engines-biome`'s
//! `tests/identity.rs`; the Nix backend is pinned here.
#![cfg(all(feature = "all-engines", feature = "engine-nix"))]

use std::fs;

use poly_core::config::{Config, Kind};
use poly_core::engine::Engine;
use poly_core::engines::nixfmt::NixFmtEngine;
use poly_core::language::Language;

/// The Nix backend is named for the formatter it actually wraps (`alejandra`),
/// not for its Rust type (`NixFmtEngine`). `nixfmt` is a *different* formatter
/// with different output, so calling this engine `"nixfmt"` would misreport
/// which tool produced the result.
///
/// The name is **not** what avoids a catalog collision, in either direction: the
/// catalog ships an `alejandra` tool for `nix` just as it ships a `nixfmt` one,
/// so this engine shares a name with a catalog tool serving its own language
/// whatever it is called. What actually keeps them out of one plan is
/// `runner::plan`: `has_tier_one_formatter` yields no catalog formatter to a
/// language that already has a registry formatter, and both catalog entries are
/// formatter-only (`categories = ["formatter"]`, no lint command), so neither
/// yields a lint engine either. Were that to change, `plan_engines` resolves the
/// collision by keeping one engine per name — see
/// `runner::plan::tests::planned_engine_names_are_unique_per_language_and_kind`.
#[test]
fn nix_backend_is_named_for_the_formatter_it_wraps() {
    assert_eq!(NixFmtEngine.name(), "alejandra");
    assert!(NixFmtEngine.languages().contains(&Language::Nix));
}

/// The claim above, asserted rather than argued: the catalog ships a `nix` tool
/// under *both* names, so the engine's name cannot be what avoids the collision;
/// and both are formatter-only, so neither can produce a lint engine to meet the
/// `alejandra` backend in a `Kind::Lint` plan. If a lint command is ever added to
/// either, the collision becomes real and this test says so.
#[test]
fn the_nix_catalog_tools_are_formatter_only() {
    let catalog = poly_catalog::Catalog::get();
    for name in ["alejandra", "nixfmt"] {
        let tool = catalog
            .tool(name)
            .unwrap_or_else(|| panic!("the catalog is expected to ship a `{name}` tool"));
        assert!(
            tool.languages.iter().any(|language| language == "nix"),
            "`{name}` is expected to serve nix, which is what makes it a collision candidate",
        );
        assert!(
            tool.lint_command().is_none(),
            "`{name}` gained a lint command, so a catalog lint engine can now meet the \
             `alejandra` backend in one Nix plan under a shared name",
        );
    }
}

/// The name is the config key: `[fmt.nix.alejandra]` reaches the Nix backend.
#[test]
fn nix_backend_config_key_matches_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, "[fmt.nix.alejandra]\nindent_width = 3\n").unwrap();
    let config = Config::load_file(&path).expect("load");

    let cfg = config.engine_config(&Language::Nix, NixFmtEngine.name(), Kind::Format);
    assert_eq!(cfg.indent_width, 3, "[fmt.nix.alejandra] must reach the nix backend");
}
