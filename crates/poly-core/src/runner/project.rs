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
mod scope;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use poly_cache::{Namespace, ResultCache};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use super::plan::build_severity_remap;
use super::selection::EngineSelection;
use super::skips::is_lock_file;
use super::types::{LintError, LintResult};
use crate::components::ResolvedComponents;
use crate::config::{EngineConfig, Kind};
use crate::discover::DiscoveredFile;
use crate::engine::{Diagnostic, ENABLED_OPTION_KEY, Engine, ProjectEnvironment, ProjectFile, ProjectRequest};
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
fn ancestor_configs(configs: &ConfigSet, paths: &[PathBuf]) -> Vec<usize> {
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
) -> Vec<ProjectPlan> {
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
        for engine in registered.chain(from_components).chain(injected) {
            if !selection.selects(engine.name()) || (ancestor && !is_whole_tree(&*engine)) {
                continue;
            }
            if !seen.insert((*config_id, engine.name())) {
                continue;
            }
            let config = configs
                .config(*config_id)
                .engine_config(language, engine.name(), Kind::Lint);
            if !is_active(&*engine, &config) {
                continue;
            }
            plans.push(ProjectPlan {
                engine,
                config_id: *config_id,
                config,
                language: language.clone(),
                ancestor,
            });
        }
    }
    plans.sort_by(|a, b| (a.config_id, a.engine.name()).cmp(&(b.config_id, b.engine.name())));
    plans
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
}

impl<'a> RunInputs<'a> {
    pub(super) fn new(
        configs: &'a ConfigSet,
        act_on_generated: &'a [bool],
        paths: &'a [PathBuf],
        exclude: &'a [String],
        filters: (&'a [PerFileIgnores], &'a [Vec<PathBuf>]),
    ) -> Self {
        Self {
            configs,
            act_on_generated,
            paths,
            exclude,
            ignores: filters.0,
            ignore_bases: filters.1,
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
        let id = self.configs.config_id_for(path);
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
        let path = match narrowed_to {
            Some(run_scope) => match run_scope.admit(&path) {
                Some(admitted) => admitted,
                None => continue,
            },
            None => path,
        };
        let config_id = if covered.is_some() {
            match governing.of(&path) {
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
    let plans = plan_project_engines(files, run.configs, selection, components, run.paths);
    let mut outcome = ProjectOutcome::default();
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

/// One file's project findings, from every engine that reported on it, before `--fix` acts on them.
struct Pending {
    config_id: usize,
    loaded: Option<Arc<str>>,
    diagnostics: Vec<Diagnostic>,
    suppressed: Vec<SuppressedDiagnostic>,
}

/// Run the project phase and fold its findings into `linted`, returning the checks that failed.
///
/// Under `--fix` the edits are committed to disk, which moves every offset a remaining finding of the
/// same file carries and can expose findings the old content hid. So when a pass rewrote a file the
/// engines that reported on it run once more over the rewritten tree, and only that second pass is
/// reported for them: its findings have spans into the files as they now are, and the groups the first
/// pass deferred (they overlapped another edit) are tried again. Every other engine's first-pass
/// findings stand: its input did not change, so running it again would repeat the walk, the read and
/// the hash for the same answer. It is the last pass, never a loop.
pub(super) fn run_and_merge(
    linted: &mut Vec<LintResult>,
    files: &[DiscoveredFile],
    run: &RunInputs<'_>,
    selection: EngineSelection<'_>,
    cache: &ResultCache,
    components: &ResolvedComponents,
    fix: FixMode,
) -> Vec<LintError> {
    let (ignores, ignore_bases) = (run.ignores, run.ignore_bases);
    let mut phase = run_project_phase(files, run, selection, cache, components, None);
    let errors = std::mem::take(&mut phase.errors);
    if !fix.apply {
        merge_findings(linted, phase, ignores, ignore_bases, fix);
        return errors;
    }
    let first_findings = phase.findings.clone();
    let mut first_pass = Vec::new();
    merge_findings(&mut first_pass, phase, ignores, ignore_bases, fix);
    let rewritten: FxHashSet<&Path> = first_pass
        .iter()
        .filter(|result| result.fixed > 0)
        .map(|result| result.path.as_path())
        .collect();
    if rewritten.is_empty() {
        fold(linted, first_pass);
        return errors;
    }
    let rerun: FxHashSet<&'static str> = first_findings
        .iter()
        .filter(|finding| rewritten.contains(finding.path.as_path()))
        .map(|finding| finding.engine)
        .collect();
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
    let errors = merge_errors(errors, std::mem::take(&mut again.errors));
    merge_findings(linted, again, ignores, ignore_bases, fix);
    errors
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

/// Fold project findings into the per-file results, applying inline `poly: allow` directives and
/// `[per-file-ignores]` exactly as the per-file tier does, then `--fix`.
///
/// A finding for a file the per-file tier never produced a result for (it was skipped, or lies
/// outside the discovered set) gets a result of its own. Findings of one file from several engines
/// are fixed together, so the edits of all of them are applied against the same content.
pub(super) fn merge_findings(
    linted: &mut Vec<LintResult>,
    outcome: ProjectOutcome,
    ignores: &[PerFileIgnores],
    ignore_bases: &[Vec<PathBuf>],
    fix: FixMode,
) {
    let mut order: Vec<PathBuf> = Vec::new();
    let mut pending: FxHashMap<PathBuf, Pending> = FxHashMap::default();
    for finding in outcome.findings {
        let ProjectFinding {
            path,
            config_id,
            mut diagnostics,
            loaded,
            ..
        } = finding;
        let content = loaded
            .clone()
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
        match pending.get_mut(&path) {
            Some(entry) => {
                // Edits are offsets into the content their engine read; a different read cannot share a commit.
                if entry.loaded != loaded {
                    for diagnostic in &mut diagnostics {
                        diagnostic.fix.clear();
                    }
                }
                entry.diagnostics.append(&mut diagnostics);
                entry.suppressed.append(&mut suppressed);
            }
            None => {
                order.push(path.clone());
                pending.insert(
                    path,
                    Pending {
                        config_id,
                        loaded,
                        diagnostics,
                        suppressed,
                    },
                );
            }
        }
    }
    let mut results = Vec::new();
    for path in order {
        let Some(Pending {
            config_id,
            loaded,
            mut diagnostics,
            suppressed,
        }) = pending.remove(&path)
        else {
            continue;
        };
        let applied = fixes::resolve(&path, loaded.as_deref(), &mut diagnostics, fix);
        if diagnostics.is_empty() && suppressed.is_empty() && applied.fixed == 0 {
            continue;
        }
        results.push(LintResult {
            path,
            config: config_id,
            diagnostics,
            suppressed,
            fix_withheld_generated: applied.withheld_generated,
            fixed: applied.fixed,
            skipped: None,
            error: None,
            debug: None,
        });
    }
    fold(linted, results);
}

/// Add `results` to `linted`, joining a file the per-file tier already has a result for.
fn fold(linted: &mut Vec<LintResult>, results: Vec<LintResult>) {
    let mut index: FxHashMap<PathBuf, usize> = linted.iter().enumerate().map(|(i, r)| (r.path.clone(), i)).collect();
    for mut result in results {
        match index.get(&result.path) {
            Some(&at) => {
                linted[at].diagnostics.append(&mut result.diagnostics);
                linted[at].suppressed.append(&mut result.suppressed);
                linted[at].fixed += result.fixed;
                linted[at].fix_withheld_generated |= result.fix_withheld_generated;
            }
            None => {
                index.insert(result.path.clone(), linted.len());
                linted.push(result);
            }
        }
    }
}

#[cfg(all(test, feature = "engine-python"))]
mod gate_tests {
    use super::*;
    use crate::config::Config;

    fn planned(config: Config) -> Vec<&'static str> {
        let files = [DiscoveredFile {
            path: PathBuf::from("app.py"),
            language: Language::Python,
            config_id: 0,
        }];
        let configs = ConfigSet::single(config);
        plan_project_engines(
            &files,
            &configs,
            EngineSelection::new(&[], &[]),
            &ResolvedComponents::default(),
            &[],
        )
        .iter()
        .map(|plan| plan.engine.name())
        .collect()
    }

    #[test]
    fn python_types_is_not_planned_without_a_backend() {
        assert!(!planned(Config::default()).contains(&"python-types"));
    }

    #[test]
    fn python_types_is_planned_once_a_backend_is_set() {
        let lint: toml::Table = toml::from_str("[python.python-types]\nbackend = \"ty\"\n").unwrap();
        let config = Config {
            lint,
            ..Config::default()
        };
        assert!(planned(config).contains(&"python-types"));
    }
}
