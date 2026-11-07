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

mod inputs;
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
use crate::filter::{PerFileIgnores, Suppressions, is_binary, is_generated_source, relative_for_match};
use crate::language::Language;
use crate::registry::engines_for;
use crate::resolve::ConfigSet;
use crate::runner::SuppressedDiagnostic;

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
}

/// A project engine's findings for one file, before suppression.
pub(super) struct ProjectFinding {
    path: PathBuf,
    config_id: usize,
    diagnostics: Vec<Diagnostic>,
}

/// What the phase produced.
#[derive(Default)]
pub(super) struct ProjectOutcome {
    /// Findings, one entry per file an engine reported on.
    pub(super) findings: Vec<ProjectFinding>,
    /// Content of every file an engine was given, for resolving inline directives.
    contents: FxHashMap<PathBuf, Arc<str>>,
    /// Engines whose check failed outright.
    pub(super) errors: Vec<LintError>,
    /// Selections the run asked for that left nothing to run, charged to the skip budget.
    pub(super) skips: Vec<SkippedFile>,
}

impl ProjectOutcome {
    /// Take the errors and the counted skips, leaving the findings to merge.
    pub(super) fn take_reports(&mut self) -> (Vec<LintError>, Vec<SkippedFile>) {
        (std::mem::take(&mut self.errors), std::mem::take(&mut self.skips))
    }
}

/// The project engines that apply to this run: those that declare the capability, are not
/// switched off with `enabled = false`, and survive `--only` / `--skip`. They are the compiled-in
/// ones and the downloadable components' (`components`).
///
/// One plan per `(config, engine)`. The engine's config is resolved against the first language
/// (by id) it serves in that config scope, which is the same table the per-file planner reads.
fn plan_project_engines(
    files: &[DiscoveredFile],
    configs: &ConfigSet,
    selection: EngineSelection<'_>,
    components: &ResolvedComponents,
    index: &ComponentIndex,
) -> (Vec<ProjectPlan>, Vec<LintError>, Vec<SkippedFile>) {
    let mut scopes: Vec<(usize, Language)> = files
        .iter()
        .map(|f| (f.config_id, f.language.clone()))
        .collect::<FxHashSet<_>>()
        .into_iter()
        .collect();
    scopes.sort_by(|a, b| (a.0, a.1.id()).cmp(&(b.0, b.1.id())));
    let extras: Vec<Arc<dyn Engine>> = EXTRA_ENGINES.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut seen: FxHashSet<(usize, &'static str)> = FxHashSet::default();
    let mut plans = Vec::new();
    let mut errors = Vec::new();
    let mut skips = Vec::new();
    let mut errored: FxHashSet<(usize, String)> = FxHashSet::default();
    let mut checked_tables: FxHashSet<(usize, String)> = FxHashSet::default();
    for (config_id, language) in &scopes {
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
            if !selection.selects_declared(engine.name(), Some(table)) || !seen.insert((*config_id, engine.name())) {
                continue;
            }
            let config = configs.config(*config_id).engine_config(language, table, Kind::Lint);
            if config.options.get(ENABLED_OPTION_KEY).and_then(toml::Value::as_bool) == Some(false) {
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
            let gated_off = engine
                .project_inputs()
                .gate_key
                .is_some_and(|key| !config.options.contains_key(&key));
            if gated_off {
                continue;
            }
            plans.push(ProjectPlan {
                engine,
                config_id: *config_id,
                config,
            });
        }
        for (table, key) in declared_tables(index, language) {
            if !checked_tables.insert((*config_id, table.clone())) {
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
    /// The component index the run resolved its engines against.
    pub(super) index: &'a ComponentIndex,
}

impl<'a> RunInputs<'a> {
    pub(super) fn new(
        configs: &'a ConfigSet,
        act_on_generated: &'a [bool],
        paths: &'a [PathBuf],
        opts: &'a crate::RunOptions,
    ) -> Self {
        Self {
            configs,
            act_on_generated,
            paths,
            exclude: &opts.exclude,
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

/// Turn an engine's `reported` findings into [`ProjectFinding`]s: severity remapped, fixes dropped, and,
/// when `narrowed_to` is set, only those on files the run named.
fn collect_findings(
    plan: &ProjectPlan,
    reported: Vec<(PathBuf, Vec<Diagnostic>)>,
    narrowed_to: Option<&scope::RunScope>,
    findings: &mut Vec<ProjectFinding>,
) {
    let remap = build_severity_remap(&plan.config);
    for (path, mut diagnostics) in reported {
        let path = match narrowed_to {
            Some(run_scope) => match run_scope.admit(&path) {
                Some(admitted) => admitted,
                None => continue,
            },
            None => path,
        };
        if !remap.is_empty() {
            remap.apply(&mut diagnostics);
        }
        // The edits were computed against the engine's own view of the project and nothing
        // applies them, so advertising them as fixable would be a promise `--fix` cannot keep.
        for diagnostic in &mut diagnostics {
            diagnostic.fix.clear();
        }
        findings.push(ProjectFinding {
            path,
            config_id: plan.config_id,
            diagnostics,
        });
    }
}

/// Run every applicable project engine over `files`.
pub(super) fn run_project_phase(
    files: &[DiscoveredFile],
    run: &RunInputs<'_>,
    selection: EngineSelection<'_>,
    cache: &ResultCache,
    components: &ResolvedComponents,
) -> ProjectOutcome {
    let (plans, unavailable, skips) = plan_project_engines(files, run.configs, selection, components, run.index);
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
    for plan in &plans {
        let served = plan.engine.languages();
        let scoped: Vec<&DiscoveredFile> = files
            .iter()
            .filter(|f| f.config_id == plan.config_id && (served.is_empty() || served.contains(&f.language)))
            .collect();
        if scoped.is_empty() {
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
        let narrowed = dirs.is_some().then_some(&run_scope);
        collect_findings(plan, reported, narrowed, &mut outcome.findings);
        for file in loaded {
            outcome.contents.entry(file.path).or_insert(file.content);
        }
    }
    outcome
}

/// Fold project findings into the per-file results, applying inline `poly: allow` directives and
/// `[per-file-ignores]` exactly as the per-file tier does.
///
/// A finding for a file the per-file tier never produced a result for (it was skipped, or lies
/// outside the discovered set) gets a result of its own.
pub(super) fn merge_findings(
    linted: &mut Vec<LintResult>,
    outcome: ProjectOutcome,
    ignores: &[PerFileIgnores],
    ignore_bases: &[Vec<PathBuf>],
) {
    let ProjectOutcome {
        findings, mut contents, ..
    } = outcome;
    let mut index: FxHashMap<PathBuf, usize> = linted.iter().enumerate().map(|(i, r)| (r.path.clone(), i)).collect();
    for finding in findings {
        let ProjectFinding {
            path,
            config_id,
            mut diagnostics,
        } = finding;
        let content = contents
            .remove(&path)
            .or_else(|| std::fs::read_to_string(&path).ok().map(Arc::from));
        let mut suppressed: Vec<SuppressedDiagnostic> = Vec::new();
        if let Some(content) = &content {
            let directives = Suppressions::parse(content);
            if !directives.is_empty() {
                directives.apply_directives(&path, &mut diagnostics, &mut suppressed);
            }
        }
        if let Some(this_ignores) = ignores.get(config_id).filter(|set| !set.is_empty()) {
            let bases = ignore_bases.get(config_id).map_or(&[][..], Vec::as_slice);
            let rel = relative_for_match(&path, bases);
            this_ignores.apply(&rel, &path, &mut diagnostics, &mut suppressed);
        }
        if diagnostics.is_empty() && suppressed.is_empty() {
            continue;
        }
        match index.get(&path) {
            Some(&at) => {
                linted[at].diagnostics.append(&mut diagnostics);
                linted[at].suppressed.append(&mut suppressed);
            }
            None => {
                index.insert(path.clone(), linted.len());
                linted.push(LintResult {
                    path,
                    config: config_id,
                    diagnostics,
                    suppressed,
                    fix_withheld_generated: false,
                    fixed: 0,
                    skipped: None,
                    error: None,
                    debug: None,
                });
            }
        }
    }
}

#[cfg(all(test, feature = "engine-python"))]
mod gate_tests {
    use super::*;
    use crate::config::Config;
    use poly_component_host::ComponentEngine;
    use poly_component_protocol::{WireCapabilities, WireEngineDesc};

    /// The `pyrefly` engine as the component index describes it: a Python project check.
    fn pyrefly_component() -> ComponentEngine {
        ComponentEngine::unavailable_declining("pyrefly", &pyrefly_desc(), "0000000000000000", "test").unwrap()
    }

    fn pyrefly_desc() -> WireEngineDesc {
        WireEngineDesc {
            name: "pyrefly".into(),
            languages: vec!["python".into()],
            capabilities: WireCapabilities {
                lint: false,
                format: false,
                fix: false,
                project: true,
            },
            version: "pyrefly-test".into(),
            option_keys: Vec::new(),
            hooks: 0,
            takes_over_tier_one: false,
            claimed_paths: Vec::new(),
            host_hook: "none".into(),
            supplements_tier_one: false,
            self_manages_enabled: false,
            supersedes_generic_formatter: false,
            batch_shard_floor: 2,
            project_inputs: Default::default(),
            selection: Some(WireSelection {
                table: "python-types".into(),
                key: "backend".into(),
                value: "pyrefly".into(),
                default: false,
            }),
        }
    }

    fn planned(config: Config) -> Vec<&'static str> {
        let files = [DiscoveredFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            config_id: 0,
        }];
        let configs = ConfigSet::single(config);
        let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
        plan_project_engines(
            &files,
            &configs,
            EngineSelection::new(&[], &[]),
            &components,
            poly_component_index::index(),
        )
        .0
        .iter()
        .map(|plan| plan.engine.name())
        .collect()
    }

    fn with_backend(backend: &str) -> Config {
        let lint: toml::Table = toml::from_str(&format!("[python.python-types]\nbackend = \"{backend}\"\n")).unwrap();
        Config {
            lint,
            ..Config::default()
        }
    }

    #[test]
    fn python_types_is_not_planned_without_a_backend() {
        assert!(planned(Config::default()).is_empty());
    }

    #[test]
    fn the_ty_backend_plans_ty_and_not_pyrefly() {
        assert_eq!(planned(with_backend("ty")), ["python-types"]);
    }

    #[test]
    fn the_pyrefly_backend_plans_the_component_engine_and_ty_stands_down() {
        assert_eq!(planned(with_backend("pyrefly")), ["pyrefly"]);
    }

    #[test]
    fn an_unknown_backend_plans_the_default_so_it_can_report_the_value() {
        assert_eq!(planned(with_backend("nonsense")), ["python-types"]);
    }

    #[test]
    fn the_routed_engine_reads_the_shared_table() {
        let files = [DiscoveredFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            config_id: 0,
        }];
        let lint: toml::Table =
            toml::from_str("[python.python-types]\nbackend = \"pyrefly\"\npython_version = \"3.11\"\n").unwrap();
        let configs = ConfigSet::single(Config {
            lint,
            ..Config::default()
        });
        let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
        let (plans, _, _) = plan_project_engines(
            &files,
            &configs,
            EngineSelection::new(&[], &[]),
            &components,
            poly_component_index::index(),
        );
        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0]
                .config
                .options
                .get("python_version")
                .and_then(toml::Value::as_str),
            Some("3.11")
        );
    }

    #[test]
    fn switching_the_shared_table_off_stops_both_backends() {
        let lint: toml::Table =
            toml::from_str("[python.python-types]\nbackend = \"pyrefly\"\nenabled = false\n").unwrap();
        let config = Config {
            lint,
            ..Config::default()
        };
        assert!(planned(config).is_empty());
    }

    fn planned_with(config: Config, only: &[&str], skip: &[&str]) -> Vec<&'static str> {
        let files = [DiscoveredFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            config_id: 0,
        }];
        let (only, skip): (Vec<String>, Vec<String>) = (
            only.iter().map(|s| (*s).to_owned()).collect(),
            skip.iter().map(|s| (*s).to_owned()).collect(),
        );
        let configs = ConfigSet::single(config);
        let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
        plan_project_engines(
            &files,
            &configs,
            EngineSelection::new(&only, &skip),
            &components,
            poly_component_index::index(),
        )
        .0
        .iter()
        .map(|plan| plan.engine.name())
        .collect()
    }

    #[test]
    fn skipping_the_shared_table_stops_the_pyrefly_backend() {
        assert!(planned_with(with_backend("pyrefly"), &[], &["python-types"]).is_empty());
        assert_eq!(planned_with(with_backend("pyrefly"), &[], &["ruff"]), ["pyrefly"]);
    }

    #[test]
    fn only_the_shared_table_keeps_the_pyrefly_backend() {
        assert_eq!(
            planned_with(with_backend("pyrefly"), &["python-types"], &[]),
            ["pyrefly"]
        );
        assert_eq!(planned_with(with_backend("pyrefly"), &["pyrefly"], &[]), ["pyrefly"]);
        assert!(planned_with(with_backend("pyrefly"), &["ruff"], &[]).is_empty());
    }

    #[test]
    fn the_table_name_selects_the_ty_backend_too() {
        assert_eq!(
            planned_with(with_backend("ty"), &["python-types"], &[]),
            ["python-types"]
        );
        assert!(planned_with(with_backend("ty"), &[], &["python-types"]).is_empty());
    }

    #[test]
    fn a_non_string_backend_plans_the_default_so_it_can_report_the_value() {
        for value in ["1", "true", "[\"pyrefly\"]"] {
            let lint: toml::Table = toml::from_str(&format!("[python.python-types]\nbackend = {value}\n")).unwrap();
            let config = Config {
                lint,
                ..Config::default()
            };
            assert_eq!(planned(config), ["python-types"], "backend = {value}");
        }
    }

    #[test]
    fn a_backend_whose_engine_this_run_lacks_is_an_error_naming_the_component() {
        let files = [DiscoveredFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            config_id: 0,
        }];
        let configs = ConfigSet::single(with_backend("pyrefly"));
        let no_components = ResolvedComponents::default();
        let (plans, errors, _) = plan_project_engines(
            &files,
            &configs,
            EngineSelection::new(&[], &[]),
            &no_components,
            poly_component_index::index(),
        );
        assert!(plans.is_empty());
        assert_eq!(
            errors.len(),
            1,
            "{:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        let message = &errors[0].message;
        assert!(
            message.contains("`pyrefly` component") && message.contains("[components] disable"),
            "{message}"
        );

        let skip = ["pyrefly".to_owned()];
        let (_, errors, _) = plan_project_engines(
            &files,
            &configs,
            EngineSelection::new(&[], &skip),
            &no_components,
            poly_component_index::index(),
        );
        assert!(errors.is_empty(), "a run that skipped the backend asked for that");
    }

    fn planned_all(config: Config, only: &[&str], index: &ComponentIndex) -> (usize, Vec<String>, Vec<String>) {
        let files = [DiscoveredFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            config_id: 0,
        }];
        let only: Vec<String> = only.iter().map(|s| (*s).to_owned()).collect();
        let configs = ConfigSet::single(config);
        let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
        let (plans, errors, skips) =
            plan_project_engines(&files, &configs, EngineSelection::new(&only, &[]), &components, index);
        (
            plans.len(),
            errors.into_iter().map(|e| e.message).collect(),
            skips.into_iter().map(|s| s.reason).collect(),
        )
    }

    #[test]
    fn only_pyrefly_over_another_backend_is_a_counted_skip_naming_why() {
        let (plans, errors, skips) = planned_all(with_backend("ty"), &["pyrefly"], poly_component_index::index());
        assert_eq!((plans, errors.len()), (0, 0));
        assert_eq!(
            skips,
            ["`--only pyrefly` selects nothing here: `[python-types] backend` is \"ty\""]
        );
        assert!(
            !crate::is_withdrawal_reason(&skips[0]),
            "must be charged to --deny-skips"
        );
        let (_, _, skips) = planned_all(Config::default(), &["python-types"], poly_component_index::index());
        assert_eq!(
            skips,
            ["`--only python-types` selects nothing here: `[python-types] backend` is unset"]
        );
    }

    #[test]
    fn an_unrelated_only_or_a_satisfied_one_adds_no_skip() {
        let index = poly_component_index::index();
        assert!(planned_all(with_backend("ty"), &["ruff"], index).2.is_empty());
        assert!(planned_all(with_backend("pyrefly"), &["pyrefly"], index).2.is_empty());
        assert!(planned_all(with_backend("ty"), &[], index).2.is_empty());
    }

    fn fake_index() -> ComponentIndex {
        let mut desc = pyrefly_desc();
        desc.name = "fake".into();
        desc.selection = Some(WireSelection {
            table: "fake-types".into(),
            key: "backend".into(),
            value: "x".into(),
            default: true,
        });
        ComponentIndex {
            components: vec![poly_component_index::Component {
                name: "fakes".into(),
                engines: vec![desc],
                ..Default::default()
            }],
            ..ComponentIndex::default()
        }
    }

    #[test]
    fn a_missing_default_engine_is_an_error_not_silence() {
        let lint: toml::Table = toml::from_str("[python.fake-types]\nbackend = \"x\"\n").unwrap();
        let config = Config {
            lint,
            ..Config::default()
        };
        let (_, errors, _) = planned_all(config, &[], &fake_index());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("`fakes` component"), "{}", errors[0]);
    }

    #[test]
    fn the_run_index_decides_what_is_claimed() {
        let files = [DiscoveredFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            config_id: 0,
        }];
        let configs = ConfigSet::single(with_backend("pyrefly"));
        let none = ResolvedComponents::default();
        let empty = ComponentIndex::default();
        let (plans, errors, _) = plan_project_engines(&files, &configs, EngineSelection::new(&[], &[]), &none, &empty);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            plans.len(),
            1,
            "no engine of this run's index claims `pyrefly`, so the default reports it"
        );
    }
}
