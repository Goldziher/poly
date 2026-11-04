//! `version()` must embed the short rev of the biome git pin in `components/Cargo.lock`, so a rev bump
//! invalidates cached results. This is the components-workspace counterpart of poly-core's
//! `version_audit`, which no longer sees the biome engines.

use std::path::Path;

use poly_engine_api::engine::Engine;
use poly_engines_biome::{BiomeCssEngine, BiomeGraphqlEngine};

const REV_PREFIX: usize = 7;

/// The pinned git rev of `krate`, read from the `source = "git+...#<rev>"` line of its lock entry.
fn locked_rev(lock: &str, krate: &str) -> String {
    let header = format!("name = \"{krate}\"");
    let entry = lock
        .split("[[package]]")
        .find(|entry| entry.lines().any(|line| line == header))
        .unwrap_or_else(|| panic!("crate `{krate}` is not in components/Cargo.lock"));
    let source = entry
        .lines()
        .find_map(|line| line.strip_prefix("source = \""))
        .unwrap_or_else(|| panic!("crate `{krate}` has no source"));
    let (_, rev) = source
        .trim_end_matches('"')
        .rsplit_once('#')
        .unwrap_or_else(|| panic!("crate `{krate}` is not a git dependency: {source}"));
    rev[..REV_PREFIX].to_owned()
}

#[test]
fn biome_engine_versions_track_the_lock() {
    let lock_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path).unwrap_or_else(|e| panic!("read {}: {e}", lock_path.display()));
    for (engine, version, krate) in [
        ("css", BiomeCssEngine.version(), "biome_css_analyze"),
        ("graphql", BiomeGraphqlEngine.version(), "biome_graphql_analyze"),
    ] {
        let rev = locked_rev(&lock, krate);
        assert!(
            version.contains(&rev),
            "the {engine} engine's version() = {version:?} must embed the git rev {rev:?} of `{krate}`; \
             bump version() with the pin"
        );
    }
}
