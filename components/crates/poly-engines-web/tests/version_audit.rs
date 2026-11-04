//! Stale-cache discipline guard for the web family.
//!
//! Every engine folds a `version()` string into the content-hash cache key, so cached output can only
//! be trusted while that string changes whenever the wrapped tool changes. For each engine this reads
//! the resolved upstream crate from the components workspace `Cargo.lock` and asserts `version()`
//! embeds the crate's version (registry dependencies) or short git rev (git dependencies).
//!
//! When a wrapped crate is bumped but the engine's `version()` is not, this test fails and names the
//! engine to bump. The root workspace's `tests/version_audit.rs` audits the engines that remain in
//! `poly-core`.

use std::collections::HashMap;
use std::path::Path;

use poly_engine_api::engine::Engine;
use poly_engines_web::{AdrlintEngine, MalvaEngine, MarkupFmtEngine, OxcEngine, RumdlEngine, TaploEngine, YamlEngine};

/// A resolved package as recorded in `Cargo.lock`.
struct LockEntry {
    version: String,
    git_rev: Option<String>,
}

/// Parse the components workspace `Cargo.lock` into `name -> LockEntry`. The lockfile is a sequence of
/// `[[package]]` blocks; for each we capture `version` and, for git sources
/// (`source = "git+…#<rev>"`), the pinned rev.
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

/// Which identifier of a wrapped crate the engine's `version()` must embed.
enum Source {
    /// crates.io dependency: the resolved semantic version must appear.
    Registry,
    /// git dependency: the short (7-char) pinned rev must appear.
    Git,
}

/// One engine's label, its `version()` and the wrapped crates it must track.
type Check<'a> = (&'a str, String, Vec<(&'a str, Source)>);

/// The number of git-rev characters that must appear in `version()`.
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
             crate `{dep}` (from Cargo.lock). The crate was bumped but version() was not; \
             bump the engine's version() so stale cached output is invalidated.",
        );
    }
}

#[test]
fn engine_versions_track_cargo_lock() {
    use Source::{Git, Registry};

    let lock = parse_cargo_lock();
    let checks: Vec<Check> = vec![
        (
            "adrlint",
            AdrlintEngine.version().to_owned(),
            vec![("adrlint", Registry)],
        ),
        (
            "malva",
            MalvaEngine.version().to_owned(),
            vec![("malva", Registry), ("oxc_formatter_css", Git)],
        ),
        (
            "markup_fmt",
            MarkupFmtEngine.version().to_owned(),
            vec![("markup_fmt", Registry), ("oxc_formatter", Git)],
        ),
        (
            "taplo",
            TaploEngine::new().version().to_owned(),
            vec![("taplo", Registry), ("oxc_formatter_toml", Git)],
        ),
        (
            "rumdl",
            RumdlEngine.version().to_owned(),
            vec![("rumdl", Registry), ("oxc_formatter_markdown", Git)],
        ),
        (
            "yaml",
            YamlEngine.version().to_owned(),
            vec![("oxc_formatter_yaml", Git)],
        ),
        ("oxc", OxcEngine.version().to_owned(), vec![("oxc_formatter", Git)]),
    ];
    assert_eq!(checks.len(), 7, "every engine of the web family is audited");
    for (engine, version, deps) in &checks {
        assert_tracks(engine, version, deps, &lock);
    }
}
