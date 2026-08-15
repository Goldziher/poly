//! Engine-identity contract: what [`Engine::name`] is, and what keeps two
//! backends that answer the *same* name from colliding.
//!
//! `BiomeCssEngine` and `BiomeGraphqlEngine` both answer `"biome"`, because the
//! name is the **tool id** a user types in `poly.toml` (`[lint.css.biome]`,
//! `[lint.graphql.biome]`) and the `engine` field of a reported diagnostic — not
//! a unique id for the Rust type. Every surface that consumes the name is scoped
//! by language on top of it, so the shared name is safe. These tests pin the
//! properties that make it safe rather than leaving them incidental:
//!
//! - the two engines never serve the same language, so they never appear in one
//!   file's engine plan;
//! - their `version()` strings differ, so even a hypothetical shared plan yields
//!   distinct cache keys;
//! - `[lint.css.biome]` configures CSS only — it cannot reach the GraphQL engine.

use std::fs;

use poly_cache::{Namespace, ResultCache};
use poly_core::config::{Config, EngineConfig, GlobalDefaults, Kind};
use poly_core::engine::{Engine, SourceFile};
use poly_core::engines::biome_css::BiomeCssEngine;
use poly_core::engines::biome_graphql::BiomeGraphqlEngine;
use poly_core::engines::nixfmt::NixFmtEngine;
use poly_core::language::Language;

/// CSS whose `colr` property fires `lint/correctness/noUnknownProperty`.
const BAD_CSS: &str = "a { colr: blue; }\n";

/// An anonymous operation, which fires `lint/correctness/useGraphqlNamedOperations`.
const BAD_GRAPHQL: &str = "query { user { id } }\n";

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language,
        content: content.into(),
    }
}

/// The premise of the shared name: both engines answer `"biome"`.
#[test]
fn both_biome_engines_answer_the_same_name() {
    assert_eq!(BiomeCssEngine.name(), "biome");
    assert_eq!(BiomeGraphqlEngine.name(), "biome");
}

/// The two engines' language sets are disjoint, so `registry::engines_for` can
/// never place both in one file's plan — the precondition for every other
/// name-keyed surface (config table, cache id, severity remap) being unambiguous.
#[test]
fn biome_engines_never_serve_the_same_language() {
    for css_language in BiomeCssEngine.languages() {
        assert!(
            !BiomeGraphqlEngine.languages().contains(css_language),
            "BiomeCssEngine and BiomeGraphqlEngine both claim {css_language:?} while sharing the \
             name \"biome\"; give them distinct names (and bump both version() strings) or keep \
             their languages disjoint",
        );
    }
}

/// Belt and braces: even if the two engines *did* meet in one plan, the cache key
/// folds in `version()` alongside the name, and their versions differ — so the
/// key still distinguishes them. This is asserted, not assumed, because it is the
/// only thing standing between the shared name and a cross-language cache hit.
#[test]
fn shared_biome_name_still_yields_distinct_cache_keys() {
    assert_ne!(
        BiomeCssEngine.version(),
        BiomeGraphqlEngine.version(),
        "the two `biome` engines share a name, so version() is what keeps their cache keys apart",
    );

    let args = toml::Table::new();
    let digest = ResultCache::single_file_digest_with_path("fixture.txt", "same bytes\n");
    let css_key = ResultCache::key(
        Namespace::Lint,
        BiomeCssEngine.name(),
        BiomeCssEngine.version(),
        &args,
        &digest,
    );
    let graphql_key = ResultCache::key(
        Namespace::Lint,
        BiomeGraphqlEngine.name(),
        BiomeGraphqlEngine.version(),
        &args,
        &digest,
    );
    assert_ne!(
        css_key, graphql_key,
        "identical file bytes + identical engine name must still produce different cache keys",
    );
}

/// Behavioral proof that the shared name does not collapse configuration:
/// `[lint.css.biome]` silences the CSS engine and leaves the GraphQL engine —
/// which answers the same name — fully armed. A user who wants CSS linting
/// relaxed but GraphQL linting strict gets exactly that.
#[test]
fn biome_config_is_scoped_to_its_language() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, "[lint.css.biome]\nignore = [\"correctness\"]\n").unwrap();
    let config = Config::load_file(&path).expect("load");

    let css_cfg = config.engine_config(&Language::Css, "biome", Kind::Lint);
    let graphql_cfg = config.engine_config(&Language::GraphQl, "biome", Kind::Lint);
    assert!(
        graphql_cfg.options.is_empty(),
        "[lint.css.biome] must not leak into the graphql engine config, got {:?}",
        graphql_cfg.options,
    );

    let css_diags = BiomeCssEngine
        .lint(&src("fixture.css", Language::Css, BAD_CSS), &css_cfg)
        .expect("css lint");
    assert!(
        css_diags.is_empty(),
        "[lint.css.biome] ignore must silence the css engine, got {css_diags:#?}",
    );

    let graphql_diags = BiomeGraphqlEngine
        .lint(&src("fixture.graphql", Language::GraphQl, BAD_GRAPHQL), &graphql_cfg)
        .expect("graphql lint");
    assert!(
        !graphql_diags.is_empty(),
        "the graphql engine must still lint under a css-only biome config",
    );

    // The assertion above has teeth only if the CSS table *would* have silenced
    // the GraphQL engine had the shared name let it through: feed it that table
    // directly and watch the diagnostics disappear.
    let leaked = BiomeGraphqlEngine
        .lint(&src("fixture.graphql", Language::GraphQl, BAD_GRAPHQL), &css_cfg)
        .expect("graphql lint");
    assert!(
        leaked.is_empty(),
        "sanity: the css `ignore` table does suppress graphql rules when applied, so the test \
         above genuinely proves the two tables stayed separate",
    );
}

/// Both engines stamp their diagnostics with the same `engine` field, which is
/// the point: it names the tool the user configures, and the file path plus the
/// rule code say which analyzer produced it.
#[test]
fn biome_diagnostics_are_attributed_to_the_configured_tool_name() {
    let css_diags = BiomeCssEngine
        .lint(&src("fixture.css", Language::Css, BAD_CSS), &engine_cfg())
        .expect("css lint");
    let graphql_diags = BiomeGraphqlEngine
        .lint(&src("fixture.graphql", Language::GraphQl, BAD_GRAPHQL), &engine_cfg())
        .expect("graphql lint");

    assert!(!css_diags.is_empty() && !graphql_diags.is_empty());
    for diag in css_diags.iter().chain(&graphql_diags) {
        assert_eq!(
            diag.engine, "biome",
            "the reported engine must match the `[lint.<lang>.biome]` config key",
        );
    }
}

/// The Nix backend is named for the formatter it actually wraps (`alejandra`),
/// not for its Rust type (`NixFmtEngine`). `nixfmt` is a *different* formatter
/// with different output — and a separate catalog tool of that name — so naming
/// this engine `"nixfmt"` would both misreport the tool and collide with that
/// catalog tool on `Language::Nix`.
#[test]
fn nix_backend_is_named_for_the_formatter_it_wraps() {
    assert_eq!(NixFmtEngine.name(), "alejandra");
    assert!(NixFmtEngine.languages().contains(&Language::Nix));
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
