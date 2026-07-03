//! Stale-cache discipline guard.
//!
//! Every engine folds a `version()` string into the content-hash cache key, so
//! cached output can only be trusted while that string changes whenever the
//! wrapped tool changes. This test enforces that link: for each backend it reads
//! the resolved upstream crate from the workspace `Cargo.lock` and asserts the
//! engine's `version()` embeds the crate's version (registry dependencies) or
//! short git rev (git dependencies).
//!
//! When a wrapped crate is bumped in `Cargo.toml`/`Cargo.lock` but the engine's
//! `version()` is not, this test fails and names the engine to bump — which also
//! forces a conscious update of the hand-maintained `+suffix` logic marker.
//!
//! Catalog and native-toolchain backends are intentionally excluded: they wrap
//! external processes, not a pinned Rust crate, so there is no lock entry to
//! track.

use std::collections::HashMap;
use std::path::Path;

use polylint_core::engine::Engine;
use polylint_core::engines::biome_css::BiomeCssEngine;
use polylint_core::engines::biome_graphql::BiomeGraphqlEngine;
use polylint_core::engines::dockerfile::DockerfileEngine;
use polylint_core::engines::graphql::GraphQlEngine;
use polylint_core::engines::hcl::HclEngine;
use polylint_core::engines::mago::MagoEngine;
use polylint_core::engines::malva::MalvaEngine;
use polylint_core::engines::markup_fmt::MarkupFmtEngine;
use polylint_core::engines::nixfmt::NixFmtEngine;
use polylint_core::engines::oxc::OxcEngine;
use polylint_core::engines::rubyfmt::RubyfmtEngine;
use polylint_core::engines::ruff::RuffEngine;
use polylint_core::engines::rumdl::RumdlEngine;
use polylint_core::engines::sqruff::SqruffEngine;
use polylint_core::engines::taplo::TaploEngine;
use polylint_core::engines::treesitter::TreeSitterEngine;
use polylint_core::engines::typos::TyposEngine;
use polylint_core::engines::yaml::YamlEngine;

/// A resolved package as recorded in `Cargo.lock`.
struct LockEntry {
    version: String,
    git_rev: Option<String>,
}

/// Parse the workspace `Cargo.lock` into `name -> LockEntry`. The lockfile is a
/// sequence of `[[package]]` blocks; for each we capture `version` and, for git
/// sources (`source = "git+…#<rev>"`), the pinned rev.
fn parse_cargo_lock() -> HashMap<String, LockEntry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let mut map = HashMap::new();
    for block in text.split("[[package]]") {
        let mut name = None;
        let mut version = None;
        let mut git_rev = None;
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("name = \"") {
                name = Some(rest.trim_end_matches('"'));
            } else if let Some(rest) = line.strip_prefix("version = \"") {
                version = Some(rest.trim_end_matches('"'));
            } else if let Some(rest) = line.strip_prefix("source = \"")
                && rest.starts_with("git+")
                && let Some(hash) = rest.rfind('#')
            {
                git_rev = Some(rest[hash + 1..].trim_end_matches('"').to_owned());
            }
        }
        if let (Some(name), Some(version)) = (name, version) {
            map.insert(
                name.to_owned(),
                LockEntry {
                    version: version.to_owned(),
                    git_rev,
                },
            );
        }
    }
    map
}

/// One engine's `version()` string and the wrapped crate(s) it must track.
struct Check {
    engine: &'static str,
    version: String,
    deps: Vec<(&'static str, Source)>,
}

/// Which identifier of a wrapped crate the engine's `version()` must embed.
enum Source {
    /// crates.io dependency — the resolved semantic version must appear.
    Registry,
    /// git dependency — the short (7-char) pinned rev must appear.
    Git,
}

/// The number of git-rev characters that must appear in `version()`. Every
/// backend embeds at least this prefix (`rev:5762638`, `c916545`, the full ruff
/// rev, …), which is unambiguous across the dependency tree.
const GIT_REV_PREFIX: usize = 7;

fn assert_tracks(engine: &str, version: &str, deps: &[(&str, Source)], lock: &HashMap<String, LockEntry>) {
    for (dep, source) in deps {
        let entry = lock
            .get(*dep)
            .unwrap_or_else(|| panic!("crate `{dep}` not found in Cargo.lock (engine `{engine}`)"));
        let (needle, kind): (String, &str) = match source {
            Source::Registry => (entry.version.clone(), "version"),
            Source::Git => {
                let rev = entry
                    .git_rev
                    .as_deref()
                    .unwrap_or_else(|| panic!("crate `{dep}` is not a git dependency but was declared as one"));
                (rev[..GIT_REV_PREFIX].to_owned(), "git rev")
            }
        };
        assert!(
            version.contains(&needle),
            "engine `{engine}` version() = {version:?} must embed the {kind} {needle:?} of \
             crate `{dep}` (from Cargo.lock). The crate was bumped but version() was not — \
             bump the engine's version() so stale cached output is invalidated.",
        );
    }
}

#[test]
fn engine_versions_track_cargo_lock() {
    use Source::{Git, Registry};

    let lock = parse_cargo_lock();

    let check = |engine, version: &str, deps| Check {
        engine,
        version: version.to_owned(),
        deps,
    };

    let checks = vec![
        check(
            "biome-graphql",
            BiomeGraphqlEngine.version(),
            vec![("biome_graphql_analyze", Git)],
        ),
        check("biome-css", BiomeCssEngine.version(), vec![("biome_css_analyze", Git)]),
        check("sqruff", SqruffEngine.version(), vec![("sqruff-lib", Registry)]),
        check("malva", MalvaEngine.version(), vec![("malva", Registry)]),
        check("markup_fmt", MarkupFmtEngine.version(), vec![("markup_fmt", Registry)]),
        check("taplo", TaploEngine.version(), vec![("taplo", Registry)]),
        check("rumdl", RumdlEngine.version(), vec![("rumdl", Registry)]),
        check(
            "typos",
            TyposEngine.version(),
            vec![("typos", Registry), ("typos-dict", Registry)],
        ),
        check(
            "hcl",
            HclEngine.version(),
            vec![("hcl-rs", Registry), ("hcl-edit", Registry)],
        ),
        check(
            "dockerfile",
            DockerfileEngine.version(),
            vec![("dockerfile-parser", Registry)],
        ),
        check("nixfmt", NixFmtEngine.version(), vec![("alejandra", Registry)]),
        check("graphql", GraphQlEngine.version(), vec![("pretty_graphql", Registry)]),
        check("yaml", YamlEngine.version(), vec![("pretty_yaml", Registry)]),
        check(
            "treesitter",
            TreeSitterEngine.version(),
            vec![("tree-sitter-language-pack", Registry)],
        ),
        check(
            "mago",
            MagoEngine::default().version(),
            vec![("mago-formatter", Registry)],
        ),
        check("oxc", OxcEngine.version(), vec![("oxc_formatter", Git)]),
        check("ruff", RuffEngine.version(), vec![("ruff_linter", Git)]),
        check("rubyfmt", RubyfmtEngine.version(), vec![("rubyfmt", Git)]),
    ];

    for Check { engine, version, deps } in &checks {
        assert_tracks(engine, version, deps, &lock);
    }
}
