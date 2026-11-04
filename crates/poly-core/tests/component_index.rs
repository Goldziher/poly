//! Drift test for the compiled component index (`crates/poly-component-index/data/index.json`).
//!
//! The index is regenerated from the registered engines: each engine is described through the
//! same `engine_desc` mapping the component producers use and grouped by component through
//! `PLACEMENT`. The committed file must equal the regeneration.
//! Refresh it with `UPDATE_INDEX=1 cargo test -p poly-core --test component_index`.

#![cfg(feature = "all-engines")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_core::registered_engines;
use poly_engine_api::desc::engine_desc;

/// Where an engine ships: a downloadable component or the always-compiled core.
enum Placement {
    Component(&'static str),
    Core,
}

use Placement::{Component as In, Core};

/// Engine name to placement (docs/size-baseline.md "Decisions"). An engine missing here fails the
/// test, so a new engine must be placed deliberately.
const PLACEMENT: &[(&str, Placement)] = &[
    ("oxc", In("web")),
    ("taplo", In("web")),
    ("rumdl", In("web")),
    ("adrlint", In("web")),
    ("yaml", In("web")),
    ("malva", In("web")),
    ("markup_fmt", In("web")),
    ("ruff", In("python")),
    ("python-types", In("python")),
    ("sqruff", In("sql")),
    ("mago", In("php")),
    ("alejandra", In("nix")),
    ("typos", In("typos")),
    ("astgrep", In("astgrep")),
    ("dockerfile", In("misc")),
    ("dotenv", In("misc")),
    ("hcl", In("misc")),
    ("graphql", In("misc")),
    ("quality", Core),
    ("uncomment", Core),
    ("ini", Core),
    ("treesitter", Core),
    ("polyconfig", Core),
    ("gofmt", Core),
    ("rustfmt", Core),
    ("zigfmt", Core),
    ("shfmt", Core),
    ("shellcheck", Core),
    ("google-java-format", Core),
    ("ktfmt", Core),
    ("styler", Core),
    ("swift-format", Core),
    ("dartfmt", Core),
    ("gleamfmt", Core),
];

/// Components whose engines `poly-core` cannot link (their upstream crates are git pins that live in
/// the `components/` workspace), so their index entries are carried over from the committed index.
/// Each family crate's own test (`poly-engines-ruby`'s `tests/rubyfmt.rs`, `poly-engines-biome`'s
/// `tests/index.rs`) checks its entry against the real engines.
const EXTERNAL_COMPONENTS: &[&str] = &["biome", "ruby"];

/// Components in index order: name, kind, opt-in, cargo features.
const COMPONENTS: &[(&str, ComponentKind, bool, &[&str])] = &[
    ("web", ComponentKind::Engines, false, &["engine-web"]),
    ("python", ComponentKind::Engines, false, &["engine-python"]),
    ("sql", ComponentKind::Engines, false, &["engine-sql"]),
    ("php", ComponentKind::Engines, false, &["engine-php"]),
    ("biome", ComponentKind::Engines, false, &["engine-biome"]),
    ("ruby", ComponentKind::Engines, false, &["engine-ruby"]),
    ("nix", ComponentKind::Engines, false, &["engine-nix"]),
    ("typos", ComponentKind::Engines, true, &["engine-typos"]),
    ("astgrep", ComponentKind::Engines, false, &["engine-astgrep"]),
    (
        "misc",
        ComponentKind::Engines,
        false,
        &["engine-dockerfile", "engine-dotenv", "engine-hcl", "engine-graphql"],
    ),
    ("mcp", ComponentKind::Frontend, false, &[]),
];

/// Language aliases usable as presets, mapped to the language ids they cover.
const ALIASES: &[(&str, &[&str])] = &[
    ("js", &["javascript", "jsx"]),
    ("ts", &["typescript", "tsx"]),
    ("json", &["json", "jsonc"]),
    ("css", &["css", "scss", "less"]),
    ("markdown", &["markdown", "mdx"]),
    ("yaml", &["yaml"]),
    ("toml", &["toml"]),
    (
        "html",
        &[
            "html", "vue", "svelte", "astro", "angular", "jinja", "vento", "mustache", "xml",
        ],
    ),
    ("python", &["python"]),
    ("php", &["php"]),
    ("sql", &["sql"]),
    ("nix", &["nix"]),
    ("ruby", &["ruby"]),
    ("graphql", &["graphql"]),
    ("dockerfile", &["dockerfile"]),
    ("hcl", &["hcl"]),
    ("dotenv", &["dotenv"]),
];

fn index_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../poly-component-index/data/index.json"
    ))
}

fn regenerate(committed: &ComponentIndex) -> ComponentIndex {
    let placement: BTreeMap<&str, &Placement> = PLACEMENT.iter().map(|(name, p)| (*name, p)).collect();
    let mut by_component: BTreeMap<&str, Vec<poly_component_protocol::WireEngineDesc>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for engine in registered_engines() {
        let desc = engine_desc(engine.as_ref());
        seen.insert(desc.name.clone());
        match placement.get(desc.name.as_str()) {
            None => panic!("engine `{}` has no entry in PLACEMENT", desc.name),
            Some(Core) => {}
            Some(In(component)) => {
                let engines = by_component.entry(component).or_default();
                if !engines.contains(&desc) {
                    engines.push(desc);
                }
            }
        }
    }
    let stale: Vec<_> = PLACEMENT
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !seen.contains(*n))
        .collect();
    assert!(
        stale.is_empty(),
        "PLACEMENT names engines that are not registered: {stale:?}"
    );

    let components: Vec<Component> = COMPONENTS
        .iter()
        .map(|(name, kind, opt_in, features)| {
            let mut engines = by_component.remove(name).unwrap_or_default();
            if EXTERNAL_COMPONENTS.contains(name) {
                engines = committed.component(name).map(|c| c.engines.clone()).unwrap_or_default();
            }
            engines.sort_by(|a, b| (&a.name, &a.languages).cmp(&(&b.name, &b.languages)));
            Component {
                name: (*name).to_owned(),
                kind: *kind,
                opt_in: *opt_in,
                features: features.iter().map(|f| (*f).to_owned()).collect(),
                engines,
                lock_sha: committed.component(name).and_then(|c| c.lock_sha.clone()),
            }
        })
        .collect();
    assert!(
        by_component.is_empty(),
        "PLACEMENT names unknown components: {:?}",
        by_component.keys()
    );

    let mut presets = BTreeMap::new();
    let all: Vec<String> = components
        .iter()
        .filter(|c| c.kind == ComponentKind::Engines && !c.opt_in)
        .map(|c| c.name.clone())
        .collect();
    presets.insert("all".to_owned(), all);
    for (alias, languages) in ALIASES {
        let serving: Vec<String> = components
            .iter()
            .filter(|c| {
                c.engines
                    .iter()
                    .any(|e| e.languages.iter().any(|l| languages.contains(&l.as_str())))
            })
            .map(|c| c.name.clone())
            .collect();
        assert!(!serving.is_empty(), "alias `{alias}` matches no component");
        presets.insert((*alias).to_owned(), serving);
    }
    ComponentIndex {
        schema: 1,
        components,
        presets,
    }
}

fn render(index: &ComponentIndex) -> String {
    let mut text = serde_json::to_string_pretty(index).expect("index serializes");
    text.push('\n');
    text
}

#[test]
fn committed_index_matches_the_registered_engines() {
    let path = index_path();
    let committed_text = std::fs::read_to_string(&path).expect("index.json is committed");
    let committed = ComponentIndex::parse(&committed_text).expect("index.json parses");
    let generated = render(&regenerate(&committed));
    if std::env::var_os("UPDATE_INDEX").is_some() {
        std::fs::write(&path, &generated).expect("write index.json");
        return;
    }
    // Compared as JSON values: `poly fmt` owns the committed file's layout.
    let committed_value: serde_json::Value = serde_json::from_str(&committed_text).expect("committed index is JSON");
    let generated_value: serde_json::Value = serde_json::from_str(&generated).expect("generated index is JSON");
    assert!(
        committed_value == generated_value,
        "component index drifted; regenerate with `UPDATE_INDEX=1 cargo test -p poly-core --test component_index`"
    );
}

#[test]
fn every_engine_component_hosts_engines_and_every_engine_has_one_home() {
    let committed = ComponentIndex::parse(&std::fs::read_to_string(index_path()).expect("index.json is committed"))
        .expect("index.json parses");
    let index = regenerate(&committed);
    for component in &index.components {
        assert_eq!(
            component.kind == ComponentKind::Engines,
            !component.engines.is_empty(),
            "component `{}`",
            component.name
        );
    }
    let mut homes: BTreeMap<&str, &str> = BTreeMap::new();
    for component in &index.components {
        for engine in &component.engines {
            let previous = homes.insert(&engine.name, &component.name);
            assert!(
                previous.is_none_or(|p| p == component.name),
                "engine `{}` is in two components",
                engine.name
            );
        }
    }
}
