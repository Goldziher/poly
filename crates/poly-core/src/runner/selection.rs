//! `--only` / `--skip`: restricting a run to named engines.
//!
//! The selection **narrows and never widens**. It removes engines from a plan
//! that was already built from the resolved config, so naming an engine that is
//! off — `uncomment`, an unconfigured catalog tool, a native toolchain that is
//! not installed — selects nothing rather than switching it on. Anything else
//! would make `--only` a second, invisible way to enable a backend, and the
//! config would stop being the answer to "what does this repository run".
//!
//! A name nothing answers to is rejected before the run starts, because the
//! failure it would otherwise produce is the silent kind: `--only ruffs` would
//! empty every plan, check nothing, find nothing, and exit 0.

use std::collections::BTreeSet;

use poly_component_index::ComponentIndex;

use crate::config::{Config, Kind};
use crate::engine::Engine;
use crate::language::Language;
use crate::registry::all_engine_names;
use crate::resolve::ConfigSet;

/// The engines a run was restricted to, if any.
///
/// Borrowed from [`crate::RunOptions`] for the length of a run; both lists are
/// normally empty, and `selects` is a no-op scan in that case.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EngineSelection<'a> {
    only: &'a [String],
    skip: &'a [String],
}

impl<'a> EngineSelection<'a> {
    /// Borrow the selection out of a run's options.
    pub(crate) fn new(only: &'a [String], skip: &'a [String]) -> Self {
        Self { only, skip }
    }

    /// Whether an engine of this name survives the selection.
    ///
    /// The lists hold a handful of entries at most, so a linear scan beats
    /// hashing — and the common case is two empty slices, where this is a pair
    /// of length checks.
    pub(super) fn selects(&self, name: &str) -> bool {
        if !self.only.is_empty() && !self.only.iter().any(|entry| entry == name) {
            return false;
        }
        !self.skip.iter().any(|entry| entry == name)
    }

    /// Whether an engine survives the selection when `--only` / `--skip` may name it or the config table it
    /// declares it shares with the alternative backends (`python-types` for both ty and pyrefly).
    pub(crate) fn selects_declared(&self, name: &str, table: Option<&str>) -> bool {
        let names = |entry: &String| entry == name || table.is_some_and(|table| entry == table);
        if !self.only.is_empty() && !self.only.iter().any(names) {
            return false;
        }
        !self.skip.iter().any(names)
    }

    /// The `--only` entries.
    pub(super) fn only(&self) -> &'a [String] {
        self.only
    }

    /// The `--skip` entries.
    pub(super) fn skip(&self) -> &'a [String] {
        self.skip
    }

    /// Whether the caller restricted this run at all.
    pub(super) fn is_narrowed(&self) -> bool {
        !self.only.is_empty() || !self.skip.is_empty()
    }
}

/// Apply the selection to one language's merged engine list.
///
/// Returns the surviving engines and whether the selection is the reason this
/// plan can no longer account for the language — either it removed every engine,
/// or it removed the only ones that held lint rules for it.
///
/// That second case is why this is not a plain `retain`. `--only ruff,typos`
/// over a Rust file leaves a non-empty plan, so nothing looks wrong, yet the
/// backends that actually knew Rust are gone and the file reports
/// `no lint rules for Rust` — true, but it blames poly for a gap the invocation
/// created, and `--deny-skips` then fails a run doing exactly what was asked.
pub(super) fn apply(
    selection: EngineSelection<'_>,
    engines: Vec<Box<dyn Engine>>,
    language: &Language,
    config: &Config,
    kind: Kind,
) -> (Vec<Box<dyn Engine>>, bool) {
    if !selection.is_narrowed() {
        return (engines, false);
    }
    let routed_anything = !engines.is_empty();
    let mut dropped_coverage = false;
    let kept: Vec<Box<dyn Engine>> = engines
        .into_iter()
        .filter(|engine| {
            if selection.selects(engine.name()) {
                return true;
            }
            // Asked only of engines being removed, and only until one says yes,
            // so a narrowed run pays for at most one config slice per language.
            if kind == Kind::Lint && !dropped_coverage {
                let cfg = config.engine_config(language, engine.name(), kind);
                dropped_coverage = engine.provides_language_lint(language, &cfg);
            }
            false
        })
        .collect();
    let narrowed_away = dropped_coverage || (routed_anything && kept.is_empty());
    (kept, narrowed_away)
}

/// Reject a selection naming an engine this run could never have planned.
///
/// Checked once, before the file walk. A typo'd `--only ruffs` otherwise reads
/// exactly like a clean run: every plan is emptied, nothing is checked, no
/// diagnostic is produced, and the exit code is 0.
///
/// The recognised set is every registry engine, every engine the run's component
/// index describes (and the config table an engine declares it shares with its alternatives) (a core build links none of them, yet `--only sqruff` must
/// select the sql component's engine), plus the catalog tools any config in this
/// run configures. A catalog tool no config mentions is rejected
/// even though the catalog knows the name, because naming it would restrict the
/// run to an engine that cannot run.
pub(super) fn validate_selection(
    configs: &ConfigSet,
    index: &ComponentIndex,
    only: &[String],
    skip: &[String],
) -> anyhow::Result<()> {
    if only.is_empty() && skip.is_empty() {
        return Ok(());
    }
    let mut known: BTreeSet<&str> = all_engine_names();
    known.extend(
        index
            .components
            .iter()
            .flat_map(|component| component.engines.iter().map(|desc| desc.name.as_str())),
    );
    known.extend(
        index
            .components
            .iter()
            .flat_map(|component| component.engines.iter())
            .filter_map(|desc| desc.selection.as_ref().map(|selection| selection.table.as_str())),
    );
    // Every resolved config, not just the root one: a monorepo (ADR 0018) can
    // enable a catalog tool in a nested `poly.toml`, and naming it is a
    // legitimate selection for the files that config governs.
    known.extend(
        configs
            .iter()
            .flat_map(|config| config.tools.iter().map(|(name, _)| name.as_str())),
    );

    let unknown: Vec<&str> = only
        .iter()
        .chain(skip)
        .map(String::as_str)
        .filter(|name| !known.contains(name))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let known: Vec<&str> = known.into_iter().collect();
    anyhow::bail!(
        "unknown engine {}: {}\nrecognized engines: {}",
        if unknown.len() == 1 { "name" } else { "names" },
        unknown.join(", "),
        known.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection<'a>(only: &'a [String], skip: &'a [String]) -> EngineSelection<'a> {
        EngineSelection::new(only, skip)
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn an_empty_selection_selects_everything() {
        let (only, skip) = (Vec::new(), Vec::new());
        let selection = selection(&only, &skip);
        assert!(selection.selects("ruff"));
        assert!(selection.selects("typos"));
    }

    #[test]
    fn only_keeps_the_named_engine_alone() {
        let (only, skip) = (names(&["ruff"]), Vec::new());
        let selection = selection(&only, &skip);
        assert!(selection.selects("ruff"));
        assert!(!selection.selects("typos"));
    }

    #[test]
    fn skip_removes_the_named_engine_alone() {
        let (only, skip) = (Vec::new(), names(&["typos"]));
        let selection = selection(&only, &skip);
        assert!(selection.selects("ruff"));
        assert!(!selection.selects("typos"));
    }

    fn set(config: Config) -> ConfigSet {
        ConfigSet::single(config)
    }

    #[test]
    fn a_recognized_selection_validates() {
        // The compiled index, so the names resolve whether this build links their families or a
        // component serves them.
        assert!(
            validate_selection(
                &set(Config::default()),
                poly_component_index::index(),
                &names(&["ruff", "typos"]),
                &names(&["sqruff"])
            )
            .is_ok()
        );
    }

    #[test]
    fn an_unknown_name_is_rejected_and_quoted_back() {
        let error = validate_selection(
            &set(Config::default()),
            poly_component_index::index(),
            &names(&["ruffs"]),
            &[],
        )
        .expect_err("must reject");
        let message = error.to_string();
        assert!(message.contains("ruffs"), "{message}");
        assert!(
            message.contains("ruff"),
            "the hint must list what is recognized: {message}"
        );
    }

    /// A configured catalog tool is a legitimate selection target even though it
    /// is not in the registry.
    #[test]
    fn a_configured_catalog_tool_is_recognized() {
        let config = Config {
            tools: toml::from_str("[golangci-lint]\nenabled = true\n").expect("valid tool config"),
            ..Config::default()
        };
        assert!(
            validate_selection(
                &set(config),
                &ComponentIndex::default(),
                &names(&["golangci-lint"]),
                &[]
            )
            .is_ok()
        );
        assert!(
            validate_selection(
                &set(Config::default()),
                &ComponentIndex::default(),
                &names(&["golangci-lint"]),
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn the_table_an_engine_shares_with_its_alternatives_is_a_recognized_name() {
        use poly_component_index::{Component, ComponentKind};
        use poly_component_protocol::{WireEngineDesc, WireSelection};

        let mut desc: WireEngineDesc = serde_json::from_value(serde_json::json!({
            "name": "alt", "languages": ["python"],
            "capabilities": {"lint": false, "format": false, "fix": false, "project": true},
            "version": "1", "option_keys": [], "hooks": 0, "takes_over_tier_one": false,
            "claimed_paths": [], "host_hook": "none", "supplements_tier_one": false,
            "self_manages_enabled": false, "supersedes_generic_formatter": false, "batch_shard_floor": 2,
        }))
        .expect("engine description");
        desc.selection = Some(WireSelection {
            table: "shared-table".into(),
            key: "backend".into(),
            value: "alt".into(),
            default: false,
        });
        let index = ComponentIndex {
            schema: 1,
            components: vec![Component {
                name: "alt".into(),
                kind: ComponentKind::Engines,
                opt_in: true,
                features: vec!["engine-alt".into()],
                engines: vec![desc],
                lock_sha: None,
            }],
            presets: Default::default(),
        };
        let check = |name: &str| validate_selection(&set(Config::default()), &index, &names(&[name]), &[]);
        assert!(check("shared-table").is_ok());
        assert!(check("alt").is_ok());
        assert!(check("other-table").is_err());
    }
}
