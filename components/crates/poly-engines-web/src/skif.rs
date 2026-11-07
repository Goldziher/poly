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
//! host walks the declared tree and hands over every JavaScript, TypeScript, JSON, YAML and TOML file in it,
//! plus the lock files as side files. The walk honors `.gitignore` and the pruned directories (`node_modules`,
//! `dist`, ...) and nothing else: `[discovery] exclude`, `--exclude` and nested `poly.toml` scopes do not cut
//! the graph, because the verdict is about the whole workspace and the run's paths only decide which findings
//! are reported. Symlinks are not followed, so a package reached through one is absent from the graph.
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
//! not the project root, `threads` the size of skif's own pool (default: the host's job budget, else the machine's parallelism; 1 spawns none), and
//! `pnpm_lock`/`npm_lock`/`yarn_lock`/`bun_lock` the lock files when they are not at the project root.
//! `[engines.skif.rules.<id>] level` moves a finding's severity through the runner's normal remap.

mod fs;
mod map;

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use poly_engine_api::engine::{
    Capabilities, Diagnostic, Engine, OptionKeys, OptionTable, OptionType, ProjectDirs, ProjectInputs, ProjectRequest,
    ProjectSideFile, Severity,
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
const VERSION: &str = "skif-0.1.1+poly4";

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

/// Ignore files skif reads for the workspace: a relative import of a gitignored (generated) file is ignored,
/// not unresolved, only when skif can see the `.gitignore` that names it.
const IGNORE_FILES: &[&str] = &[".gitignore", ".ignore"];

/// Rules that judge a binary by what an installed package provides.
///
/// The run is hermetic, so `node_modules` is never there and nothing says which package ships a binary named
/// otherwise than the package (`@acme/toolkit` running `kit`). Off unless the project asks for them: in `select`,
/// in `[engines.skif.rules.<id>]`, or in its own `skif.toml` `[rules]`.
const NEEDS_INSTALLED_PACKAGES: &[&str] = &["unlisted-binaries"];

/// Rules that also need to know which binaries and peers a dependency provides, to tell a dependency that is
/// only run as a script binary (or only the peer of another dependency) from an unused one.
///
/// skif falls back on a lock file when nothing is installed. `package-lock.json`, `bun.lock` and a Berry
/// `yarn.lock` record binary names and peers; `pnpm-lock.yaml` records that a package has a binary and skif
/// assumes it is named like the package, a classic `yarn.lock` records neither, and no lock file records
/// nothing. These rules are on when the lock file in effect records them, and otherwise off like
/// [`NEEDS_INSTALLED_PACKAGES`].
const NEEDS_RECORDED_BINARIES: &[&str] = &["unused-dependencies", "unused-dev-dependencies"];

/// The lock files in the order skif consults them: it reads the first that exists.
const LOCK_PRECEDENCE: &[&str] = &["pnpm-lock.yaml", "package-lock.json", "bun.lock", "yarn.lock"];

/// The id of the note that names the rules left off by default.
pub const DEFAULTED_OFF_CODE: &str = "rules-defaulted-off";

/// The files skif reads: sources (single-file components included, which skif counts as code), manifests,
/// `tsconfig`, `pnpm-workspace.yaml` and `skif.toml`. `.mdx` is not among them: skif has no use for it.
static LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::TypeScript,
    Language::Jsx,
    Language::Tsx,
    Language::Vue,
    Language::Svelte,
    Language::Astro,
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
    /// One info-level finding naming the rules left off by default, on the workspace's root `package.json`,
    /// or `None` when every rule ran.
    pub note: Option<(PathBuf, Diagnostic)>,
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
                whole_tree: true,
                file_names: IGNORE_FILES.iter().map(|name| (*name).to_owned()).collect(),
                list_unserved: false,
            }),
        }
    }

    fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
        let Analysis { mut findings, note, .. } = analyze(req)?;
        if let Some((path, note)) = note {
            match findings.iter_mut().find(|(found, _)| *found == path) {
                Some((_, diagnostics)) => diagnostics.push(note),
                None => findings.push((path, vec![note])),
            }
        }
        Ok(findings)
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
        note: None,
    };
    if req.config.options.get("enabled").and_then(toml::Value::as_bool) != Some(true) {
        return Ok(nothing());
    }
    let settings = Settings::from_options(&req.config.options, req.threads)?;
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
    let mut config = skif::load_config(&root, &fs, &settings.overrides).context("loading the skif configuration")?;
    let records_binaries = lock_records_binaries(req.files, &root);
    let off: Vec<String> = NEEDS_INSTALLED_PACKAGES
        .iter()
        .chain(NEEDS_RECORDED_BINARIES.iter().filter(|_| !records_binaries))
        .filter(|id| !settings.asked_for.contains(**id) && config.rule(id).is_none())
        .map(|id| (*id).to_owned())
        .collect();
    config
        .apply(&ConfigOverrides::default().with_ignore_rules(off.clone()))
        .context("turning off the rules that need installed packages")?;
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
    let note = defaulted_off_note(&off, records_binaries).map(|note| (host_path(Path::new("package.json")), note));
    Ok(Analysis {
        findings,
        log: fs.log(),
        note,
    })
}

/// Whether the lock file skif reads in the workspace `root` records the binary names and peers of its packages.
/// Looked up in the request's own files, so it adds nothing to the log of what skif read.
fn lock_records_binaries(files: &[poly_engine_api::engine::ProjectFile], root: &Path) -> bool {
    let Some((name, lock)) = LOCK_PRECEDENCE.iter().find_map(|name| {
        let wanted = root.join(name);
        files
            .iter()
            .find(|file| absolute(&file.path) == wanted)
            .map(|file| (*name, file))
    }) else {
        return false;
    };
    match name {
        "package-lock.json" => names_locked_packages(&lock.content, "node_modules/"),
        "bun.lock" => names_locked_packages(&strip_trailing_commas(&lock.content), ""),
        "yarn.lock" => lock.content.contains("__metadata:"),
        _ => false,
    }
}

/// Whether `json` is a lock whose `packages` object has a key containing `marker`: skif reads nothing
/// from a lockfileVersion 1 `package-lock.json` (it has `dependencies`, not `packages`) or from a malformed file,
/// and a lock it gets no package data from records no binaries.
fn names_locked_packages(json: &str, marker: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| value.get("packages").and_then(serde_json::Value::as_object).cloned())
        .is_some_and(|packages| packages.keys().any(|key| key.contains(marker)))
}

/// `bun.lock` is JSONC: drops the commas that precede a closing bracket so it parses as JSON.
fn strip_trailing_commas(text: &str) -> String {
    static TRAILING: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = TRAILING.get_or_init(|| regex::Regex::new(r",(\s*[}\]])").expect("a valid pattern"));
    pattern.replace_all(text, "$1").into_owned()
}

/// The info-level finding that makes the defaulting visible: which rules were left off and how to turn them on.
fn defaulted_off_note(off: &[String], records_binaries: bool) -> Option<Diagnostic> {
    if off.is_empty() {
        return None;
    }
    let list = off.iter().map(|id| format!("`{id}`")).collect::<Vec<_>>().join(", ");
    let why = if records_binaries {
        "they judge a dependency by the binaries an installed package provides, which this run cannot see"
    } else {
        "they judge a dependency by the binaries and peers an installed package provides, which this run cannot see \
         and the lock file does not record"
    };
    Some(Diagnostic {
        engine: ENGINE_NAME.to_owned(),
        code: Some(DEFAULTED_OFF_CODE.to_owned()),
        severity: Severity::Info,
        title: format!("skif left {list} off: {why}"),
        description: Some(
            "Turn a rule on by naming it in `select`, or in `[engines.skif.rules.<id>]` (`level = \"warning\"`), \
             or in the project's `skif.toml` `[rules]`."
                .to_owned(),
        ),
        span: None,
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    })
}

/// `path` made absolute without touching the file system. skif refuses relative paths, and the host may
/// root a project at a relative directory.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The options skif reads, validated. `threads` defaults to the host's job budget (`ProjectRequest::threads`), else
/// the machine's parallelism: reading and parsing every manifest and source of a monorepo on one thread is the bulk
/// of a cold run.
struct Settings {
    dir: PathBuf,
    threads: NonZeroUsize,
    select: Option<Vec<String>>,
    /// Rules the project named itself, in `select` or `[engines.skif.rules.<id>]`.
    asked_for: BTreeSet<String>,
    overrides: ConfigOverrides,
}

impl Settings {
    fn from_options(options: &toml::Table, budget: Option<usize>) -> anyhow::Result<Self> {
        let dir = match options.get("dir") {
            None => PathBuf::from(DEFAULT_DIR),
            Some(value) => PathBuf::from(value.as_str().with_context(|| wrong("dir", "a string", value))?),
        };
        let threads = match options.get("threads") {
            None => budget
                .and_then(NonZeroUsize::new)
                .or_else(|| std::thread::available_parallelism().ok())
                .unwrap_or(NonZeroUsize::MIN),
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
        let select = str_list(options, "select")?;
        let configured = options.get("rules").and_then(toml::Value::as_table);
        let asked_for = select
            .iter()
            .flatten()
            .cloned()
            .chain(configured.into_iter().flat_map(|rules| rules.keys().cloned()))
            .collect();
        Ok(Self {
            dir,
            threads,
            select,
            asked_for,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_default_to_the_hosts_budget_then_the_machines_parallelism_and_an_option_overrides_both() {
        let machine = Settings::from_options(&toml::Table::new(), None)
            .expect("defaults")
            .threads;
        assert_eq!(machine, std::thread::available_parallelism().expect("parallelism"));
        let zero = Settings::from_options(&toml::Table::new(), Some(0)).expect("zero budget");
        assert_eq!(zero.threads, machine, "a budget of zero is no budget");
        let budget = Settings::from_options(&toml::Table::new(), Some(3)).expect("budget");
        assert_eq!(budget.threads.get(), 3);
        let one = Settings::from_options(&toml::from_str("threads = 1").expect("toml"), Some(3)).expect("one");
        assert_eq!(one.threads, NonZeroUsize::MIN);
    }
}
