//! The hoisting algorithm: v1 per-language, per-phase tables in, v2 engine tables out.
//!
//! # Algorithm
//!
//! For every engine `e`, every v1 table is a *cell* addressed by `(scope, phase)`: the scope is
//! a language id, or "agnostic" for the cross-cutting engines (`typos`, `astgrep`, `uncomment`,
//! `quality`) whose v1 table has no language level. An engine that has no `[fmt...]` cell at all
//! and is cross-cutting is lint-only, so its lint cells are mirrored into the format phase; that
//! is what lets a lint-only key settle at engine level instead of under `.lint`.
//!
//! Each option key `k` is placed independently, bottom-up, and the placement never widens the
//! set of tables `k` reached in v1:
//!
//! 1. **Language to phase.** In phase `p`, `k` moves from the language cells to the phase level
//!    when every language `e` serves in `p` has a cell carrying `k` with an equal value, and no
//!    agnostic cell sets `k` in `p`. The served set comes from the real registry
//!    ([`poly_core::engine_languages`]), so a single-language engine hoists as soon as its one
//!    language agrees, while an engine spanning `typescript`, `tsx`, `json`, ... never hoists a
//!    value that only some of them had. Cells for languages the engine does not serve stay
//!    language-level. An empty served set (the engine family is compiled out) never hoists. An
//!    agnostic cell's `k` is phase level to begin with.
//! 2. **Phase to engine.** When both phases hold `k` at phase level with equal values, it moves
//!    to `[engines.e]`. Otherwise it stays in `[engines.e.lint]` / `[engines.e.format]`.
//! 3. **Leftover language values.** Per language `l`: present in both phases with equal values
//!    gives `[engines.e.lang.l]`; present in one phase, or differing, gives
//!    `[engines.e.lang.l.lint]` / `[engines.e.lang.l.format]`.
//!
//! v2 precedence (engine, phase, language, language+phase) is the order of that list, so each
//! step keeps the value a v1 file resolved to. Rule overrides (`rules.<id>`) have no phase: they
//! take the same language-to-engine hoist as a key, over the languages served in the lint phase
//! (the format phase when the engine cannot lint), and otherwise stay at `lang.<l>.rules.<id>`. Everything is held in ordered maps, so the output depends only
//! on the input.
//!
//! Hoisting therefore never widens a setting: a hoisted value was already present for every
//! language the engine serves in that phase.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use toml_edit::value;

use super::CROSS_CUTTING_ENGINES;
use super::collect::{Cell, Collected, EngineCells, Entry, Phase, Scope};

/// Languages an engine serves, per phase: `(lint, format)`.
pub(super) type Served = (Vec<String>, Vec<String>);

/// Looks up the languages `engine` serves; production uses the real registry.
pub(super) type ServedLookup<'a> = &'a dyn Fn(&str) -> Served;

/// The registry's answer for `engine`.
pub(super) fn registry_served(engine: &str) -> Served {
    (
        poly_core::engine_languages(engine, false),
        poly_core::engine_languages(engine, true),
    )
}

/// Options for one table of the output, by key.
pub(super) type Options = BTreeMap<String, Entry>;

/// `[engines.<id>.lang.<l>]` and its sub-tables.
#[derive(Debug, Default, Clone)]
pub(super) struct LangPlaced {
    /// Options for both phases.
    pub options: Options,
    /// `lint` / `format` sub-tables.
    pub phases: BTreeMap<Phase, Options>,
    /// `rules.<id>` overrides.
    pub rules: Options,
}

/// `[engines.<id>]` and its sub-tables.
#[derive(Debug, Default, Clone)]
pub(super) struct EnginePlaced {
    /// Options for every phase and language.
    pub options: Options,
    /// `lint` / `format` sub-tables.
    pub phases: BTreeMap<Phase, Options>,
    /// `lang.<l>` tables.
    pub langs: BTreeMap<String, LangPlaced>,
    /// `rules.<id>` overrides.
    pub rules: Options,
}

/// The v2 layout of every engine.
pub(super) type Placed = BTreeMap<String, EnginePlaced>;

/// Place every engine; when `keep_typos_default`, make typos' v1 default-on explicit.
pub(super) fn place(
    collected: &Collected,
    keep_typos_default: bool,
    served: ServedLookup<'_>,
    notes: &mut Vec<String>,
) -> Result<Placed> {
    let mut placed = Placed::new();
    for (name, cells) in collected {
        let engine = place_engine(name, cells, &served(name), notes)?;
        placed.insert(name.clone(), engine);
    }
    if keep_typos_default {
        keep_typos_on(&mut placed);
    }
    Ok(placed)
}

/// typos is opt-in in v2. Unless the file already switched it off for the whole engine, write
/// `enabled = true` so the migrated config keeps v1's behaviour.
fn keep_typos_on(placed: &mut Placed) {
    let typos = placed.entry("typos".to_string()).or_default();
    let explicit = typos.options.contains_key("enabled") || typos.phases.values().any(|o| o.contains_key("enabled"));
    if explicit {
        return;
    }
    let item = value(true);
    typos.options.insert(
        "enabled".to_string(),
        Entry {
            comments: vec!["# typos is opt-in in poly v2; this keeps the v1 default (on).".to_string()],
            canon: "true".to_string(),
            item,
            order: 0,
        },
    );
}

fn place_engine(name: &str, source: &EngineCells, served: &Served, notes: &mut Vec<String>) -> Result<EnginePlaced> {
    let mut cells = source.cells.clone();
    let lint_only = CROSS_CUTTING_ENGINES.contains(&name) && !cells.keys().any(|(_, phase)| *phase == Phase::Format);
    if lint_only {
        let mirrored: Vec<_> = cells
            .iter()
            .map(|((scope, _), cell)| ((scope.clone(), Phase::Format), cell.clone()))
            .collect();
        cells.extend(mirrored);
    }
    let keys: BTreeSet<&String> = cells.values().flat_map(|cell| cell.options.keys()).collect();
    let mut engine = EnginePlaced::default();
    for key in keys {
        place_key(key, &cells, served, &mut engine);
    }
    place_rules(name, &cells, served, &mut engine)?;
    note_language_values(name, &engine, notes);
    Ok(engine)
}

/// Language cells of `phase`, by language id.
fn language_cells(cells: &BTreeMap<(Scope, Phase), Cell>, phase: Phase) -> BTreeMap<&str, &Cell> {
    cells
        .iter()
        .filter(|((_, p), _)| *p == phase)
        .filter_map(|((scope, _), cell)| scope.as_deref().map(|language| (language, cell)))
        .collect()
}

fn place_key(key: &str, cells: &BTreeMap<(Scope, Phase), Cell>, served: &Served, engine: &mut EnginePlaced) {
    let mut phase_level: BTreeMap<Phase, Entry> = BTreeMap::new();
    let mut leftover: BTreeMap<Phase, BTreeMap<&str, &Entry>> = BTreeMap::new();
    for phase in Phase::ALL {
        let agnostic = cells.get(&(None, phase)).and_then(|cell| cell.options.get(key));
        let languages = language_cells(cells, phase);
        let carrying: BTreeMap<&str, &Entry> = languages
            .iter()
            .filter_map(|(language, cell)| cell.options.get(key).map(|entry| (*language, entry)))
            .collect();
        let serving = if phase == Phase::Lint { &served.0 } else { &served.1 };
        let served_entries: Vec<&Entry> = serving
            .iter()
            .filter_map(|language| carrying.get(language.as_str()).copied())
            .collect();
        let hoist = agnostic.is_none()
            && !serving.is_empty()
            && served_entries.len() == serving.len()
            && all_equal(served_entries.iter().copied());
        if hoist {
            phase_level.insert(phase, merge(served_entries.iter().copied()));
            let rest = carrying
                .into_iter()
                .filter(|(language, _)| !serving.iter().any(|s| s.as_str() == *language))
                .collect();
            leftover.insert(phase, rest);
        } else {
            if let Some(entry) = agnostic {
                phase_level.insert(phase, entry.clone());
            }
            leftover.insert(phase, carrying);
        }
    }
    let engine_level = match (phase_level.get(&Phase::Lint), phase_level.get(&Phase::Format)) {
        (Some(lint), Some(format)) if lint.canon == format.canon => Some(merge([lint, format])),
        _ => None,
    };
    if let Some(entry) = engine_level {
        engine.options.insert(key.to_string(), entry);
    } else {
        for (phase, entry) in phase_level {
            engine.phases.entry(phase).or_default().insert(key.to_string(), entry);
        }
    }
    let languages: BTreeSet<&str> = leftover.values().flat_map(|m| m.keys().copied()).collect();
    for language in languages {
        let lint = leftover.get(&Phase::Lint).and_then(|m| m.get(language)).copied();
        let format = leftover.get(&Phase::Format).and_then(|m| m.get(language)).copied();
        let target = engine.langs.entry(language.to_string()).or_default();
        match (lint, format) {
            (Some(a), Some(b)) if a.canon == b.canon => {
                target.options.insert(key.to_string(), merge([a, b]));
            }
            (a, b) => {
                for (phase, entry) in [(Phase::Lint, a), (Phase::Format, b)] {
                    if let Some(entry) = entry {
                        target
                            .phases
                            .entry(phase)
                            .or_default()
                            .insert(key.to_string(), entry.clone());
                    }
                }
            }
        }
    }
}

fn place_rules(
    engine_name: &str,
    cells: &BTreeMap<(Scope, Phase), Cell>,
    served: &Served,
    engine: &mut EnginePlaced,
) -> Result<()> {
    let mut agnostic: BTreeMap<&str, &Entry> = BTreeMap::new();
    let mut by_language: BTreeMap<&str, BTreeMap<&str, &Entry>> = BTreeMap::new();
    for ((scope, _), cell) in cells {
        for (id, entry) in &cell.rules {
            let slot = match scope {
                None => &mut agnostic,
                Some(language) => by_language.entry(language.as_str()).or_default(),
            };
            if let Some(previous) = slot.insert(id.as_str(), entry)
                && previous.canon != entry.canon
            {
                bail!("engine `{engine_name}`: rule `{id}` is set differently under [lint] and [fmt]; keep one");
            }
        }
    }
    let ids: BTreeSet<&str> = agnostic
        .keys()
        .copied()
        .chain(by_language.values().flat_map(|m| m.keys().copied()))
        .collect();
    for id in ids {
        let carrying: BTreeMap<&str, &Entry> = by_language
            .iter()
            .filter_map(|(language, rules)| rules.get(id).map(|entry| (*language, *entry)))
            .collect();
        let serving = if served.0.is_empty() { &served.1 } else { &served.0 };
        let served_entries: Vec<&Entry> = serving
            .iter()
            .filter_map(|language| carrying.get(language.as_str()).copied())
            .collect();
        let hoist = !agnostic.contains_key(id)
            && !serving.is_empty()
            && served_entries.len() == serving.len()
            && all_equal(served_entries.iter().copied());
        if hoist {
            engine
                .rules
                .insert(id.to_string(), merge(served_entries.iter().copied()));
        }
        if let Some(entry) = agnostic.get(id) {
            engine.rules.insert(id.to_string(), (*entry).clone());
        }
        for (language, entry) in carrying {
            if hoist && serving.iter().any(|s| s.as_str() == language) {
                continue;
            }
            let target = engine.langs.entry(language.to_string()).or_default();
            target.rules.insert(id.to_string(), entry.clone());
        }
    }
    Ok(())
}

fn all_equal<'a>(mut entries: impl Iterator<Item = &'a Entry>) -> bool {
    let Some(first) = entries.next() else {
        return true;
    };
    entries.all(|entry| entry.canon == first.canon)
}

/// One entry standing for several equal sources: the first value, every distinct comment, the
/// earliest rank.
fn merge<'a>(sources: impl IntoIterator<Item = &'a Entry>) -> Entry {
    let mut iter = sources.into_iter();
    let first = iter.next().expect("merge needs at least one entry");
    let mut merged = first.clone();
    for other in iter {
        for comment in &other.comments {
            if !merged.comments.contains(comment) {
                merged.comments.push(comment.clone());
            }
        }
        merged.order = merged.order.min(other.order);
    }
    merged
}

fn note_language_values(name: &str, engine: &EnginePlaced, notes: &mut Vec<String>) {
    for (language, placed) in &engine.langs {
        let mut keys: BTreeSet<&str> = placed.options.keys().map(String::as_str).collect();
        keys.extend(placed.phases.values().flat_map(|o| o.keys().map(String::as_str)));
        keys.extend(placed.rules.keys().map(|id| id.as_str()));
        if keys.is_empty() {
            continue;
        }
        let list: Vec<&str> = keys.into_iter().collect();
        notes.push(format!(
            "engines.{name}.lang.{language}: {} stayed language-specific (the v1 values differ across languages, or \
             the engine spans languages that had no table)",
            list.join(", ")
        ));
    }
}
