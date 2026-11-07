//! skif backend: dependency, manifest, version-policy and dead-code linting for JavaScript and TypeScript
//! workspaces (npm, pnpm, yarn, bun).
//!
//! Wraps [`skif`](https://crates.io/crates/skif) in-process, as a **project check**: the rules judge a whole
//! workspace graph, so the engine declares `capabilities.project` and nothing runs per file. The flow is skif's
//! own: `find_workspace_root`, `load_config`, `Linter::discover_with_fs`, `Linter::run_with`.
//!
//! # Inputs and the cache key
//!
//! The host computes the project-check cache key before the call, so every input has to be known up front. The
//! engine therefore declares them ([`Engine::project_inputs`]) instead of discovering them while it runs: the
//! host walks the declared tree (honoring `.gitignore`, the pruned directories and `[discovery] exclude`) and
//! hands over every JavaScript, TypeScript, JSON, YAML and TOML file in it, plus the lock files as side files.
//! Manifests, `tsconfig`, `pnpm-workspace.yaml`, `skif.toml` and sources are all in that set, so editing, adding
//! or removing any of them changes the key. skif then runs on a [`RecordingFs`](fs::RecordingFs) that serves
//! only those files: nothing is read from the disk, so the result cannot depend on a `node_modules` that is
//! installed on one machine and not another, and the log of what it read is how a test proves every read was a
//! keyed input ([`analyze`]).
//!
//! # Activation
//!
//! **Off unless `[engines.skif] enabled = true`.** Discovering a JavaScript workspace is not consent to
//! enforce dependency policy on it: the first run on an existing monorepo reports findings, and turning a green
//! run red on upgrade is what the other opt-in engines avoid too (ADR 0044).
//!
//! # Configuration
//!
//! A project's `skif.toml` is honored as skif reads it. `[engines.skif]` maps onto skif's config overrides:
//! `ignore` (rule ids turned off), `select` (run only these rules), `ignore_packages`, `ignore_dependencies`,
//! `production`, `js` (`false` turns the source analysis off). `dir` names the workspace directory when it is
//! not the project root, `threads` the size of skif's own pool (default 1, which spawns none), and
//! `pnpm_lock`/`npm_lock`/`yarn_lock`/`bun_lock` the lock files when they are not at the project root.
//! `[engines.skif.rules.<id>] level` moves a finding's severity through the runner's normal remap.

mod fs;
mod map;

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use poly_engine_api::engine::{
    Capabilities, Diagnostic, Engine, OptionKeys, OptionTable, OptionType, ProjectDirs, ProjectInputs, ProjectRequest,
    ProjectSideFile,
};
use poly_engine_api::language::Language;
use skif::{ConfigOverrides, Linter, RunOptions};

pub use fs::ReadLog;
use fs::RecordingFs;

/// The engine id, and so the `[engines.skif]` table that configures it.
pub const ENGINE_NAME: &str = "skif";

/// Cache-key identity: the wrapped crate version plus a marker for this backend's own mapping.
///
/// Bump the suffix whenever the diagnostic or fix mapping changes, and the version whenever `skif` is upgraded.
const VERSION: &str = "skif-0.1.1+poly1";

/// The directory skif is run on, relative to the project root.
const DEFAULT_DIR: &str = ".";

/// Lock files skif consults, as (option, default path). The host skips lock files when it walks a tree, so they
/// travel as side inputs.
const LOCK_FILES: &[(&str, &str)] = &[
    ("pnpm_lock", "pnpm-lock.yaml"),
    ("npm_lock", "package-lock.json"),
    ("yarn_lock", "yarn.lock"),
    ("bun_lock", "bun.lock"),
];

/// The files skif reads: sources, manifests, `tsconfig`, `pnpm-workspace.yaml` and `skif.toml`.
static LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::TypeScript,
    Language::Jsx,
    Language::Tsx,
    Language::Json,
    Language::Jsonc,
    Language::Yaml,
    Language::Toml,
];

/// What one run produced, and what it read to produce it.
#[derive(Debug)]
pub struct Analysis {
    /// Findings grouped by the file they are in.
    pub findings: Vec<(PathBuf, Vec<Diagnostic>)>,
    /// Every lookup skif made, against the files the request carried.
    pub log: ReadLog,
}

/// skif dependency and monorepo linter. See the module docs.
pub struct SkifEngine;

impl Engine for SkifEngine {
    /// No per-file hook is overridden, so the host answers all of them from the index and a warm run never
    /// loads the component for a JSON, YAML or TOML file just to ask a skif that has nothing to say about it.
    fn overridden_hooks(&self) -> u32 {
        0
    }

    fn name(&self) -> &'static str {
        ENGINE_NAME
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: false,
            format: false,
            fix: false,
            project: true,
        }
    }

    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint => OptionKeys::declared(&[
                ("enabled", OptionType::BOOLEAN),
                ("dir", OptionType::STRING),
                ("threads", OptionType::INTEGER),
                ("select", OptionType::ARRAY),
                ("ignore", OptionType::ARRAY),
                ("ignore_packages", OptionType::ARRAY),
                ("ignore_dependencies", OptionType::ARRAY),
                ("production", OptionType::BOOLEAN),
                ("js", OptionType::BOOLEAN),
                ("pnpm_lock", OptionType::STRING),
                ("npm_lock", OptionType::STRING),
                ("yarn_lock", OptionType::STRING),
                ("bun_lock", OptionType::STRING),
            ]),
            OptionTable::Format => OptionKeys::declared(&[]),
            OptionTable::CrossCuttingLint => OptionKeys::UNCHECKED,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    /// Planned only once `enabled` is set, over the whole tree under `dir`, with the lock files as side files.
    fn project_inputs(&self) -> ProjectInputs {
        ProjectInputs {
            gate_key: Some("enabled".to_owned()),
            files: LOCK_FILES
                .iter()
                .map(|(option_key, default_path)| ProjectSideFile {
                    option_key: (*option_key).to_owned(),
                    default_path: (*default_path).to_owned(),
                })
                .collect(),
            dirs: Some(ProjectDirs {
                dir_key: "dir".to_owned(),
                dir_default: DEFAULT_DIR.to_owned(),
                paths_key: None,
                recursive_key: None,
                recursive_default: true,
                list_unserved: false,
            }),
        }
    }

    fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
        analyze(req).map(|analysis| analysis.findings)
    }
}

/// Runs skif over the files in `req` and returns its findings together with the log of what it read.
///
/// An engine that is not enabled, and a project with no JavaScript workspace in it, produce nothing.
///
/// # Errors
/// An option of the wrong type, an invalid `skif.toml`, a rule named in `select` that skif does not have, or a
/// failure of skif itself.
pub fn analyze(req: &ProjectRequest<'_>) -> anyhow::Result<Analysis> {
    let nothing = || Analysis {
        findings: Vec::new(),
        log: ReadLog::default(),
    };
    if req.config.options.get("enabled").and_then(toml::Value::as_bool) != Some(true) {
        return Ok(nothing());
    }
    let settings = Settings::from_options(&req.config.options)?;
    let project = absolute(req.root);
    let fs = RecordingFs::new(
        req.files
            .iter()
            .map(|file| (absolute(&file.path), file.content.as_ref())),
    );

    let start = project.join(&settings.dir);
    let root = match skif::find_workspace_root(&start, &fs) {
        Ok(root) => root,
        Err(skif::Error::NoWorkspace(_)) => return Ok(nothing()),
        Err(error) => return Err(error).context("locating the JavaScript workspace"),
    };
    let config = skif::load_config(&root, &fs, &settings.overrides).context("loading the skif configuration")?;
    let linter = Linter::with_fs(&config, Arc::new(fs.clone())).context("building the skif linter")?;

    let mut options = RunOptions::default().with_threads(settings.threads);
    if let Some(select) = &settings.select {
        options = options.with_rules(select);
    }
    let workspace = linter
        .discover_with_fs(&root, &fs, &options)
        .context("discovering the workspace")?;
    let report = linter.run_with(&workspace, &options).context("running skif")?;

    let host_path = |relative: &Path| {
        let found = root.join(relative);
        match found.strip_prefix(&project) {
            Ok(tail) => req.root.join(tail),
            Err(_) => found,
        }
    };
    let findings = map::findings(&report, &workspace, &fs, &host_path);
    Ok(Analysis {
        findings,
        log: fs.log(),
    })
}

/// `path` made absolute without touching the file system. skif refuses relative paths, and the host may
/// root a project at a relative directory.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The options skif reads, validated.
struct Settings {
    dir: PathBuf,
    threads: NonZeroUsize,
    select: Option<Vec<String>>,
    overrides: ConfigOverrides,
}

impl Settings {
    fn from_options(options: &toml::Table) -> anyhow::Result<Self> {
        let dir = match options.get("dir") {
            None => PathBuf::from(DEFAULT_DIR),
            Some(value) => PathBuf::from(value.as_str().with_context(|| wrong("dir", "a string", value))?),
        };
        let threads = match options.get("threads") {
            None => NonZeroUsize::MIN,
            Some(value) => value
                .as_integer()
                .and_then(|count| usize::try_from(count).ok())
                .and_then(NonZeroUsize::new)
                .with_context(|| wrong("threads", "a positive integer", value))?,
        };
        let mut overrides = ConfigOverrides::default();
        overrides.ignore_rules = str_list(options, "ignore")?.unwrap_or_default();
        overrides.ignore_packages = str_list(options, "ignore_packages")?.unwrap_or_default();
        overrides.ignore_dependencies = str_list(options, "ignore_dependencies")?.unwrap_or_default();
        overrides.production = flag(options, "production")?;
        overrides.no_js = flag(options, "js")? == Some(false);
        Ok(Self {
            dir,
            threads,
            select: str_list(options, "select")?,
            overrides,
        })
    }
}

fn wrong(key: &str, expected: &str, found: &toml::Value) -> String {
    format!("option `{key}` must be {expected}, found {}", found.type_str())
}

fn flag(options: &toml::Table, key: &str) -> anyhow::Result<Option<bool>> {
    options
        .get(key)
        .map(|value| value.as_bool().with_context(|| wrong(key, "a boolean", value)))
        .transpose()
}

fn str_list(options: &toml::Table, key: &str) -> anyhow::Result<Option<Vec<String>>> {
    let Some(value) = options.get(key) else {
        return Ok(None);
    };
    let Some(list) = value.as_array() else {
        bail!(wrong(key, "an array of strings", value));
    };
    list.iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("option `{key}` must hold only strings, found {}", entry.type_str()))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Some)
}
