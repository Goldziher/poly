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
//!
//! This file's `checks` list is hand-maintained, which is exactly how
//! `astgrep` was once omitted while it silently advertised a stale
//! `tree-sitter-language-pack` version. It cannot enumerate the registry
//! itself (`registry::engines_for` is `pub(crate)`), so the companion guard
//! `registry::tests::every_registered_engine_is_audited_or_declared_exempt`
//! (`src/registry.rs`) walks every engine the registry actually wires up and
//! asserts each one either has a `check(...)` entry in this file or is listed
//! in that test's `NATIVE_TOOLCHAIN_ENGINES` exemption. Run it alongside this
//! file: `cargo test -p poly-core --lib registry::tests`.

use std::collections::HashMap;
use std::path::Path;

use poly_core::engine::Engine;
use poly_core::engines::astgrep::AstGrepEngine;
use poly_core::engines::biome_css::BiomeCssEngine;
use poly_core::engines::biome_graphql::BiomeGraphqlEngine;
use poly_core::engines::dockerfile::DockerfileEngine;
use poly_core::engines::graphql::GraphQlEngine;
use poly_core::engines::hcl::HclEngine;
use poly_core::engines::mago::MagoEngine;
use poly_core::engines::malva::MalvaEngine;
use poly_core::engines::markup_fmt::MarkupFmtEngine;
use poly_core::engines::nixfmt::NixFmtEngine;
use poly_core::engines::oxc::OxcEngine;
use poly_core::engines::rubyfmt::RubyfmtEngine;
use poly_core::engines::ruff::RuffEngine;
use poly_core::engines::rumdl::RumdlEngine;
use poly_core::engines::sqruff::SqruffEngine;
use poly_core::engines::taplo::TaploEngine;
use poly_core::engines::treesitter::TreeSitterEngine;
use poly_core::engines::typos::TyposEngine;
use poly_core::engines::uncomment::UncommentEngine;
use poly_core::engines::yaml::YamlEngine;

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
    /// Human label for the failure message, derived from the backend module —
    /// deliberately *not* `Engine::name()`, which is a per-language tool id and
    /// is neither unique (`biome_css` and `biome_graphql` are both `"biome"`)
    /// nor always the module name (`nixfmt` wraps, and is named, `alejandra`).
    /// See the [`Engine::name`] contract; this list needs one distinct label per
    /// *row*, which the tool ids cannot supply.
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
        // `BiomeGraphqlEngine` and `BiomeCssEngine` both answer `Engine::name()
        // == "biome"` (one config/cache namespace, two wrapped analyzer
        // crates), so both checks below use the label `"biome"` — matching the
        // real `name()` is what lets `registry::tests::
        // every_registered_engine_is_audited_or_declared_exempt` find them.
        check(
            "biome",
            BiomeGraphqlEngine.version(),
            vec![("biome_graphql_analyze", Git)],
        ),
        check("biome", BiomeCssEngine.version(), vec![("biome_css_analyze", Git)]),
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
        // `NixFmtEngine::name()` is `"alejandra"` (the wrapped formatter it is
        // named after), not `"nixfmt"` — label matches the real name so the
        // exhaustiveness guard above can find this check.
        check("alejandra", NixFmtEngine.version(), vec![("alejandra", Registry)]),
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
        check("uncomment", UncommentEngine.version(), vec![("uncomment", Registry)]),
        // `astgrep` parses with the same grammar pack as `treesitter`, so both
        // must embed the same `tree-sitter-language-pack` version — new grammars
        // change what rules match. It was omitted from this list until the pack
        // moved 1.14.3 -> 1.15.0 and the audit stayed green while astgrep still
        // advertised the old pack, which is the exact stale-cache failure this
        // test exists to prevent.
        check(
            "astgrep",
            AstGrepEngine.version(),
            vec![("ast-grep-core", Registry), ("tree-sitter-language-pack", Registry)],
        ),
    ];

    for Check { engine, version, deps } in &checks {
        assert_tracks(engine, version, deps, &lock);
    }
}
