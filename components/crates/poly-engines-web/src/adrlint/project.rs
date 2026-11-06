//! The tree-wide half of the adrlint backend: numbering, gaps, duplicates and file names
//! (`MADR41a`, `MADR41b`, `MADR42`, `MADR43`, `MADR44`) and the immutability lock (`ADR50`, `ADR51`).
//!
//! These rules judge a whole ADR log, so they run as a project check over the Markdown files the
//! host discovered, not per file. The component reads nothing: `adrlint.lock` arrives as one more
//! [`ProjectFile`] (the host loads it, see `PROJECT_SIDE_INPUTS` in `poly-core`), which is also
//! what makes the result cacheable, since the project-check cache key folds in the digest of every
//! file handed over, the lock and each sibling ADR included.
//!
//! The ADR log is `[lint.markdown.adrlint] dir` (default `docs/adr`, adrlint's own default) or the
//! directories in `paths`, relative to the project root; `recursive = true` takes their
//! subdirectories too. `start` and `allow_gaps` mean what they do in adrlint's `[adr]` table, and
//! `lock` names the manifest (default `adrlint.lock`). A glob in `paths` is not supported. `MADR42`
//! flags a non-Markdown file in the log, which only fires when the host supplies one: it hands over
//! the files of the languages the engine serves, Markdown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ::adrlint::config::AdrConfig;
use ::adrlint::diagnostics as adr_diagnostics;
use ::adrlint::lock;
use anyhow::Context;
use poly_engine_api::engine::{Diagnostic, ProjectFile, ProjectRequest};
use poly_engine_api::language::Language;
use poly_engine_api::rule_config::string_list;

use super::{Selection, enabled, map_diagnostic};

/// The default ADR directory, adrlint's own.
const DEFAULT_DIR: &str = "docs/adr";

/// The default lock manifest, matching the host's `PROJECT_SIDE_INPUTS`.
const DEFAULT_LOCK: &str = "adrlint.lock";

/// Runs the tree-wide and lock rules over the ADR log in `req.files`.
pub(super) fn check(req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
    let cfg = req.config;
    if !enabled(cfg) {
        return Ok(Vec::new());
    }
    let settings = Settings::from_options(cfg);
    let lock_name = settings.lock.clone();

    let mut members: Vec<(PathBuf, &str)> = Vec::new();
    let mut originals: BTreeMap<PathBuf, &Path> = BTreeMap::new();
    let mut manifest = None;
    for file in req.files {
        let relative = file.path.strip_prefix(req.root).unwrap_or(&file.path).to_path_buf();
        if is_lock(file, &relative, &lock_name) {
            manifest = Some(
                lock::parse_manifest(&file.content)
                    .with_context(|| format!("invalid lock file {}", relative.display()))?,
            );
        } else if settings.in_log(&relative) {
            originals.insert(relative.clone(), &file.path);
            members.push((relative, &file.content));
        }
    }

    let mut config = AdrConfig::default();
    if let Some(start) = settings.start {
        config.start = start;
        config.start_configured = true;
    }
    config.allow_gaps = settings.allow_gaps;

    let mut findings = ::adrlint::rules::check_project(&members, manifest.as_ref(), &config);
    adr_diagnostics::add_default_help(&mut findings);

    let selection = Selection::from_options(cfg);
    let no_fixes = BTreeMap::new();
    let mut grouped: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for finding in findings.into_iter().filter(|finding| selection.keeps(finding.code)) {
        let path = originals
            .get(&finding.path)
            .map_or_else(|| req.root.join(&finding.path), |original| (*original).to_path_buf());
        grouped
            .entry(path)
            .or_default()
            .push(map_diagnostic(finding, &no_fixes));
    }
    Ok(grouped.into_iter().collect())
}

/// Whether `file` is the lock manifest the host loaded as a side input.
fn is_lock(file: &ProjectFile, relative: &Path, lock_name: &Path) -> bool {
    matches!(file.language, Language::Other(_)) && relative == lock_name
}

/// The `[lint.markdown.adrlint]` keys the project check reads.
struct Settings {
    roots: Vec<PathBuf>,
    recursive: bool,
    start: Option<u32>,
    allow_gaps: bool,
    lock: PathBuf,
}

impl Settings {
    fn from_options(cfg: &poly_engine_api::config::EngineConfig) -> Self {
        let mut roots: Vec<PathBuf> = string_list(cfg, "paths").into_iter().map(PathBuf::from).collect();
        if roots.is_empty() {
            let dir = cfg
                .options
                .get("dir")
                .and_then(toml::Value::as_str)
                .unwrap_or(DEFAULT_DIR);
            roots.push(PathBuf::from(dir));
        }
        Self {
            roots,
            recursive: option_bool(cfg, "recursive"),
            start: cfg
                .options
                .get("start")
                .and_then(toml::Value::as_integer)
                .and_then(|start| u32::try_from(start).ok()),
            allow_gaps: option_bool(cfg, "allow_gaps"),
            lock: PathBuf::from(
                cfg.options
                    .get("lock")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(DEFAULT_LOCK),
            ),
        }
    }

    /// Whether `relative` sits in a configured ADR directory (or beneath one, when recursive).
    fn in_log(&self, relative: &Path) -> bool {
        let Some(parent) = relative.parent() else {
            return false;
        };
        self.roots.iter().any(|root| {
            let root = root.strip_prefix(".").unwrap_or(root);
            if self.recursive {
                parent.starts_with(root)
            } else {
                parent == root
            }
        })
    }
}

fn option_bool(cfg: &poly_engine_api::config::EngineConfig, key: &str) -> bool {
    cfg.options.get(key).and_then(toml::Value::as_bool).unwrap_or(false)
}
