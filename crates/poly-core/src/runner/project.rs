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
//! Results are cached under [`Namespace::Lint`] with the engine id `project:<engine>`, keyed on
//! the engine version, a fingerprint of its resolved config, the host environment, and the sorted
//! `(path, content digest)` list of the files it saw — any change to one file, the set, the config
//! or the interpreter re-runs the whole check, and nothing else does.

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
use crate::registry::{GATED_PROJECT_ENGINES, engines_for};
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
) -> Vec<ProjectPlan> {
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
        for engine in registered.chain(from_components).chain(injected) {
            if !selection.selects(engine.name()) || !seen.insert((*config_id, engine.name())) {
                continue;
            }
            let config = configs
                .config(*config_id)
                .engine_config(language, engine.name(), Kind::Lint);
            if config.options.get(ENABLED_OPTION_KEY).and_then(toml::Value::as_bool) == Some(false) {
                continue;
            }
            let gated_off = GATED_PROJECT_ENGINES
                .iter()
                .any(|(name, key)| *name == engine.name() && !config.options.contains_key(*key));
            if gated_off {
                continue;
            }
            plans.push(ProjectPlan {
                engine,
                config_id: *config_id,
                config,
            });
        }
    }
    plans.sort_by(|a, b| (a.config_id, a.engine.name()).cmp(&(b.config_id, b.engine.name())));
    plans
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

/// Run every applicable project engine over `files`.
pub(super) fn run_project_phase(
    files: &[DiscoveredFile],
    configs: &ConfigSet,
    selection: EngineSelection<'_>,
    cache: &ResultCache,
    act_on_generated: &[bool],
    components: &ResolvedComponents,
) -> ProjectOutcome {
    let plans = plan_project_engines(files, configs, selection, components);
    let mut outcome = ProjectOutcome::default();
    if plans.is_empty() {
        return outcome;
    }
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
        let root = root_for(configs, plan.config_id);
        let environment = environments
            .entry(root.clone())
            .or_insert_with(|| crate::project::env::resolve_environment(&root));
        let loaded = load_files(&scoped, act_on_generated);
        let key = cache.enabled().then(|| cache_key(plan, &root, &loaded, &*environment));
        let cached = key
            .as_ref()
            .and_then(|key| cache.get(Namespace::Lint, key))
            .and_then(|bytes| serde_json::from_slice::<Vec<(PathBuf, Vec<Diagnostic>)>>(&bytes).ok());
        let reported = match cached {
            Some(reported) => reported,
            None => {
                let request = ProjectRequest {
                    root: &root,
                    files: &loaded,
                    config: &plan.config,
                    environment: &*environment,
                };
                match guard_engine_panic(plan.engine.name(), &root, || plan.engine.project_check(&request)) {
                    Ok(reported) => {
                        if let Some(key) = &key
                            && let Ok(bytes) = serde_json::to_vec(&reported)
                            && let Err(error) = cache.put(Namespace::Lint, key, &bytes)
                        {
                            tracing::debug!(%error, "project check: result not cached");
                        }
                        reported
                    }
                    Err(error) => {
                        outcome.errors.push(LintError {
                            path: root.clone(),
                            message: format!("project check '{}' failed: {error:#}", plan.engine.name()),
                        });
                        continue;
                    }
                }
            }
        };
        let remap = build_severity_remap(&plan.config);
        for (path, mut diagnostics) in reported {
            if !remap.is_empty() {
                remap.apply(&mut diagnostics);
            }
            // The edits were computed against the engine's own view of the project and nothing
            // applies them, so advertising them as fixable would be a promise `--fix` cannot keep.
            for diagnostic in &mut diagnostics {
                diagnostic.fix.clear();
            }
            outcome.findings.push(ProjectFinding {
                path,
                config_id: plan.config_id,
                diagnostics,
            });
        }
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
