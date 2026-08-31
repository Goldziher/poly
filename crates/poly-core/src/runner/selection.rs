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

use crate::config::{Config, Kind};
use crate::engine::Engine;
use crate::language::Language;
use crate::registry::all_engine_names;

/// The engines a run was restricted to, if any.
///
/// Borrowed from [`crate::RunOptions`] for the length of a run; both lists are
/// normally empty, and `selects` is a no-op scan in that case.
#[derive(Debug, Clone, Copy)]
pub(super) struct EngineSelection<'a> {
    only: &'a [String],
    skip: &'a [String],
}

impl<'a> EngineSelection<'a> {
    /// Borrow the selection out of a run's options.
    pub(super) fn new(only: &'a [String], skip: &'a [String]) -> Self {
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
/// The recognised set is every registry engine plus the catalog tools this
/// config configures. A catalog tool the config does not mention is rejected
/// even though the catalog knows the name, because naming it would restrict the
/// run to an engine that cannot run.
pub(super) fn validate_selection(config: &Config, only: &[String], skip: &[String]) -> anyhow::Result<()> {
    if only.is_empty() && skip.is_empty() {
        return Ok(());
    }
    let mut known: BTreeSet<&str> = all_engine_names();
    known.extend(config.tools.iter().map(|(name, _)| name.as_str()));

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

    #[test]
    fn a_recognized_selection_validates() {
        let config = Config::default();
        assert!(validate_selection(&config, &names(&["ruff", "typos"]), &names(&["oxc"])).is_ok());
    }

    #[test]
    fn an_unknown_name_is_rejected_and_quoted_back() {
        let config = Config::default();
        let error = validate_selection(&config, &names(&["ruffs"]), &[]).expect_err("must reject");
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
        assert!(validate_selection(&config, &names(&["golangci-lint"]), &[]).is_ok());
        assert!(validate_selection(&Config::default(), &names(&["golangci-lint"]), &[]).is_err());
    }
}
