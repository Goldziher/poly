//! Static validation of a `poly.toml`: whether its engine tables, option keys and component
//! selection are well formed, without loading any engine or component.
//!
//! The keys an engine reads come from the compiled component index
//! ([`poly_component_index::index`]), so the verdict is the same in a core-only binary, with
//! nothing installed, and in a build that links every engine. Keys a component can only recognise
//! by running its own serde probe cannot be judged statically; they are accepted and the component
//! is reported in [`ValidationReport::partial`]. Nothing here downloads or loads a component.

use std::collections::{BTreeMap, BTreeSet};

use poly_component_index::index;
use poly_component_protocol::WireOptionTable;
use poly_config::v2::{BUILTIN_PACK_KEY, Enable, EngineTable, PhaseTable, PolyConfigV2, RULE_DIRS_KEY, detect_v1};

use crate::engine::{Engine, OptionTable, OptionType};
use crate::engines::config_keys::{self, StaticKeys, edit_distance, near_miss_key};
use crate::registry::{all_languages, engines_for};

/// One reason a config is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProblem {
    /// Dotted path of the offending table or key, e.g. `engines.mago.lint`; empty for the document.
    pub path: String,
    /// The offending key inside `path`, when the problem is about one key.
    pub key: Option<String>,
    /// What is wrong, with a suggestion when one is known.
    pub message: String,
}

/// A component some of whose keys could not be judged because they are derived at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialValidation {
    /// Component (package) name.
    pub component: String,
    /// The configured engines of that component whose keys were only partly checked.
    pub engines: Vec<String>,
}

/// The outcome of validating one config.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationReport {
    /// Every problem found; empty when the config is valid.
    pub problems: Vec<ConfigProblem>,
    /// Components validated only partially.
    pub partial: Vec<PartialValidation>,
}

impl ValidationReport {
    /// Whether no problem was found.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Validate the text of one `poly.toml`.
///
/// A v1 file (`[lint.*]` / `[fmt.*]` tables) is checked with the v1 option-key check; a v2 file
/// (`[engines.*]`, `[components]`) is checked table by table.
#[must_use]
pub fn validate_source(source: &str) -> ValidationReport {
    let mut report = ValidationReport::default();
    let table = match source.parse::<toml::Table>() {
        Ok(table) => table,
        Err(error) => {
            report.problems.push(problem("", format!("not valid TOML: {error}")));
            return report;
        }
    };
    if !detect_v1(&table).is_empty() {
        for diagnostic in config_keys::check_source(source) {
            let line = diagnostic
                .span
                .map(|span| format!("line {}: ", span.start_line))
                .unwrap_or_default();
            let detail = diagnostic.description.map(|d| format!(" {d}")).unwrap_or_default();
            report
                .problems
                .push(problem("", format!("{line}{}{detail}", diagnostic.title)));
        }
        return report;
    }
    check_defaults(&table, &mut report);
    let config: PolyConfigV2 = match toml::from_str(source) {
        Ok(config) => config,
        Err(error) => {
            report.problems.push(problem("", error.message().to_string()));
            return report;
        }
    };
    let disabled = check_components(&config, &mut report);
    let mut partial: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, engine) in &config.engines {
        check_engine(id, engine, &disabled, &mut report, &mut partial);
    }
    report.partial = partial
        .into_iter()
        .map(|(component, engines)| PartialValidation {
            component,
            engines: engines.into_iter().collect(),
        })
        .collect();
    report
}

fn problem(path: &str, message: String) -> ConfigProblem {
    ConfigProblem {
        path: path.to_string(),
        key: None,
        message,
    }
}

fn key_problem(path: &str, key: &str, message: String) -> ConfigProblem {
    ConfigProblem {
        key: Some(key.to_string()),
        ..problem(path, message)
    }
}

/// Report keys of `[defaults]` that the schema does not have.
fn check_defaults(table: &toml::Table, report: &mut ValidationReport) {
    let Some(defaults) = table.get("defaults").and_then(toml::Value::as_table) else {
        return;
    };
    for key in defaults.keys() {
        if poly_config::DEFAULTS_KEYS.contains(&key.as_str()) {
            continue;
        }
        let known: Vec<String> = poly_config::DEFAULTS_KEYS.iter().map(|k| (*k).to_string()).collect();
        let hint = near_miss_key(key, &known)
            .map(|suggestion| format!(" Did you mean `{suggestion}`?"))
            .unwrap_or_default();
        report.problems.push(key_problem(
            "defaults",
            key,
            format!(
                "unknown option `{key}` in `[defaults]`: poly does not read it.{hint} Keys read: {}.",
                known.join(", ")
            ),
        ));
    }
}

/// Check `[components]` names against the index; returns the component names `disable` removes.
fn check_components(config: &PolyConfigV2, report: &mut ValidationReport) -> BTreeSet<String> {
    let index = index();
    let mut disabled = BTreeSet::new();
    let enabled: &[String] = match &config.components.enable {
        Enable::Auto => &[],
        Enable::List(list) => list,
    };
    for (path, names) in [
        ("components.enable", enabled),
        ("components.disable", config.components.disable.as_slice()),
    ] {
        for name in names {
            match index.expand_preset(name) {
                Some(components) if path == "components.disable" => {
                    disabled.extend(components.iter().map(|c| c.name.clone()));
                }
                Some(_) => {}
                None => report
                    .problems
                    .push(problem(path, format!("unknown component or preset `{name}`"))),
            }
        }
    }
    disabled
}

/// What a config may set on one engine, gathered from the index or, for the core's own engines,
/// from the linked backend.
struct EngineKeys {
    /// Hosting component; `None` for an engine the core always links.
    component: Option<String>,
    lint: Vec<StaticKeys>,
    format: Vec<StaticKeys>,
    /// Per-language tables accept `indent_width`/`rules`; a language-agnostic engine does not.
    universal: bool,
}

fn engine_keys(id: &str) -> Option<EngineKeys> {
    let index = index();
    let descs: Vec<_> = index.engine_descs(id).collect();
    if !descs.is_empty() {
        // A bundled build links the engine, so its derived-key probes can run in-process.
        let linked = linked_engines()
            .into_iter()
            .find(|(name, _)| name == id)
            .map(|(_, e)| e);
        let table = |wanted: WireOptionTable| -> Vec<StaticKeys> {
            let local = linked.as_ref().map(|engine| {
                engine.option_keys(match wanted {
                    WireOptionTable::Format => OptionTable::Format,
                    WireOptionTable::CrossCuttingLint => OptionTable::CrossCuttingLint,
                    WireOptionTable::Lint => OptionTable::Lint,
                })
            });
            descs
                .iter()
                .filter_map(|desc| desc.option_keys.iter().find(|keys| keys.table == wanted))
                .map(|keys| StaticKeys::from_wire(keys).resolved_with(local.as_ref()))
                .collect()
        };
        return Some(EngineKeys {
            component: index.component_for_engine(id).map(|c| c.name.clone()),
            lint: table(WireOptionTable::Lint),
            format: table(WireOptionTable::Format),
            universal: descs.iter().any(|desc| !desc.languages.is_empty()),
        });
    }
    let linked = linked_engines().into_iter().find(|(name, _)| name == id)?.1;
    let cross_cutting = linked.languages().is_empty();
    let lint_table = if cross_cutting {
        OptionTable::CrossCuttingLint
    } else {
        OptionTable::Lint
    };
    Some(EngineKeys {
        component: None,
        lint: vec![StaticKeys::from_linked(&linked.option_keys(lint_table))],
        format: vec![StaticKeys::from_linked(&linked.option_keys(OptionTable::Format))],
        universal: !cross_cutting,
    })
}

/// Every linked engine by name, one instance per name.
fn linked_engines() -> Vec<(String, Box<dyn Engine>)> {
    let mut seen = BTreeSet::new();
    let mut engines = Vec::new();
    for language in all_languages() {
        for engine in engines_for(&language) {
            if seen.insert(engine.name().to_string()) {
                engines.push((engine.name().to_string(), engine));
            }
        }
    }
    engines
}

/// A configured engine name one edit from a known one, when it is itself not known.
fn near_miss_engine_name(id: &str) -> Option<String> {
    let indexed = index()
        .components
        .iter()
        .flat_map(|c| c.engines.iter())
        .map(|desc| desc.name.clone());
    indexed
        .chain(linked_engines().into_iter().map(|(name, _)| name))
        .filter(|name| name.chars().count() >= 4)
        .find(|name| edit_distance(id, name) == 1)
}

fn check_engine(
    id: &str,
    engine: &EngineTable,
    disabled: &BTreeSet<String>,
    report: &mut ValidationReport,
    partial: &mut BTreeMap<String, BTreeSet<String>>,
) {
    let path = format!("engines.{id}");
    let Some(keys) = engine_keys(id) else {
        if let Some(suggestion) = near_miss_engine_name(id) {
            report.problems.push(problem(
                &path,
                format!("unknown engine `{id}`: did you mean `{suggestion}`?"),
            ));
        }
        return;
    };
    if let Some(component) = &keys.component
        && disabled.contains(component)
    {
        report.problems.push(problem(
            &path,
            format!(
                "engine `{id}` is configured, but its component `{component}` is disabled by `[components] disable`"
            ),
        ));
    }
    let mut checker = Checker {
        id,
        keys: &keys,
        report,
        partial,
    };
    let both: Vec<&StaticKeys> = keys.lint.iter().chain(&keys.format).collect();
    checker.options(&path, &engine.options, &both);
    checker.phases(&path, engine.lint.as_ref(), engine.format.as_ref());
    for (language, table) in &engine.lang {
        let lang_path = format!("{path}.lang.{language}");
        checker.options(&lang_path, &table.options, &both);
        checker.phases(&lang_path, table.lint.as_ref(), table.format.as_ref());
    }
}

struct Checker<'a> {
    id: &'a str,
    keys: &'a EngineKeys,
    report: &'a mut ValidationReport,
    partial: &'a mut BTreeMap<String, BTreeSet<String>>,
}

impl Checker<'_> {
    fn phases(&mut self, path: &str, lint: Option<&PhaseTable>, format: Option<&PhaseTable>) {
        let keys = self.keys;
        if let Some(table) = lint {
            let lint_keys: Vec<&StaticKeys> = keys.lint.iter().collect();
            self.options(&format!("{path}.lint"), &table.options, &lint_keys);
        }
        if let Some(table) = format {
            let format_keys: Vec<&StaticKeys> = keys.format.iter().collect();
            self.options(&format!("{path}.format"), &table.options, &format_keys);
        }
    }

    /// Check each key of `options` against the tables `keys` of the engine: accepted when any
    /// table accepts it at a usable type.
    fn options(&mut self, path: &str, options: &toml::Table, keys: &[&StaticKeys]) {
        let universal = self.keys.universal;
        for (key, value) in options {
            if self.id == "astgrep" && (key == RULE_DIRS_KEY || key == BUILTIN_PACK_KEY) {
                continue;
            }
            if keys.iter().any(|k| !k.is_checked()) {
                continue;
            }
            let usable = keys
                .iter()
                .any(|k| k.expected_type(key, universal).is_some() && k.value_problem(key, value, universal).is_none());
            if usable {
                continue;
            }
            if let Some(expected) = keys.iter().find_map(|k| k.expected_type(key, universal)) {
                let expected = expected.describe();
                let found = OptionType::describe_value(value);
                self.report.problems.push(key_problem(
                    path,
                    key,
                    format!("invalid value for `{key}` in `[{path}]`: expected {expected}, found {found}"),
                ));
                continue;
            }
            if keys.iter().any(|k| k.has_unresolved_derived()) {
                if let Some(component) = &self.keys.component {
                    self.partial
                        .entry(component.clone())
                        .or_default()
                        .insert(self.id.to_string());
                }
                continue;
            }
            if keys.iter().any(|k| k.accepts(key, options, universal)) {
                continue;
            }
            let mut known: Vec<String> = keys.iter().flat_map(|k| k.known_keys(universal)).collect();
            known.sort_unstable();
            known.dedup();
            let hint = near_miss_key(key, &known)
                .map(|suggestion| format!(" Did you mean `{suggestion}`?"))
                .unwrap_or_default();
            let note = keys
                .iter()
                .find_map(|k| k.note())
                .map(|note| format!(" {note}"))
                .unwrap_or_default();
            self.report.problems.push(key_problem(
                path,
                key,
                format!(
                    "unknown option `{key}` in `[{path}]`: the {} engine does not read it.{hint} Keys read: {}.{note}",
                    self.id,
                    known.join(", ")
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests;
