//! Synchronous engine operations shared by every MCP tool.
//!
//! These functions are deliberately free of any `rmcp`/`tokio` types: they run
//! the same `poly-core` pipeline the CLI runs and return **typed** results — the
//! `poly-core` *run* structs for lint/format (results **plus** the files the run
//! failed on or declined), and the MCP-local DTOs in
//! [`crate::dto`] for cache/rules/config/workspace. The async tool handlers in
//! [`crate::server`] serialize them to structured content and call these from a
//! blocking task so the synchronous, rayon-driven engine never runs on a tokio
//! worker thread.
//!
//! Config resolution is **network-free**: local `extends` bases resolve as
//! usual, but remote **git** `extends` bases are not fetched (the git resolver
//! lives in `poly-cli`, which the server cannot depend on without a dependency
//! cycle). A repo whose config extends a remote git base should be served via
//! the `poly` CLI. This is a documented v1 limitation (ADR 0020).

use std::path::{Path, PathBuf};

use poly_cache::{ResultCache, root_from_cwd};
use poly_config::PolyConfig;
use poly_core::engines::astgrep::rules::load_flat;
use poly_core::engines::astgrep::test::{CaseKind, run_tests};
use poly_core::{Config, FormatRun, LintRun, RunOptions};

use crate::dto::{
    CacheCleanReport, CacheNamespace, CacheStatsReport, ConfigDefaults, ConfigShowReport, RuleInfo, RuleTestOutcome,
    RulesReport, WorkspaceReport, WorkspaceToolReport,
};

/// Resolve the `poly-core` run configuration the way the CLI does, but
/// network-free (see the module docs): load an explicit file when supplied,
/// otherwise discover `poly.toml` from the working directory.
pub fn resolve_config(explicit: Option<&Path>) -> anyhow::Result<Config> {
    match explicit {
        Some(path) => Config::load_file(path),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Config::load(&cwd)
        }
    }
}

/// Resolve the full parsed [`PolyConfig`] (needed by `config_show` and the
/// whole-project tools), network-free — mirroring [`resolve_config`].
pub fn resolve_poly_config(explicit: Option<&Path>) -> anyhow::Result<PolyConfig> {
    match explicit {
        Some(path) => PolyConfig::load_file(path),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            PolyConfig::load(&cwd)
        }
    }
}

/// Turn the request's path list into concrete paths, defaulting to the current
/// directory when the caller passes none (matching the CLI default).
fn resolve_paths(paths: &[String]) -> Vec<PathBuf> {
    if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.iter().map(PathBuf::from).collect()
    }
}

/// The run options every path-oriented tool uses, differing only in the caller's
/// exclude globs and whether a config file was named explicitly.
fn run_options(exclude: &[String], explicit_config: bool) -> RunOptions {
    RunOptions {
        exclude: exclude.to_vec(),
        force_exclude: false,
        fix_generated: false,
        explicit_config,
        ..RunOptions::default()
    }
}

/// Lint `paths`, returning the **whole run** — the per-file results plus the
/// files the run failed on and the ones it declined. When `fix` is true,
/// available autofixes are applied in place before the remaining diagnostics are
/// collected.
///
/// Deliberately [`poly_core::lint_run`] rather than `poly_core::lint`: the
/// latter returns only the results and drops [`LintRun::errors`], which for an
/// MCP caller — who has no exit code to fall back on — turns a file poly failed
/// to read into a clean-looking answer.
pub fn lint_run(paths: &[String], exclude: &[String], config: Option<&str>, fix: bool) -> anyhow::Result<LintRun> {
    let explicit_config = config.is_some();
    let config = resolve_config(config.map(Path::new))?;
    let resolved = resolve_paths(paths);
    poly_core::lint_run(&resolved, &config, &run_options(exclude, explicit_config), fix, false)
}

/// Format `paths`, returning the **whole run** (see [`lint_run`] for why the
/// run and not just the results). When `write` is true, changed files are
/// rewritten in place; otherwise this is a dry run.
pub fn format_run(
    paths: &[String],
    exclude: &[String],
    config: Option<&str>,
    write: bool,
) -> anyhow::Result<FormatRun> {
    let explicit_config = config.is_some();
    let config = resolve_config(config.map(Path::new))?;
    let resolved = resolve_paths(paths);
    poly_core::format_run(&resolved, &config, &run_options(exclude, explicit_config), write, false)
}

/// Open the result cache the way `poly cache` does: honor `[cache] dir` from the
/// resolved config, else fall back to the default per-repo anchor walk (which
/// respects `POLY_CACHE_HOME`). Network-free, matching the rest of this module.
fn open_result_cache() -> anyhow::Result<ResultCache> {
    let root = match resolve_poly_config(None)?.cache.dir {
        Some(dir) => PathBuf::from(dir),
        None => root_from_cwd()?,
    };
    ResultCache::open(root, true)
}

/// Report cache footprint (mirrors `poly cache stats`).
pub fn cache_stats() -> anyhow::Result<CacheStatsReport> {
    let cache = open_result_cache()?;
    let stats = cache.stats()?;
    let per_namespace = stats
        .per_namespace
        .iter()
        .map(|ns| CacheNamespace {
            namespace: ns.namespace.as_dir().to_string(),
            entries: ns.entries,
            bytes: ns.bytes,
        })
        .collect();
    Ok(CacheStatsReport {
        format_version: stats.format_version.clone(),
        on_disk_version: stats.on_disk_version.clone(),
        total_bytes: stats.total_bytes,
        per_namespace,
    })
}

/// Remove every cached entry (mirrors `poly cache clean`) and report freed bytes.
pub fn cache_clean() -> anyhow::Result<CacheCleanReport> {
    let cache = open_result_cache()?;
    let freed = cache.clean()?;
    Ok(CacheCleanReport { freed_bytes: freed })
}

/// List (and optionally test) the custom ast-grep rule packs (mirrors `poly
/// rules`). `dirs` defaults to `[rules] dirs` from the resolved config when
/// empty; when `test` is true the rule-test snippets are run too.
pub fn rules_report(dirs: &[String], config: Option<&str>, test: bool) -> anyhow::Result<RulesReport> {
    let dirs = if dirs.is_empty() {
        resolve_poly_config(config.map(Path::new))?.rules.dirs
    } else {
        dirs.to_vec()
    };

    let rules = load_flat(&dirs)?
        .iter()
        .map(|rule| RuleInfo {
            id: rule.id.clone(),
            language: rule.language.name().to_string(),
            severity: format!("{:?}", rule.severity),
        })
        .collect();

    let mut report = RulesReport {
        dirs: dirs.clone(),
        rules,
        tests: None,
        missing_rule_ids: None,
        untested_rule_ids: None,
        ok: None,
    };

    if test {
        let test_report = run_tests(&dirs)?;
        report.ok = Some(test_report.is_ok());
        report.tests = Some(
            test_report
                .outcomes
                .iter()
                .map(|outcome| RuleTestOutcome {
                    rule_id: outcome.rule_id.clone(),
                    kind: match outcome.kind {
                        CaseKind::Valid => "valid",
                        CaseKind::Invalid => "invalid",
                        CaseKind::Fixed => "fixed",
                    }
                    .to_string(),
                    index: outcome.index,
                    passed: outcome.passed,
                    detail: outcome.detail.clone(),
                })
                .collect(),
        );
        report.missing_rule_ids = Some(test_report.missing_rule_ids.clone());
        report.untested_rule_ids = Some(test_report.untested_rule_ids.clone());
    }

    Ok(report)
}

/// Summarize the effective, merged config (mirrors `poly config show`), resolved
/// network-free.
pub fn config_show(config: Option<&str>) -> anyhow::Result<ConfigShowReport> {
    let resolved = resolve_poly_config(config.map(Path::new))?;
    let defaults = &resolved.defaults;
    Ok(ConfigShowReport {
        config_path: config.unwrap_or("<discovered>").to_string(),
        defaults: ConfigDefaults {
            line_length: defaults.line_length,
            line_ending: format!("{:?}", defaults.line_ending),
            final_newline: defaults.final_newline,
            trim_trailing_whitespace: defaults.trim_trailing_whitespace,
        },
        lint_keys: resolved.lint.keys().cloned().collect(),
        fmt_keys: resolved.fmt.keys().cloned().collect(),
        tools: resolved.tools.iter().map(|(name, _)| name.to_string()).collect(),
        hooks_present: resolved.hooks.present,
        rule_dirs: resolved.rules.dirs.clone(),
    })
}

/// Run the whole-project (workspace) lint phase against the live worktree
/// (mirrors `poly lint`'s whole-project phase). When `fix` is true the tools run
/// in fix mode. Reporting is captured, never written to stdout, so the caller's
/// stdio MCP transport stays a clean JSON-RPC stream.
pub fn workspace_lint(
    config: Option<&str>,
    fix: bool,
    jobs: Option<usize>,
    no_cache: bool,
) -> anyhow::Result<WorkspaceReport> {
    let resolved = resolve_poly_config(config.map(Path::new))?;
    let outcome = poly_workspace::run_workspace_lint(
        &resolved,
        &poly_workspace::WorkspaceLintOptions {
            fix,
            jobs,
            no_cache,
            report_to_stdout: false,
        },
    )?;
    Ok(WorkspaceReport {
        passed: outcome.passed,
        fixed: fix,
        tools: outcome
            .tools
            .iter()
            .map(|tool| WorkspaceToolReport {
                id: tool.id.clone(),
                failed: tool.failed,
                cached: tool.cached,
                output: String::from_utf8_lossy(&tool.output).into_owned(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_stats_reports_namespaces() {
        let report = cache_stats().unwrap();
        // The stats always carry the current format version.
        assert!(!report.format_version.is_empty(), "format version is populated");
    }

    #[test]
    fn resolve_poly_config_is_reachable() {
        // A missing explicit config is an error, exercising the PolyConfig path.
        let err = resolve_poly_config(Some(Path::new("/nonexistent/poly.toml")));
        assert!(err.is_err(), "missing explicit config should error");
    }
}
