//! Synchronous engine operations behind the host services.
//!
//! These functions are deliberately free of any `rmcp`/`tokio` types: they run
//! the same `poly-core` pipeline the CLI runs and return **typed** results — the
//! `poly-core` *run* structs for lint/format (results **plus** the files the run
//! failed on or declined), and the report types in
//! [`poly_frontend_api::dto`] for cache/rules/config/workspace. The MCP
//! frontend reaches them through the host services in [`crate::Services`] and
//! calls those from a blocking task, so the synchronous, rayon-driven engine
//! never runs on a tokio worker thread.
//!
//! Config resolution is **network-free**: local `extends` bases resolve as
//! usual, but remote **git** `extends` bases are not fetched (the git resolver
//! lives in `poly-cli`, which this crate cannot depend on without a dependency
//! cycle). A repo whose config extends a remote git base should be served via
//! the `poly` CLI. This is a documented v1 limitation (ADR 0020).

use std::path::{Path, PathBuf};

use poly_cache::{ResultCache, root_from_cwd};
use poly_config::PolyConfig;
use poly_core::{Config, FormatRun, Kind, Language, LintRun, RunOptions};

use poly_config::v2::{ComponentInstallPolicy, OnMissing};
use poly_frontend_api::dto::{
    CacheCleanReport, CacheNamespace, CacheStatsReport, ConfigDefaults, ConfigShowReport, RulesReport, WorkspaceReport,
    WorkspaceToolReport,
};
use poly_frontend_api::wire::InstallMode;

/// Resolve the `poly-core` run configuration the way the CLI does, but
/// network-free (see the module docs): load an explicit file when supplied,
/// otherwise discover `poly.toml` from the working directory.
pub fn resolve_config(explicit: Option<&Path>) -> anyhow::Result<Config> {
    install_tool_lock(explicit)?;
    match explicit {
        Some(path) => Config::load_file(path),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Config::load(&cwd)
        }
    }
}

/// Make the repository's `poly.lock` tool pins visible to the tool provisioner,
/// as the CLI's config resolver does, so a run served here downloads the locked
/// releases rather than resolving `latest` afresh.
fn install_tool_lock(explicit: Option<&Path>) -> anyhow::Result<()> {
    let start = match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    poly_config::tool_lock::install_from_repo(&start)
}

/// Resolve the full parsed [`PolyConfig`] (needed by `config_show` and the
/// whole-project tools), network-free — mirroring [`resolve_config`].
pub fn resolve_poly_config(explicit: Option<&Path>) -> anyhow::Result<PolyConfig> {
    install_tool_lock(explicit)?;
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
///
/// `force_exclude` is read from the resolved config rather than written as a
/// literal. The CLI resolves the same question in one place — `--include-excluded`,
/// then `[discovery] force_exclude` (which `--set` can override), then the built-in
/// default — and this surface has no flags, so only the last two steps can apply.
/// Reading the key *is* that resolution here: it defaults to `true` in
/// [`poly_config::DiscoveryConfig`], so an unset key still yields the CLI's
/// default and only a repo that deliberately turned it off sees a change.
///
/// It was previously hardcoded `true`, which made the key inert for every MCP
/// caller: one `poly.toml` answered differently depending on which surface asked.
fn run_options(config: &Config, exclude: &[String], explicit_config: bool, install: InstallMode) -> RunOptions {
    let (install, on_missing) = install_policy(install);
    RunOptions {
        exclude: exclude.to_vec(),
        force_exclude: config.force_exclude,
        fix_generated: false,
        explicit_config,
        install,
        on_missing,
        ..RunOptions::default()
    }
}

/// The run overrides an `install` parameter stands for. `never` also makes a missing component a
/// skip rather than a failure, since the caller asked for no download and would otherwise get an
/// error for honouring it; `config` and `auto` leave `on_missing` to the config.
#[must_use]
pub fn install_policy(mode: InstallMode) -> (Option<ComponentInstallPolicy>, Option<OnMissing>) {
    match mode {
        InstallMode::Config => (None, None),
        InstallMode::Auto => (Some(ComponentInstallPolicy::Auto), None),
        InstallMode::Never => (Some(ComponentInstallPolicy::Never), Some(OnMissing::Skip)),
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
    lint_run_with(paths, exclude, config, fix, InstallMode::Config)
}

/// [`lint_run`] under an explicit component install policy.
pub fn lint_run_with(
    paths: &[String],
    exclude: &[String],
    config: Option<&str>,
    fix: bool,
    install: InstallMode,
) -> anyhow::Result<LintRun> {
    let explicit_config = config.is_some();
    let config = resolve_config(config.map(Path::new))?;
    let resolved = resolve_paths(paths);
    poly_core::lint_run(
        &resolved,
        &config,
        &run_options(&config, exclude, explicit_config, install),
        fix,
        false,
    )
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
    format_run_with(paths, exclude, config, write, InstallMode::Config)
}

/// [`format_run`] under an explicit component install policy.
pub fn format_run_with(
    paths: &[String],
    exclude: &[String],
    config: Option<&str>,
    write: bool,
    install: InstallMode,
) -> anyhow::Result<FormatRun> {
    let explicit_config = config.is_some();
    let config = resolve_config(config.map(Path::new))?;
    let resolved = resolve_paths(paths);
    poly_core::format_run(
        &resolved,
        &config,
        &run_options(&config, exclude, explicit_config, install),
        write,
        false,
    )
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

/// List (and optionally test) the ast-grep rules a run would apply (mirrors
/// `poly rules`): poly's **built-in rule pack** plus the user rules in `dirs`,
/// which defaults to `[rules] dirs` from the resolved config when empty. When
/// `test` is true the user rule-test snippets are run too.
///
/// The listing goes through the ast-grep engine's own merge + selection path, the
/// one `lint` runs, so it reflects `[rules] builtin` and the `[lint.astgrep]`
/// `select` / `extend_select` / `ignore` / per-rule `level` keys rather than
/// reporting a raw on-disk inventory. A build that links the engine family
/// answers in-process; a core build asks the `astgrep` component (see
/// [`rules_report_for`]).
pub fn rules_report(dirs: &[String], config: Option<&str>, test: bool) -> anyhow::Result<RulesReport> {
    let resolved = resolve_config(config.map(Path::new))?;
    rules_report_for(&resolved, dirs, test, None)
}

/// [`rules_report`] over an already resolved config.
///
/// With the ast-grep engine family linked the report is built in-process; otherwise the
/// `astgrep` component is made available (honouring `--offline` and `[components] install`)
/// and answers `Request::Rules`. Either way the answer is the same
/// [`poly_component_protocol::WireRulesReport`], converted by one function, so the two
/// flavours cannot drift. `injection` replaces the component host and index (tests).
pub fn rules_report_for(
    resolved: &Config,
    dirs: &[String],
    test: bool,
    injection: Option<&poly_core::components::ComponentInjection>,
) -> anyhow::Result<RulesReport> {
    let engine_config = resolved.engine_config(&Language::Other("astgrep".to_string()), "astgrep", Kind::Lint);
    let dirs = if dirs.is_empty() {
        resolved.rules_dirs.clone()
    } else {
        dirs.to_vec()
    };
    let wire = rules_wire(resolved, &dirs, &engine_config, test, injection)?;
    Ok(rules_report_from_wire(dirs, wire))
}

#[cfg(feature = "engine-astgrep")]
fn rules_wire(
    resolved: &Config,
    dirs: &[String],
    engine_config: &poly_core::config::EngineConfig,
    test: bool,
    injection: Option<&poly_core::components::ComponentInjection>,
) -> anyhow::Result<poly_component_protocol::WireRulesReport> {
    if injection.is_some() {
        return rules_wire_via_component(resolved, dirs, engine_config, test, injection);
    }
    poly_core::engines::astgrep::report::rules_report(dirs, engine_config, test)
}

#[cfg(not(feature = "engine-astgrep"))]
fn rules_wire(
    resolved: &Config,
    dirs: &[String],
    engine_config: &poly_core::config::EngineConfig,
    test: bool,
    injection: Option<&poly_core::components::ComponentInjection>,
) -> anyhow::Result<poly_component_protocol::WireRulesReport> {
    rules_wire_via_component(resolved, dirs, engine_config, test, injection)
}

fn rules_wire_via_component(
    resolved: &Config,
    dirs: &[String],
    engine_config: &poly_core::config::EngineConfig,
    test: bool,
    injection: Option<&poly_core::components::ComponentInjection>,
) -> anyhow::Result<poly_component_protocol::WireRulesReport> {
    use poly_component_protocol::{Payload, Request};
    use poly_core::components::call_engine_component;

    let request = Request::Rules {
        dirs: dirs.to_vec(),
        config: poly_engine_api::component_wire::engine_config_to_wire(engine_config),
        test,
    };
    match call_engine_component(injection, resolved, "astgrep", &request)? {
        Payload::Rules(report) => Ok(report),
        other => anyhow::bail!("the astgrep component answered Rules with an unexpected payload: {other:?}"),
    }
}

/// The one conversion from the engine's report to the frontend DTO.
pub fn rules_report_from_wire(dirs: Vec<String>, wire: poly_component_protocol::WireRulesReport) -> RulesReport {
    use poly_frontend_api::dto::{RuleInfo, RuleTestOutcome};

    let rules = wire
        .rules
        .into_iter()
        .map(|rule| RuleInfo {
            id: rule.id,
            language: rule.language,
            source: rule.source,
            default_severity: rule.default_severity,
            severity: rule.severity,
            enabled: rule.enabled,
        })
        .collect();
    let mut report = RulesReport {
        dirs,
        builtin_pack_enabled: wire.builtin_pack_enabled,
        rules,
        tests: None,
        total_rules: None,
        missing_rule_ids: None,
        untested_rule_ids: None,
        ok: None,
    };
    if let Some(tests) = wire.tests {
        report.ok = Some(tests.outcomes.iter().all(|outcome| outcome.passed) && tests.missing_rule_ids.is_empty());
        report.total_rules = Some(tests.total_rules as usize);
        report.tests = Some(
            tests
                .outcomes
                .into_iter()
                .map(|outcome| RuleTestOutcome {
                    rule_id: outcome.rule_id,
                    kind: outcome.kind,
                    index: outcome.index as usize,
                    passed: outcome.passed,
                    detail: outcome.detail,
                })
                .collect(),
        );
        report.missing_rule_ids = Some(tests.missing_rule_ids);
        report.untested_rule_ids = Some(tests.untested_rule_ids);
    }
    report
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
    fn loading_a_config_installs_the_repositorys_tool_lock() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let config = repo.path().join("poly.toml");
        std::fs::write(&config, "").unwrap();
        std::fs::write(
            repo.path().join("poly.lock"),
            "version = 1\n[[tools]]\nname = \"mcp-locked-tool\"\nversion = \"3.2.1\"\n\
             [[tools.assets]]\nplatform = \"any\"\nurl = \"https://example.invalid/t\"\nsha256 = \"aa\"\n",
        )
        .unwrap();
        resolve_config(Some(&config)).unwrap();
        let locked = poly_config::tool_lock::get("mcp-locked-tool");
        resolve_poly_config(Some(&config)).unwrap();
        poly_config::tool_lock::install(Vec::new());
        assert_eq!(locked.expect("the MCP config path installs poly.lock").version, "3.2.1");
    }

    #[test]
    fn resolve_poly_config_is_reachable() {
        // A missing explicit config is an error, exercising the PolyConfig path.
        let err = resolve_poly_config(Some(Path::new("/nonexistent/poly.toml")));
        assert!(err.is_err(), "missing explicit config should error");
    }

    #[test]
    fn the_install_parameter_maps_to_the_run_overrides() {
        assert_eq!(install_policy(InstallMode::Config), (None, None));
        assert_eq!(
            install_policy(InstallMode::Auto),
            (Some(ComponentInstallPolicy::Auto), None)
        );
        assert_eq!(
            install_policy(InstallMode::Never),
            (Some(ComponentInstallPolicy::Never), Some(OnMissing::Skip)),
            "never downloads nothing and skips rather than fails"
        );
    }
}
