//! The effective-config document behind `poly config show`.
//!
//! `poly config show` used to print a section *summary* — `[lint]  python` named
//! the language and stopped — which left a user with no way to ask poly what it
//! actually parsed (issue #16). This module builds the answer: the whole merged
//! config, rendered as TOML so it diffs directly against the `poly.toml` the user
//! wrote, plus a resolution block for the facts that are not themselves config
//! (which files were merged, what `extends` resolved to).
//!
//! Two layers make up the document:
//!
//! - the **merged raw table** ([`PolyConfig::effective_table_with`]), which
//!   preserves every key exactly as written — including keys no typed field
//!   claims, which is what makes a discarded value visible; and
//! - an **overlay** of poly's resolved values for the four sections it
//!   normalizes with non-obvious defaults (`[defaults]`, `[discovery]`,
//!   `[rules]`, `[workspace]`), so a setting nobody wrote is still shown.
//!
//! The overlay writes individual keys rather than replacing the section, so an
//! unrecognized neighbour (`[discovery] excludes = …`) still shows up.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use poly_config::extends::ExtendsSource;
use poly_config::{LOCAL_OVERRIDE_NAME, LineEnding, PolyConfig};
use serde::Serialize;

use crate::config_sources::{self, RemoteExtendsResolver};

/// One `extends` base a config in the merge chain declared.
#[derive(Debug, Serialize)]
pub struct ExtendsEntry {
    /// `"path"` for a local base, `"git"` for a remote one.
    kind: &'static str,
    /// The base's identifier — its `id`, path, or repository URL.
    id: String,
    /// The object ID a git base resolved to, or `None` for a local base or an
    /// unlocked symbolic revision.
    resolved: Option<String>,
}

/// Facts about *how* the config was resolved, which the config itself cannot
/// carry.
#[derive(Debug, Serialize)]
pub struct Resolution {
    /// The nearest config file governing the run, or `None` when no `poly.toml`
    /// was found and the config below is poly's built-in defaults.
    config_path: Option<String>,
    /// Every config file that fed the merge, farthest (weakest) first.
    merged_from: Vec<String>,
    /// The `extends` bases declared by those files, in declared order.
    extends: Vec<ExtendsEntry>,
    /// Whether a `[hooks]` section survived the merge.
    hooks_present: bool,
}

/// The `poly config show` document: the effective config plus its resolution.
#[derive(Debug, Serialize)]
pub struct EffectiveConfig {
    /// The fully-merged config, with poly's resolved defaults overlaid.
    config: toml::Table,
    /// How that config was arrived at.
    resolution: Resolution,
}

impl EffectiveConfig {
    /// Resolve the effective config for `explicit` (or, when `None`, for the
    /// working directory) and describe how it was merged.
    pub fn resolve(explicit: Option<&Path>, resolver: &RemoteExtendsResolver) -> Result<Self> {
        let (config, mut table, sources) = match explicit {
            Some(path) => (
                PolyConfig::load_file_with(path, resolver)?,
                PolyConfig::effective_file_table_with(path, resolver)?,
                single_source(path),
            ),
            None => {
                let cwd = std::env::current_dir().context("resolving the working directory")?;
                (
                    PolyConfig::load_with(&cwd, resolver)?,
                    PolyConfig::effective_table_with(&cwd, resolver)?,
                    merged_sources(&cwd, resolver)?,
                )
            }
        };
        overlay_resolved_sections(&config, &mut table);

        // Naming a `poly.toml` that does not exist would read as if it had been
        // used; `None` says plainly that these are the built-in defaults.
        let config_path = sources
            .iter()
            .rev()
            .find(|path| path.file_name().is_some_and(|name| name != LOCAL_OVERRIDE_NAME))
            .map(|path| display(path));

        Ok(EffectiveConfig {
            config: table,
            resolution: Resolution {
                config_path,
                merged_from: sources.iter().map(|path| display(path)).collect(),
                extends: extends_entries(&sources, resolver),
                hooks_present: config.hooks.present,
            },
        })
    }

    /// Render as a TOML document: the resolution block as leading comments, the
    /// effective config as the body. Deterministic — the table is key-sorted.
    pub fn to_toml(&self) -> Result<String> {
        let mut out = String::new();
        for line in self.header_lines() {
            if line.is_empty() {
                out.push_str("#\n");
            } else {
                out.push_str("# ");
                out.push_str(&line);
                out.push('\n');
            }
        }
        out.push('\n');
        out.push_str(&toml::to_string_pretty(&self.config).context("rendering the effective config as TOML")?);
        Ok(out)
    }

    /// The comment header: what this document is, and how it was resolved.
    fn header_lines(&self) -> Vec<String> {
        let resolution = &self.resolution;
        let mut lines = vec![
            "poly effective configuration".to_string(),
            String::new(),
            "What poly resolved and will act on, after `extends` bases, the poly.toml".to_string(),
            "cascade and poly.local.toml were merged. Keys poly does not recognize are".to_string(),
            "shown as written, so this document diffs against your own config.".to_string(),
            String::new(),
            match &resolution.config_path {
                Some(path) => format!("config:      {path}"),
                None => "config:      (none found — poly's built-in defaults)".to_string(),
            },
        ];
        lines.extend(labeled("merged from:", resolution.merged_from.iter().cloned()));
        lines.extend(labeled(
            "extends:",
            resolution.extends.iter().map(|entry| match &entry.resolved {
                Some(oid) => format!("{} {} -> {oid}", entry.kind, entry.id),
                None => format!("{} {}", entry.kind, entry.id),
            }),
        ));
        lines.push(format!(
            "hooks:       {}",
            if resolution.hooks_present { "present" } else { "absent" }
        ));
        lines
    }
}

/// Render `label` against the first entry and indent the rest under it; nothing
/// at all when `entries` is empty.
fn labeled(label: &str, entries: impl Iterator<Item = String>) -> Vec<String> {
    let indent = " ".repeat(label.len());
    entries
        .enumerate()
        .map(|(index, entry)| {
            let prefix = if index == 0 { label } else { indent.as_str() };
            format!("{prefix} {entry}")
        })
        .collect()
}

/// Overlay poly's resolved values for the sections it normalizes with defaults a
/// user never sees in their own file. Keys are written individually so an
/// unrecognized neighbour in the same section survives.
fn overlay_resolved_sections(config: &PolyConfig, table: &mut toml::Table) {
    let defaults = &config.defaults;
    let line_ending = match defaults.line_ending {
        LineEnding::Lf => "lf",
        LineEnding::Crlf => "crlf",
    };
    overlay(
        table,
        "defaults",
        [
            ("line_length", toml::Value::Integer(defaults.line_length as i64)),
            ("line_ending", toml::Value::String(line_ending.to_string())),
            ("final_newline", toml::Value::Boolean(defaults.final_newline)),
            (
                "trim_trailing_whitespace",
                toml::Value::Boolean(defaults.trim_trailing_whitespace),
            ),
        ],
    );

    let discovery = &config.discovery;
    overlay(
        table,
        "discovery",
        [
            ("exclude", strings(discovery.exclude.as_slice())),
            ("no_prune", strings(discovery.no_prune.as_slice())),
            ("force_exclude", toml::Value::Boolean(discovery.force_exclude)),
            ("generated", toml::Value::Boolean(discovery.generated)),
        ],
    );

    overlay(
        table,
        "rules",
        [
            ("dirs", strings(&config.rules.dirs)),
            ("builtin", toml::Value::Boolean(config.rules.builtin)),
        ],
    );

    overlay(
        table,
        "workspace",
        [("root", toml::Value::Boolean(config.workspace.root))],
    );
}

/// Write `entries` into `table[section]`, creating the section if needed and
/// leaving any key not named in `entries` untouched.
fn overlay<const N: usize>(table: &mut toml::Table, section: &str, entries: [(&str, toml::Value); N]) {
    let target = table
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(target) = target.as_table_mut() else {
        // The user wrote a scalar where a section belongs; leave it visible
        // rather than overwriting the evidence.
        return;
    };
    for (key, value) in entries {
        target.insert(key.to_string(), value);
    }
}

fn strings(values: &[String]) -> toml::Value {
    toml::Value::Array(values.iter().map(|value| toml::Value::String(value.clone())).collect())
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// The config files an explicit `--config` path merges: the file itself and its
/// sibling `poly.local.toml`.
fn single_source(path: &Path) -> Vec<PathBuf> {
    let mut sources = vec![path.to_path_buf()];
    if let Some(parent) = path.parent() {
        let local = parent.join(LOCAL_OVERRIDE_NAME);
        if local.is_file() {
            sources.push(local);
        }
    }
    sources
}

/// Every config file that fed the merge for `dir`, farthest (weakest) first.
///
/// Mirrors [`PolyConfig::load_with`]'s own dispatch: inside a git repository the
/// hierarchical cascade (ADR 0018) merges the whole ancestor chain, and outside
/// one only the nearest `poly.toml` is loaded. Listing the chain is file-level
/// attribution — it names what *could* have contributed a value, not which file
/// each key came from.
fn merged_sources(dir: &Path, resolver: &RemoteExtendsResolver) -> Result<Vec<PathBuf>> {
    let dirs = if git_root(dir).is_some() {
        poly_config::config_chain_dirs_with(dir, resolver)?
    } else {
        match poly_config::find_config(dir) {
            Some(path) => vec![path.parent().unwrap_or(&path).to_path_buf()],
            None => Vec::new(),
        }
    };
    let mut sources = Vec::new();
    // The chain is nearest-first; the weakest layer reads first in a merge list.
    for config_dir in dirs.into_iter().rev() {
        for name in [poly_config::CONFIG_FILE_NAMES[0], LOCAL_OVERRIDE_NAME] {
            let candidate = config_dir.join(name);
            if candidate.is_file() {
                sources.push(candidate);
            }
        }
    }
    Ok(sources)
}

/// The nearest ancestor of `dir` (inclusive) containing `.git`.
fn git_root(dir: &Path) -> Option<PathBuf> {
    let mut current = Some(dir.to_path_buf());
    while let Some(candidate) = current {
        if candidate.join(".git").exists() {
            return Some(candidate);
        }
        current = candidate.parent().map(Path::to_path_buf);
    }
    None
}

/// The `extends` bases declared across every merged config file, in the order
/// they are merged. A base whose declaration cannot be parsed is skipped: the
/// loader already succeeded, so a failure here is about a file that no longer
/// exists, not about the config poly acted on.
fn extends_entries(sources: &[PathBuf], resolver: &RemoteExtendsResolver) -> Vec<ExtendsEntry> {
    let mut entries = Vec::new();
    for source in sources {
        let Ok(declared) = config_sources::declared_extends(source) else {
            continue;
        };
        entries.extend(declared.iter().map(|base| entry_for(base, resolver)));
    }
    entries
}

fn entry_for(source: &ExtendsSource, resolver: &RemoteExtendsResolver) -> ExtendsEntry {
    if source.git.is_some() {
        ExtendsEntry {
            kind: "git",
            id: source.display_id(),
            resolved: Some(
                resolver
                    .resolved_oid(source)
                    .unwrap_or_else(|| "<unlocked>".to_string()),
            ),
        }
    } else {
        ExtendsEntry {
            kind: "path",
            id: source.display_id(),
            resolved: None,
        }
    }
}
