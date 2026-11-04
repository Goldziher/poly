//! Stale-cache discipline guard.
//!
//! Every engine folds a `version()` string into the content-hash cache key, so
//! cached output can only be trusted while that string changes whenever the
//! wrapped tool changes. This test enforces that link: for each backend it reads
//! the resolved upstream crate from the workspace `Cargo.lock` and asserts the
//! engine's `version()` embeds the crate's version. The git-pinned web family
//! is audited against `components/Cargo.lock` in `poly-engines-web`'s own
//! `tests/version_audit.rs`.
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
#![cfg(feature = "all-engines")]

use std::collections::HashMap;
use std::path::Path;

use poly_core::engine::Engine;
use poly_core::engines::astgrep::AstGrepEngine;
use poly_core::engines::dockerfile::DockerfileEngine;
use poly_core::engines::dotenv::DotenvEngine;
use poly_core::engines::graphql::GraphQlEngine;
use poly_core::engines::hcl::HclEngine;
use poly_core::engines::ini::IniEngine;
use poly_core::engines::mago::MagoEngine;
use poly_core::engines::nixfmt::NixFmtEngine;
use poly_core::engines::quality::QualityEngine;
use poly_core::engines::ruff::RuffEngine;
use poly_core::engines::sqruff::SqruffEngine;
use poly_core::engines::treesitter::TreeSitterEngine;
use poly_core::engines::ty::TyEngine;
use poly_core::engines::typos::TyposEngine;
use poly_core::engines::uncomment::UncommentEngine;

/// Parse the workspace `Cargo.lock` into `name -> version`. The lockfile is a
/// sequence of `[[package]]` blocks. The core workspace carries no git
/// dependencies (they live in `components/`), so the resolved version is the
/// only identifier an engine's `version()` has to embed.
fn parse_cargo_lock() -> HashMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let mut map = HashMap::new();
    for block in text.split("[[package]]") {
        let mut name = None;
        let mut version = None;
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("name = \"") {
                name = Some(rest.trim_end_matches('"'));
            } else if let Some(rest) = line.strip_prefix("version = \"") {
                version = Some(rest.trim_end_matches('"'));
            }
        }
        if let (Some(name), Some(version)) = (name, version) {
            map.insert(name.to_owned(), version.to_owned());
        }
    }
    map
}

/// One engine's `version()` string and the wrapped crate(s) it must track.
struct Check {
    /// Human label for the failure message, derived from the backend module —
    /// deliberately *not* `Engine::name()`, which is a per-language tool id and
    /// is neither unique (two engines can share one tool id)
    /// nor always the module name (`nixfmt` wraps, and is named, `alejandra`).
    /// See the [`Engine::name`] contract; this list needs one distinct label per
    /// *row*, which the tool ids cannot supply.
    engine: &'static str,
    version: String,
    deps: Vec<&'static str>,
}

fn assert_tracks(engine: &str, version: &str, deps: &[&str], lock: &HashMap<String, String>) {
    for dep in deps {
        let needle = lock
            .get(*dep)
            .unwrap_or_else(|| panic!("crate `{dep}` not found in Cargo.lock (engine `{engine}`)"));
        assert!(
            version.contains(needle.as_str()),
            "engine `{engine}` version() = {version:?} must embed the version {needle:?} of \
             crate `{dep}` (from Cargo.lock). The crate was bumped but version() was not — \
             bump the engine's version() so stale cached output is invalidated.",
        );
    }
}

#[test]
fn engine_versions_track_cargo_lock() {
    let lock = parse_cargo_lock();

    let check = |engine, version: &str, deps| Check {
        engine,
        version: version.to_owned(),
        deps,
    };

    let checks = vec![
        check("sqruff", SqruffEngine.version(), vec!["sqruff-lib"]),
        check("typos", TyposEngine.version(), vec!["typos", "typos-dict"]),
        check("hcl", HclEngine.version(), vec!["hcl-edit"]),
        check("dockerfile", DockerfileEngine.version(), vec!["dockerfile-parser"]),
        // `NixFmtEngine::name()` is `"alejandra"` (the wrapped formatter it is
        // named after), not `"nixfmt"` — label matches the real name so the
        // exhaustiveness guard above can find this check.
        check("alejandra", NixFmtEngine.version(), vec!["alejandra"]),
        check("graphql", GraphQlEngine.version(), vec!["pretty_graphql"]),
        check(
            "treesitter",
            TreeSitterEngine.version(),
            vec!["tree-sitter-language-pack"],
        ),
        check("mago", MagoEngine::default().version(), vec!["mago-formatter"]),
        check("ruff", RuffEngine.version(), vec!["ruff_linter"]),
        check("python-types", TyEngine.version(), vec!["ty_python_semantic"]),
        check("uncomment", UncommentEngine.version(), vec!["uncomment"]),
        check("dotenv", DotenvEngine.version(), vec!["dotenv-analyzer", "dotenv-core"]),
        check("ini", IniEngine.version(), vec!["rust-ini"]),
        // `astgrep` parses with the same grammar pack as `treesitter`, so both
        // must embed the same `tree-sitter-language-pack` version — new grammars
        // change what rules match. It was omitted from this list until the pack
        // moved 1.14.3 -> 1.15.0 and the audit stayed green while astgrep still
        // advertised the old pack, which is the exact stale-cache failure this
        // test exists to prevent.
        check(
            "astgrep",
            AstGrepEngine.version(),
            vec!["ast-grep-core", "tree-sitter-language-pack"],
        ),
        // `quality` parses with the same grammar pack as `treesitter`/`astgrep` for
        // its structural rules (nesting, complexity, definitions, magic-number,
        // law-of-demeter); a grammar-pack bump can change what those rules see.
        check("quality", QualityEngine.version(), vec!["tree-sitter-language-pack"]),
    ];

    for Check { engine, version, deps } in &checks {
        assert_tracks(engine, version, deps, &lock);
    }
}
