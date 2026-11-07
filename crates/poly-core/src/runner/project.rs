//! The project-check phase: whole-project analysis that runs once per project after the per-file
//! tier (lint only).
//!
//! An engine that declares [`Capabilities::project`](crate::engine::Capabilities::project) is
//! handed every discovered file it serves, with contents, plus the toolchain environment the host
//! resolved ([`crate::project::env`]); it answers with findings grouped by file. The findings are
//! then folded into the per-file results and pass through the same inline `poly: allow`
//! suppression and `[per-file-ignores]` filters as per-file diagnostics, so a report cannot tell
//! the two apart.
//!
//! An engine that declares directories in [`Engine::project_inputs`](crate::engine::Engine::project_inputs)
//! is not handed the run's file set but the tree under those directories, so its verdict does not
//! depend on which paths the run named; only the findings it reports do.
//!
//! Results are cached under [`Namespace::Lint`] with the engine id `project:<engine>`, keyed on
//! the engine version, a fingerprint of its resolved config, the host environment, and the sorted
//! `(path, content digest)` list of the files it saw — any change to one file, the set, the config
//! or the interpreter re-runs the whole check, and nothing else does.

mod fixes;
mod inputs;
mod merge;
mod scope;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use poly_cache::{Namespace, ResultCache};
use poly_component_index::ComponentIndex;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use super::plan::build_severity_remap;
use super::selection::EngineSelection;
use super::skips::{SkippedFile, is_lock_file};
use super::types::{LintError, LintResult};
use crate::components::ResolvedComponents;
use crate::config::{EngineConfig, Kind};
use crate::discover::DiscoveredFile;
use crate::engine::{
    Diagnostic, ENABLED_OPTION_KEY, Engine, ProjectEnvironment, ProjectFile, ProjectRequest, WireSelection,
};
use crate::engine_guard::guard_engine_panic;
use crate::filter::{PerFileIgnores, is_binary, is_generated_source};
use crate::language::Language;
use crate::registry::engines_for;
use crate::resolve::ConfigSet;
use merge::{fold, merge_findings};

/// Engines a test registered on top of the compiled-in registry; see
/// [`with_extra_project_engines`].
static EXTRA_ENGINES: Mutex<Vec<Arc<dyn Engine>>> = Mutex::new(Vec::new());

/// Serialises callers of [`with_extra_project_engines`], whose registration is process-global.
static EXTRA_ENGINES_SERIAL: Mutex<()> = Mutex::new(());

/// Run `run` with `engines` registered as additional project-check engines.
///
/// A test seam: no compiled-in engine declares the `project` capability yet, and the registry is
/// a static `match`, so a synthetic engine has no other way into a run. Registration is
/// process-global, so concurrent callers are serialised and the registrations are removed again
/// when `run` returns or unwinds. Not part of the supported API.
#[doc(hidden)]
pub fn with_extra_project_engines<R>(engines: Vec<Arc<dyn Engine>>, run: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            EXTRA_ENGINES.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }
    }
    let _serial = EXTRA_ENGINES_SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    *EXTRA_ENGINES.lock().unwrap_or_else(PoisonError::into_inner) = engines;
    let _reset = Reset;
    run()
}

/// One project-check engine, resolved for one config scope.
struct ProjectPlan {
    engine: Arc<dyn Engine>,
    config_id: usize,
    config: EngineConfig,
    /// The language the config was resolved against, which resolves the same engine config for any
    /// other config scope a whole-tree plan reports on.
    language: Language,
    /// Planned for a config that owns none of the run's files but sits above a path it named.
    ancestor: bool,
}

/// A project engine's findings for one file, before suppression.
#[derive(Clone)]
pub(super) struct ProjectFinding {
    path: PathBuf,
    config_id: usize,
    /// The engine that reported it.
    engine: &'static str,
    diagnostics: Vec<Diagnostic>,
    /// The content the engine was given for `path`, which its edits are offsets into. `None` for a file
    /// it was not given (or was given by name only).
    loaded: Option<Arc<str>>,
}

/// Whether `--fix` applies the edits project findings carry, and whether it may rewrite a file that
/// announces itself as machine-generated.
#[derive(Debug, Clone, Copy)]
pub(super) struct FixMode {
    pub(super) apply: bool,
    pub(super) generated: bool,
}

/// What the phase produced.
#[derive(Default)]
pub(super) struct ProjectOutcome {
    /// Findings, one entry per file an engine reported on.
    pub(super) findings: Vec<ProjectFinding>,
    /// Engines whose check failed outright.
    pub(super) errors: Vec<LintError>,
    /// Per engine that ran, the files it was handed.
    pub(super) reads: Vec<(&'static str, Vec<PathBuf>)>,
    /// Selections the run asked for that left nothing to run, charged to the skip budget.
    pub(super) skips: Vec<SkippedFile>,
}

impl ProjectOutcome {
    /// Take the errors and the counted skips, leaving the findings to merge.
    pub(super) fn take_reports(&mut self) -> (Vec<LintError>, Vec<SkippedFile>) {
        (std::mem::take(&mut self.errors), std::mem::take(&mut self.skips))
    }
}

/// Whether `engine` is switched on in `config`: not `enabled = false`, and past its gate key.
fn is_active(engine: &dyn Engine, config: &EngineConfig) -> bool {
    if config.options.get(ENABLED_OPTION_KEY).and_then(toml::Value::as_bool) == Some(false) {
        return false;
    }
    !engine
        .project_inputs()
        .gate_key
        .is_some_and(|key| !config.options.contains_key(&key))
}

fn is_whole_tree(engine: &dyn Engine) -> bool {
    engine.project_inputs().dirs.is_some_and(|dirs| dirs.whole_tree)
}

/// The configs that own none of the run's files but sit at or above a path it named, nearest-config
/// resolved so a directory that two configs claim counts once.
///
/// `poly lint packages/a` never discovers a file of the root config, yet the root config is what
/// decides a whole-tree check about the workspace `packages/a` is part of.
pub(super) fn ancestor_configs(configs: &ConfigSet, paths: &[PathBuf]) -> Vec<usize> {
    let named: Vec<PathBuf> = paths
        .iter()
        .map(|path| {
            let dir = if path.is_dir() {
                Some(path.as_path())
            } else {
                path.parent()
            };
            let dir = dir.filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
            scope::canonical(dir)
        })
        .collect();
    (0..configs.len())
        .filter(|&id| {
            let Some(dir) = configs.config_dir(id) else {
                return false;
            };
            configs.config_id_for(&dir.join("_")) == id && {
                let dir = scope::canonical(dir);
                named.iter().any(|named| named.starts_with(&dir))
            }
        })
        .collect()
}

/// The project engines that apply to this run: those that declare the capability, are not
/// switched off with `enabled = false`, and survive `--only` / `--skip`. They are the compiled-in
/// ones and the downloadable components' (`components`).
///
/// One plan per `(config, engine)`. The engine's config is resolved against the first language
/// (by id) it serves in that config scope, which is the same table the per-file planner reads.
/// A whole-tree engine is also planned for the configs above the named `paths`, whether or not they
/// own a file of the run.
fn plan_project_engines(
    files: &[DiscoveredFile],
    configs: &ConfigSet,
    selection: EngineSelection<'_>,
    components: &ResolvedComponents,
    paths: &[PathBuf],
    index: &ComponentIndex,
) -> (Vec<ProjectPlan>, Vec<LintError>, Vec<SkippedFile>) {
    let mut scopes: Vec<(usize, Language)> = files
        .iter()
        .map(|f| (f.config_id, f.language.clone()))
        .collect::<FxHashSet<_>>()
        .into_iter()
        .collect();
    let owning: FxHashSet<usize> = scopes.iter().map(|(id, _)| *id).collect();
    let languages: FxHashSet<Language> = scopes.iter().map(|(_, language)| language.clone()).collect();
    for id in ancestor_configs(configs, paths)
        .into_iter()
        .filter(|id| !owning.contains(id))
    {
        scopes.extend(languages.iter().map(|language| (id, language.clone())));
    }
    scopes.sort_by(|a, b| (a.0, a.1.id()).cmp(&(b.0, b.1.id())));
    let extras: Vec<Arc<dyn Engine>> = EXTRA_ENGINES.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut seen: FxHashSet<(usize, &'static str)> = FxHashSet::default();
    let mut plans = Vec::new();
    let mut errors = Vec::new();
    let mut skips = Vec::new();
    let mut errored: FxHashSet<(usize, String)> = FxHashSet::default();
    let mut checked_tables: FxHashSet<(usize, String)> = FxHashSet::default();
    for (config_id, language) in &scopes {
        let ancestor = !owning.contains(config_id);
        let registered = engines_for(language)
            .into_iter()
            .filter(|engine| engine.capabilities().project)
            .map(Arc::<dyn Engine>::from);
        let from_components = components
            .engines(*config_id, language)
            .iter()
            .filter(|engine| engine.capabilities().project)
            .map(|engine| Arc::new(engine.clone()) as Arc<dyn Engine>);
        let injected = extras
            .iter()
            .filter(|engine| {
                let served = engine.languages();
                served.is_empty() || served.contains(language)
            })
            .cloned();
        let candidates: Vec<Arc<dyn Engine>> = registered.chain(from_components).chain(injected).collect();
        let declared_here: Vec<WireSelection> = candidates.iter().filter_map(|engine| engine.selection()).collect();
        for engine in candidates {
            let declared = engine.selection();
            let table = declared.as_ref().map_or_else(|| engine.name(), |s| s.table.as_str());
            if !selection.selects_declared(engine.name(), Some(table))
                || (ancestor && !is_whole_tree(&*engine))
                || !seen.insert((*config_id, engine.name()))
            {
                continue;
            }
            let config = configs.config(*config_id).engine_config(language, table, Kind::Lint);
            if config.options.get(ENABLED_OPTION_KEY).and_then(toml::Value::as_bool) == Some(false) {
                continue;
            }
            if !is_active(&*engine, &config) {
                continue;
            }
            if let Some(declared) = &declared {
                match picked(declared, &config.options, &declared_here, index) {
                    Pick::Mine => {}
                    Pick::Other => continue,
                    Pick::Unavailable(value) => {
                        errored.insert((*config_id, table.to_owned()));
                        errors.extend(unavailable_backend(
                            (&declared.table, &declared.key),
                            &value,
                            selection,
                            (*config_id, configs),
                            index,
                        ));
                        continue;
                    }
                }
            }
            plans.push(ProjectPlan {
                engine,
                config_id: *config_id,
                config,
                language: language.clone(),
                ancestor,
            });
        }
        for (table, key) in declared_tables(index, language) {
            if ancestor || !checked_tables.insert((*config_id, table.clone())) {
                continue;
            }
            let config = configs.config(*config_id).engine_config(language, &table, Kind::Lint);
            if config.options.get(ENABLED_OPTION_KEY).and_then(toml::Value::as_bool) == Some(false) {
                continue;
            }
            let served = plans
                .iter()
                .filter(|plan| plan.config_id == *config_id)
                .any(|plan| plan.engine.selection().is_some_and(|s| s.table == table));
            if served || errored.contains(&(*config_id, table.clone())) {
                continue;
            }
            let value = config.options.get(&key).and_then(toml::Value::as_str);
            if let Some(value) = value
                && let Some(error) = unavailable_backend((&table, &key), value, selection, (*config_id, configs), index)
            {
                errors.push(error);
                continue;
            }
            skips.extend(only_selects_nothing(
                (&table, &key),
                value,
                selection,
                (*config_id, configs),
                index,
            ));
        }
    }
    plans.sort_by(|a, b| (a.config_id, a.engine.name()).cmp(&(b.config_id, b.engine.name())));
    (plans, errors, skips)
}

/// The `(table, key)` pairs of the selections that engines of `index` serving `language` declare, sorted.
fn declared_tables(index: &ComponentIndex, language: &Language) -> Vec<(String, String)> {
    let mut tables: Vec<(String, String)> = index
        .components
        .iter()
        .flat_map(|component| component.engines.iter())
        .filter(|engine| engine.languages.iter().any(|l| l == language.id()))
        .filter_map(|engine| engine.selection.as_ref())
        .map(|s| (s.table.clone(), s.key.clone()))
        .collect();
    tables.sort();
    tables.dedup();
    tables
}

/// The counted skip for a run whose `--only` names a table, or an engine that shares it, though the config
/// leaves that table with no engine to run.
///
/// `--only` narrows by design, so this is not an error; but a run that was asked for a type check and ran none
/// must say so, and being a skip outside the withdrawal reasons it is charged to `--deny-skips`.
fn only_selects_nothing(
    (table, key): (&str, &str),
    value: Option<&str>,
    selection: EngineSelection<'_>,
    (config_id, configs): (usize, &ConfigSet),
    index: &ComponentIndex,
) -> Option<SkippedFile> {
    let names = |entry: &String| {
        entry == table
            || index
                .engine_descs(entry)
                .any(|desc| desc.selection.as_ref().is_some_and(|s| s.table == table))
    };
    let asked = selection.only().iter().find(|entry| names(entry))?;
    if selection.skip().iter().any(names) {
        return None;
    }
    let current = value.map_or_else(|| "unset".to_owned(), |v| format!("\"{v}\""));
    Some(SkippedFile {
        path: root_for(configs, config_id),
        reason: format!("`--only {asked}` selects nothing here: `[{table}] {key}` is {current}"),
    })
}

/// What the table options say about the engine that declared a selection.
enum Pick {
    /// The engine's own `value`, or a value no engine declares and the engine is the default (it then reports
    /// that value as unknown).
    Mine,
    /// Another candidate's value, or no value: engines that share a table are opt-in.
    Other,
    /// A value an engine of the component index declares, though no candidate in this run does: the engine is
    /// not available here, and the default engine must not stand down silently.
    Unavailable(String),
}

/// Which engine the table options pick, for the engine that declared `selection`.
///
/// A present value that is not a string (`backend = 1`) is a value no engine declares, so the default engine
/// is picked and reports it.
fn picked(
    selection: &WireSelection,
    options: &toml::Table,
    siblings: &[WireSelection],
    index: &ComponentIndex,
) -> Pick {
    let Some(value) = options.get(&selection.key) else {
        return Pick::Other;
    };
    let chosen = value.as_str();
    if chosen == Some(selection.value.as_str()) {
        return Pick::Mine;
    }
    if !selection.default {
        return Pick::Other;
    }
    let Some(chosen) = chosen else {
        return Pick::Mine;
    };
    let claimed = |other: &WireSelection| other.table == selection.table && other.key == selection.key;
    if siblings.iter().any(|other| claimed(other) && other.value == chosen) {
        return Pick::Other;
    }
    if index.claims_selection(&selection.table, &selection.key, chosen) {
        return Pick::Unavailable(chosen.to_owned());
    }
    Pick::Mine
}

/// The error for a backend the config names but this run has no engine for, when the run asked for it.
///
/// The component hosting it is disabled (`[components] disable`) or not part of this build; either way the run
/// would otherwise type-check nothing and read as clean. A run that deselected the backend with `--skip`, or
/// that `--only` leaves it out of, asked for that and gets no error.
fn unavailable_backend(
    (table, key): (&str, &str),
    value: &str,
    selection: EngineSelection<'_>,
    (config_id, configs): (usize, &ConfigSet),
    index: &ComponentIndex,
) -> Option<LintError> {
    let (component, engine) = index.claiming_component(table, key, value)?;
    if !selection.selects_declared(&engine.name, Some(table)) {
        return None;
    }
    Some(LintError {
        path: root_for(configs, config_id),
        message: format!(
            "`{key} = \"{value}\"` in `[{table}]` needs the `{engine}` engine of the `{component}` component, which is \
             not available in this run: it is disabled (remove `{component}` from `[components] disable`) or this \
             build does not include it (use a build that does, or set `{key}` to another backend)",
            key = key,
            table = table,
            engine = engine.name,
            component = component.name,
        ),
    })
}

/// The plan, rooted above plan `at`, of the same whole-tree engine whose directories contain `at`'s root,
/// if there is one.
///
/// A nested `poly.toml` makes its own scope, and so its own plan; a check about the whole tree walked
/// from the nested directory would judge a fragment of the graph the outer plan judges whole, and
/// report the same files twice. An outer plan that does not reach the nested root (its `dir` names a
/// sibling) leaves the nested plan to run.
fn covering_plan(plans: &[ProjectPlan], roots: &[PathBuf], at: usize) -> Option<usize> {
    if !is_whole_tree(&*plans[at].engine) {
        return None;
    }
    (0..plans.len()).find(|&other| {
        other != at
            && plans[other].engine.name() == plans[at].engine.name()
            && roots[at] != roots[other]
            && roots[at].starts_with(&roots[other])
            && inputs::plan_dirs(&plans[other])
                .ok()
                .flatten()
                .is_some_and(|dirs| dirs.absolute(&roots[other]).any(|dir| roots[at].starts_with(dir)))
    })
}

/// A warning on the nested `poly.toml` of plan `at` when it sets analysis options the covering plan
/// overrides, `None` when the two agree (a nested config inherits its parent's table, so they usually do).
fn ignored_options_note(plans: &[ProjectPlan], roots: &[PathBuf], at: usize, outer: usize) -> Option<ProjectFinding> {
    let analysis = |plan: &ProjectPlan| {
        let mut options = plan.config.options.clone();
        options.remove(ENABLED_OPTION_KEY);
        options.remove("rules");
        options
    };
    if analysis(&plans[at]) == analysis(&plans[outer]) {
        return None;
    }
    let engine = plans[at].engine.name();
    let diagnostic = Diagnostic {
        engine: engine.to_owned(),
        code: Some(NESTED_OPTIONS_CODE.to_owned()),
        severity: crate::engine::Severity::Warning,
        title: format!(
            "`{engine}` options set here are ignored: the check runs once over the tree of {}, with that config's options",
            roots[outer].display()
        ),
        description: Some(
            "Only `enabled` and the per-rule levels of a nested config apply to its own files.".to_owned(),
        ),
        span: None,
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    };
    Some(ProjectFinding {
        path: roots[at].join(poly_config::CONFIG_FILE_NAMES[0]),
        config_id: plans[at].config_id,
        engine,
        diagnostics: vec![diagnostic],
        loaded: None,
    })
}

/// Rule code of the warning a nested config gets when its options are overridden by the covering plan.
const NESTED_OPTIONS_CODE: &str = "nested-options-ignored";

/// The project root for a config scope: the directory holding its `poly.toml`, else the run root,
/// else the working directory.
fn root_for(configs: &ConfigSet, config_id: usize) -> PathBuf {
    configs
        .config_dir(config_id)
        .or_else(|| configs.root_dir())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Read `files` for a project check, passing over what the per-file tier passes over: lock files,
/// binaries, text that is not UTF-8 and (when the config opts out) generated sources.
fn load_files(files: &[&DiscoveredFile], act_on_generated: &[bool]) -> Vec<ProjectFile> {
    let mut loaded: Vec<ProjectFile> = files
        .par_iter()
        .filter(|f| !is_lock_file(&f.path))
        .filter_map(|f| {
            let bytes = std::fs::read(&f.path)
                .inspect_err(|error| tracing::warn!(path = %f.path.display(), %error, "project check: unreadable file"))
                .ok()?;
            if is_binary(&bytes) {
                return None;
            }
            let content = String::from_utf8(bytes).ok()?;
            if !act_on_generated[f.config_id] && is_generated_source(&content) {
                return None;
            }
            Some(ProjectFile {
                path: f.path.clone(),
                language: f.language.clone(),
                content: Arc::from(content),
            })
        })
        .collect();
    loaded.sort_by(|a, b| a.path.cmp(&b.path));
    loaded
}

/// The cache key for one `(engine, config, files, environment)` combination.
fn cache_key(
    plan: &ProjectPlan,
    root: &Path,
    files: &[ProjectFile],
    environment: &ProjectEnvironment,
) -> poly_cache::CacheKey {
    let mut args = toml::Table::new();
    let fingerprint = plan
        .config
        .fingerprint()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    args.insert("__config".to_string(), toml::Value::String(fingerprint));
    args.insert(
        "__root".to_string(),
        toml::Value::String(root.to_string_lossy().into_owned()),
    );
    let env_pairs: Vec<String> = environment
        .to_wire_pairs()
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    args.insert("__environment".to_string(), toml::Value::String(env_pairs.join("\0")));
    let names: Vec<std::borrow::Cow<'_, str>> = files.iter().map(|f| f.path.to_string_lossy()).collect();
    let digest = ResultCache::file_set_digest(
        names
            .iter()
            .zip(files)
            .map(|(name, file)| (&**name, file.content.as_bytes())),
    );
    ResultCache::key(
        Namespace::Lint,
        &format!("project:{}", plan.engine.name()),
        plan.engine.version(),
        &args,
        &digest,
    )
}

/// What the run was given, beyond the discovered files: the configs, whether each acts on generated
/// sources, and the paths and `--exclude` globs. An engine that declares directories reads its own
/// tree whatever the paths are (see [`inputs`]); its findings are then narrowed to what they name.
pub(super) struct RunInputs<'a> {
    pub(super) configs: &'a ConfigSet,
    pub(super) act_on_generated: &'a [bool],
    pub(super) paths: &'a [PathBuf],
    pub(super) exclude: &'a [String],
    /// `[per-file-ignores]` per config, and the directories their globs are relative to.
    pub(super) ignores: &'a [PerFileIgnores],
    pub(super) ignore_bases: &'a [Vec<PathBuf>],
    /// The component index the run resolved its engines against.
    pub(super) index: &'a ComponentIndex,
}

impl<'a> RunInputs<'a> {
    pub(super) fn new(
        configs: &'a ConfigSet,
        act_on_generated: &'a [bool],
        paths: &'a [PathBuf],
        opts: &'a crate::RunOptions,
        filters: (&'a [PerFileIgnores], &'a [Vec<PathBuf>]),
    ) -> Self {
        Self {
            configs,
            act_on_generated,
            paths,
            exclude: &opts.exclude,
            ignores: filters.0,
            ignore_bases: filters.1,
            index: crate::components::run_index(opts),
        }
    }
}

/// An engine's findings for `loaded`, from the cache when it holds them, else by running the check.
fn report_for(
    plan: &ProjectPlan,
    root: &Path,
    loaded: &[ProjectFile],
    environment: &ProjectEnvironment,
    cache: &ResultCache,
) -> Result<Vec<(PathBuf, Vec<Diagnostic>)>, LintError> {
    let key = cache.enabled().then(|| cache_key(plan, root, loaded, environment));
    let cached = key
        .as_ref()
        .and_then(|key| cache.get(Namespace::Lint, key))
        .and_then(|bytes| serde_json::from_slice::<Vec<(PathBuf, Vec<Diagnostic>)>>(&bytes).ok());
    if let Some(reported) = cached {
        return Ok(reported);
    }
    let request = ProjectRequest {
        root,
        files: loaded,
        config: &plan.config,
        environment,
        threads: Some(rayon::current_num_threads()),
    };
    let reported =
        guard_engine_panic(plan.engine.name(), root, || plan.engine.project_check(&request)).map_err(|error| {
            LintError {
                path: root.to_path_buf(),
                message: format!("project check '{}' failed: {error:#}", plan.engine.name()),
            }
        })?;
    if let Some(key) = &key
        && let Ok(bytes) = serde_json::to_vec(&reported)
        && let Err(error) = cache.put(Namespace::Lint, key, &bytes)
    {
        tracing::debug!(%error, "project check: result not cached");
    }
    Ok(reported)
}

/// Which config governs each file a whole-tree plan reports on, and how that config remaps severities.
struct Governing<'a> {
    plan: &'a ProjectPlan,
    configs: &'a ConfigSet,
    /// Per config id: its severity remap, or `None` when the engine is off there.
    remaps: FxHashMap<usize, Option<crate::filter::SeverityRemap>>,
}

impl<'a> Governing<'a> {
    fn new(plan: &'a ProjectPlan, configs: &'a ConfigSet) -> Self {
        Self {
            plan,
            configs,
            remaps: FxHashMap::default(),
        }
    }

    /// The config governing `path` and the remap of its engine table; `None` when that config switches
    /// the engine off, so a nested `enabled = false` silences its own subtree.
    fn of(&mut self, path: &Path) -> (usize, Option<&crate::filter::SeverityRemap>) {
        let id = self.configs.config_id_containing(path).unwrap_or_else(|| {
            // Outside every nested config: the root config governs what sits under the root, and a
            // file elsewhere (an engine `dir` pointing out of the tree) takes the plan's own config.
            match self.configs.config_dir(0) {
                Some(root) if path.starts_with(root) => 0,
                _ => self.plan.config_id,
            }
        });
        let (plan, configs) = (self.plan, self.configs);
        let remap = self.remaps.entry(id).or_insert_with(|| {
            let config = if id == plan.config_id {
                plan.config.clone()
            } else {
                configs
                    .config(id)
                    .engine_config(&plan.language, plan.engine.name(), Kind::Lint)
            };
            is_active(&*plan.engine, &config).then(|| build_severity_remap(&config))
        });
        (id, remap.as_ref())
    }
}

/// Turn an engine's `reported` findings into [`ProjectFinding`]s: severity remapped and, when
/// `narrowed_to` is set, only those on files the run named.
///
/// A whole-tree engine (`covered` is set) reports across config scopes; each finding then takes the
/// config governing its own file, so a nested `[per-file-ignores]`, rule level or `enabled = false`
/// applies to its own subtree.
fn collect_findings(
    plan: &ProjectPlan,
    configs: &ConfigSet,
    reported: Vec<(PathBuf, Vec<Diagnostic>)>,
    narrowed_to: Option<&scope::RunScope>,
    covered: Option<&FxHashSet<PathBuf>>,
    loaded: &[ProjectFile],
    findings: &mut Vec<ProjectFinding>,
) {
    let own_remap = build_severity_remap(&plan.config);
    let mut governing = Governing::new(plan, configs);
    for (path, mut diagnostics) in reported {
        if covered.is_some_and(|covered| !covered.contains(&scope::canonical(&path))) {
            continue;
        }
        let content = inputs::content_of(loaded, &path);
        // The engine spells a path as the config directory it walked does, which is how config
        // lookup is keyed; the admitted spelling is the run's own and serves display only.
        let governing_path = path.clone();
        let path = match narrowed_to {
            Some(run_scope) => match run_scope.admit(&path) {
                Some(admitted) => admitted,
                None => continue,
            },
            None => path,
        };
        let config_id = if covered.is_some() {
            match governing.of(&governing_path) {
                (_, None) => continue,
                (id, Some(remap)) => {
                    if !remap.is_empty() {
                        remap.apply(&mut diagnostics);
                    }
                    id
                }
            }
        } else {
            if !own_remap.is_empty() {
                own_remap.apply(&mut diagnostics);
            }
            plan.config_id
        };
        findings.push(ProjectFinding {
            path,
            config_id,
            engine: plan.engine.name(),
            diagnostics,
            loaded: content,
        });
    }
}

/// Run every applicable project engine over `files`; with `only`, just the engines it names.
pub(super) fn run_project_phase(
    files: &[DiscoveredFile],
    run: &RunInputs<'_>,
    selection: EngineSelection<'_>,
    cache: &ResultCache,
    components: &ResolvedComponents,
    only: Option<&FxHashSet<&'static str>>,
) -> ProjectOutcome {
    let (plans, unavailable, skips) =
        plan_project_engines(files, run.configs, selection, components, run.paths, run.index);
    let mut outcome = ProjectOutcome {
        errors: unavailable,
        skips,
        ..ProjectOutcome::default()
    };
    if plans.is_empty() {
        return outcome;
    }
    let run_scope = scope::RunScope::new(run.paths);
    let mut environments: FxHashMap<PathBuf, ProjectEnvironment> = FxHashMap::default();
    let roots: Vec<PathBuf> = plans.iter().map(|plan| root_for(run.configs, plan.config_id)).collect();
    for (at, plan) in plans.iter().enumerate() {
        if only.is_some_and(|only| !only.contains(plan.engine.name())) {
            continue;
        }
        if let Some(outer) = covering_plan(&plans, &roots, at) {
            outcome.findings.extend(ignored_options_note(&plans, &roots, at, outer));
            continue;
        }
        let served = plan.engine.languages();
        let scoped: Vec<&DiscoveredFile> = files
            .iter()
            .filter(|f| f.config_id == plan.config_id && (served.is_empty() || served.contains(&f.language)))
            .collect();
        if scoped.is_empty() && !plan.ancestor {
            continue;
        }
        let root = root_for(run.configs, plan.config_id);
        let dirs = match inputs::plan_dirs(plan) {
            Ok(dirs) => dirs,
            Err(message) => {
                outcome.errors.push(LintError {
                    path: root,
                    message: format!("project check '{}': {message}", plan.engine.name()),
                });
                continue;
            }
        };
        if let Some(dirs) = &dirs
            && !dirs.absolute(&root).any(|dir| run_scope.reaches(&dir))
        {
            continue;
        }
        let environment = environments
            .entry(root.clone())
            .or_insert_with(|| crate::project::env::resolve_environment(&root));
        let loaded = match inputs::load(plan, &root, dirs.as_ref(), &scoped, run) {
            Ok(loaded) => loaded,
            Err(message) => {
                outcome.errors.push(LintError {
                    path: root,
                    message: format!("project check '{}': {message}", plan.engine.name()),
                });
                continue;
            }
        };
        if !inputs::has_substance(&loaded) {
            continue;
        }
        let reported = match report_for(plan, &root, &loaded, environment, cache) {
            Ok(reported) => reported,
            Err(error) => {
                outcome.errors.push(error);
                continue;
            }
        };
        outcome.reads.push((
            plan.engine.name(),
            loaded.iter().map(|file| file.path.clone()).collect(),
        ));
        let narrowed = dirs.is_some().then_some(&run_scope);
        // A whole-tree check saw files the run's excludes drop; it reports only on the ones the run covers.
        let covered = dirs
            .as_ref()
            .is_some_and(|dirs| dirs.whole_tree)
            .then(|| scope::covered(files, &reported));
        collect_findings(
            plan,
            run.configs,
            reported,
            narrowed,
            covered.as_ref(),
            &loaded,
            &mut outcome.findings,
        );
    }
    outcome
}

/// Run the project phase and fold its findings into `linted`, returning the checks that failed and the
/// selections that left nothing to run.
///
/// Under `--fix` the edits are committed to disk, which moves every offset a remaining finding of the
/// same file carries and can expose findings the old content hid. So when a pass rewrote a file the
/// engines that reported on it run once more over the rewritten tree, and only that second pass is
/// reported for them: its findings have spans into the files as they now are, and the groups the first
/// pass deferred (they overlapped another edit) are tried again. An engine that was handed a rewritten
/// file runs again even with no finding in it, since a whole-tree verdict may depend on a file it never
/// reports on. Every other engine's first-pass findings stand: its input did not change, so running it
/// again would repeat the walk, the read and the hash for the same answer. It is the last pass, never a loop.
pub(super) fn run_and_merge(
    linted: &mut Vec<LintResult>,
    files: &[DiscoveredFile],
    run: &RunInputs<'_>,
    selection: EngineSelection<'_>,
    cache: &ResultCache,
    components: &ResolvedComponents,
    fix: FixMode,
) -> (Vec<LintError>, Vec<SkippedFile>) {
    let (ignores, ignore_bases) = (run.ignores, run.ignore_bases);
    let mut phase = run_project_phase(files, run, selection, cache, components, None);
    let (errors, skips) = phase.take_reports();
    if !fix.apply {
        merge_findings(linted, phase, ignores, ignore_bases, fix);
        return (errors, skips);
    }
    let first_findings = phase.findings.clone();
    let first_reads = std::mem::take(&mut phase.reads);
    let mut first_pass = Vec::new();
    merge_findings(&mut first_pass, phase, ignores, ignore_bases, fix);
    let rewritten: FxHashSet<&Path> = first_pass
        .iter()
        .filter(|result| result.fixed > 0)
        .map(|result| result.path.as_path())
        .collect();
    if rewritten.is_empty() {
        fold(linted, first_pass);
        return (errors, skips);
    }
    let rewritten_files: FxHashSet<PathBuf> = rewritten.iter().map(|path| scope::canonical(path)).collect();
    let mut rerun: FxHashSet<&'static str> = first_findings
        .iter()
        .filter(|finding| rewritten.contains(finding.path.as_path()))
        .map(|finding| finding.engine)
        .collect();
    rerun.extend(
        first_reads
            .iter()
            .filter(|(_, read)| scope::reads_any(read, &rewritten_files))
            .map(|(engine, _)| *engine),
    );
    // What the first pass fixed stays counted; what it still found is superseded by the second pass.
    for result in &mut first_pass {
        result.diagnostics.clear();
        result.suppressed.clear();
    }
    fold(linted, first_pass);
    let mut again = run_project_phase(files, run, selection, cache, components, Some(&rerun));
    let carried = first_findings
        .into_iter()
        .filter(|finding| !rerun.contains(finding.engine));
    again.findings.extend(carried.map(|mut finding| {
        // Their edits were offered to the first pass; none of them is applied by the second.
        for diagnostic in &mut finding.diagnostics {
            diagnostic.fix.clear();
        }
        finding
    }));
    // The second pass plans the same selection again, so its skips repeat the first pass's; they count once.
    let (again_errors, _) = again.take_reports();
    let errors = merge_errors(errors, again_errors);
    merge_findings(linted, again, ignores, ignore_bases, fix);
    (errors, skips)
}

/// `first` followed by what `second` adds: the same failure reported by both passes counts once.
fn merge_errors(mut first: Vec<LintError>, second: Vec<LintError>) -> Vec<LintError> {
    for error in second {
        if !first
            .iter()
            .any(|seen| seen.path == error.path && seen.message == error.message)
        {
            first.push(error);
        }
    }
    first
}

#[cfg(all(test, feature = "engine-python"))]
mod tests;
