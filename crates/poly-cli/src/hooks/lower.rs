//! Lower a parsed `[hooks]` [`HooksConfig`] into the native `poly-hooks` model.
//!
//! For a requested git stage this produces a single [`StageSpec`] whose hook
//! list unifies, in priority order:
//!
//! 1. poly's **builtins** (`[hooks.builtin]`) whose resolved stages include the
//!    requested stage,
//! 2. the stage's **inline jobs** (`[[hooks.<stage>.jobs]]` / `.commands` /
//!    `.scripts`), and
//! 3. the **`[hooks.always]`** pseudo-stage jobs, appended to every concrete
//!    stage (lefthook "run everywhere").
//!
//! `[hooks.always]` is config-only — it never maps to a [`HookStage`]; its jobs
//! are expanded into each concrete stage during lowering.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use poly_config::{
    Guard, HookCacheMode, HooksConfig, Job, Patterns, Stage as ConfigStage, StageConfig,
};
use poly_hooks::Stage as HookStage;
use poly_hooks::filter::FilePattern;
use poly_hooks::identify::TagSet;
use poly_hooks::model::{Hook, HookCommand, StageSpec};

use self::builtins::{PathProbe, ToolProbe};

mod builtins;
mod cache;

/// Map a config [`ConfigStage`] to the runner's [`HookStage`].
///
/// Total **except** [`ConfigStage::Always`], which is an expand-only
/// pseudo-stage (its jobs are appended to every concrete stage during lowering)
/// and therefore has no runner counterpart — it returns `None`.
#[must_use]
pub fn to_hook_stage(stage: ConfigStage) -> Option<HookStage> {
    Some(match stage {
        ConfigStage::PreCommit => HookStage::PreCommit,
        ConfigStage::PreMergeCommit => HookStage::PreMergeCommit,
        ConfigStage::PrepareCommitMsg => HookStage::PrepareCommitMsg,
        ConfigStage::CommitMsg => HookStage::CommitMsg,
        ConfigStage::PostCommit => HookStage::PostCommit,
        ConfigStage::PreRebase => HookStage::PreRebase,
        ConfigStage::PostCheckout => HookStage::PostCheckout,
        ConfigStage::PostMerge => HookStage::PostMerge,
        ConfigStage::PrePush => HookStage::PrePush,
        ConfigStage::PostRewrite => HookStage::PostRewrite,
        ConfigStage::Manual => HookStage::Manual,
        ConfigStage::Always => return None,
    })
}

/// The config [`ConfigStage`] that a runner [`HookStage`] corresponds to.
///
/// Total over the runner's stages. The trailing wildcard exists only because
/// [`HookStage`] is `#[non_exhaustive]`; every variant in scope is mapped
/// explicitly and the stage-parity test guards against drift.
#[must_use]
pub fn from_hook_stage(stage: HookStage) -> ConfigStage {
    match stage {
        HookStage::PreCommit => ConfigStage::PreCommit,
        HookStage::PreMergeCommit => ConfigStage::PreMergeCommit,
        HookStage::PrepareCommitMsg => ConfigStage::PrepareCommitMsg,
        HookStage::CommitMsg => ConfigStage::CommitMsg,
        HookStage::PostCommit => ConfigStage::PostCommit,
        HookStage::PreRebase => ConfigStage::PreRebase,
        HookStage::PostCheckout => ConfigStage::PostCheckout,
        HookStage::PostMerge => ConfigStage::PostMerge,
        HookStage::PrePush => ConfigStage::PrePush,
        HookStage::PostRewrite => ConfigStage::PostRewrite,
        HookStage::Manual => ConfigStage::Manual,
        _ => ConfigStage::Manual,
    }
}

/// Build the [`StageSpec`] for `stage` from the parsed `[hooks]` config.
///
/// `poly_bin` is the absolute path of the running `poly` binary (used as the
/// entry for builtins); `files` is the resolved candidate file set, used only
/// for `{staged_files}` / `{all_files}` template substitution; `cache_mode` is
/// the global `[cache.results] hooks` mode, used to resolve each hook's
/// effective result-cache policy.
///
/// # Errors
///
/// Returns `Err` if a builtin's configured stage name is invalid, or a job's
/// file glob (or cache-input glob) fails to compile.
pub fn lower_stage(
    hooks: &HooksConfig,
    poly_bin: &Path,
    stage: HookStage,
    files: &[PathBuf],
    cache_mode: &HookCacheMode,
    root: &Path,
) -> Result<StageSpec> {
    lower_stage_with_probe(
        hooks,
        poly_bin,
        stage,
        files,
        cache_mode,
        &PathProbe { root },
    )
}

/// [`lower_stage`] with an injectable capability [`ToolProbe`].
///
/// `lower_stage` calls this with the production [`PathProbe`]; tests pass a stub
/// so Cargo-builtin gating is deterministic regardless of the host toolchain.
fn lower_stage_with_probe(
    hooks: &HooksConfig,
    poly_bin: &Path,
    stage: HookStage,
    files: &[PathBuf],
    cache_mode: &HookCacheMode,
    probe: &dyn ToolProbe,
) -> Result<StageSpec> {
    let config_stage = from_hook_stage(stage);
    let stage_config = hooks.stage_configs.get(&config_stage);

    // An unconditional stage-level `skip`/`only` guard yields an empty stage. It
    // is checked *before* anything is appended, so it also suppresses builtins.
    if stage_config.is_some_and(|cfg| guard_skips(cfg.skip.as_ref(), cfg.only.as_ref())) {
        return Ok(StageSpec {
            stage,
            ..StageSpec::default()
        });
    }

    let mut entries: Vec<Hook> = Vec::new();
    append_builtins(
        hooks,
        poly_bin,
        config_stage,
        cache_mode,
        probe,
        &mut entries,
    )?;

    if let Some(cfg) = stage_config {
        append_jobs(hooks, stage, cfg, files, cache_mode, &mut entries)?;
    }
    // `[hooks.always]` jobs are appended to every concrete stage.
    if let Some(always) = hooks.stage_configs.get(&ConfigStage::Always) {
        append_jobs(hooks, stage, always, files, cache_mode, &mut entries)?;
    }

    // Priority order; `sort_by_key` is stable, so equal-priority hooks keep
    // their insertion order (builtins, then inline jobs, then `always`).
    entries.sort_by_key(|hook| hook.priority);

    let (precondition, before, after) = stage_steps(stage_config);
    Ok(StageSpec {
        stage,
        precondition,
        before,
        after,
        hooks: entries,
    })
}

// ── Builtins ────────────────────────────────────────────────────────────────

/// Append poly's enabled builtins that resolve to `config_stage`.
///
/// Builtins use [`Hook::run`], whose `always_run` defaults to `false`: a builtin
/// is skipped when the matched file set is empty. This differs from an inline
/// job with no `files` filter (which sets `always_run = true`) — a deliberate
/// distinction, since `poly lint`/`fmt` over zero files is a no-op anyway.
fn append_builtins(
    hooks: &HooksConfig,
    poly_bin: &Path,
    config_stage: ConfigStage,
    cache_mode: &HookCacheMode,
    probe: &dyn ToolProbe,
    out: &mut Vec<Hook>,
) -> Result<()> {
    let poly = shell_quote(&poly_bin.to_string_lossy());

    if hooks.builtin.polylint.enabled
        && builtin_runs_on(
            &hooks.builtin.polylint.stages,
            &hooks.stages,
            ConfigStage::PreCommit,
            config_stage,
        )?
    {
        let mut hook = Hook::run("polylint", format!("{poly} lint"));
        hook.cache = cache::builtin_cache(cache_mode);
        out.push(hook);
    }
    if hooks.builtin.polyfmt.enabled
        && builtin_runs_on(
            &hooks.builtin.polyfmt.stages,
            &hooks.stages,
            ConfigStage::PreCommit,
            config_stage,
        )?
    {
        let mut hook = Hook::run("polyfmt", format!("{poly} fmt --check"));
        hook.cache = cache::builtin_cache(cache_mode);
        out.push(hook);
    }
    if hooks.builtin.commit.enabled
        && builtin_runs_on(
            &hooks.builtin.commit.stages,
            &hooks.stages,
            ConfigStage::CommitMsg,
            config_stage,
        )?
    {
        // `poly commit` consumes the commit-message file as its positional
        // argument, so it must run with `pass_filenames` (the matched "file" in
        // the runner's message-file mode is that path). `Hook::run` enables it.
        out.push(Hook::run("poly-commit", format!("{poly} commit")));
    }
    builtins::append_file_safety(hooks, &poly, config_stage, out)?;
    builtins::append_cargo(hooks, config_stage, probe, out)?;
    Ok(())
}

/// Whether a builtin runs on `config_stage`, given its own `stages`, the global
/// default `stages`, and the builtin's own fallback when both are empty.
pub(super) fn builtin_runs_on(
    own_stages: &[String],
    default_stages: &[String],
    fallback: ConfigStage,
    config_stage: ConfigStage,
) -> Result<bool> {
    let raw = if !own_stages.is_empty() {
        own_stages
    } else if !default_stages.is_empty() {
        default_stages
    } else {
        return Ok(config_stage == fallback);
    };
    for name in raw {
        let parsed: ConfigStage = name
            .parse()
            .with_context(|| format!("invalid builtin hook stage `{name}`"))?;
        if parsed == config_stage {
            return Ok(true);
        }
    }
    Ok(false)
}

// ── Inline jobs ───────────────────────────────────────────────────────────────

fn append_jobs(
    hooks: &HooksConfig,
    stage: HookStage,
    cfg: &StageConfig,
    files: &[PathBuf],
    cache_mode: &HookCacheMode,
    out: &mut Vec<Hook>,
) -> Result<()> {
    for (label, job) in cfg.labeled_jobs() {
        if guard_skips(job.skip.as_ref(), job.only.as_ref()) {
            continue;
        }
        if job_excluded_by_tags(job, &cfg.exclude_tags) {
            continue;
        }
        out.push(job_to_hook(
            hooks, stage, cfg, &label, job, files, cache_mode,
        )?);
    }
    Ok(())
}

fn job_to_hook(
    hooks: &HooksConfig,
    stage: HookStage,
    cfg: &StageConfig,
    label: &str,
    job: &Job,
    files: &[PathBuf],
    cache_mode: &HookCacheMode,
) -> Result<Hook> {
    // Per-job env merged over the global `[hooks].env` (job wins).
    let mut env: BTreeMap<String, String> = hooks.env.clone();
    env.extend(job.env.iter().map(|(k, v)| (k.clone(), v.clone())));

    let include = collect_patterns(&[cfg.files.as_ref(), job.files.as_ref(), job.glob.as_ref()]);
    let exclude = collect_patterns(&[cfg.exclude.as_ref(), job.exclude.as_ref()]);
    let has_include = !include.is_empty();
    let files_pattern = build_glob(include)?;
    let exclude_pattern = build_glob(exclude)?;

    let types = (!job.file_types.is_empty()).then(|| TagSet::from_tags(&job.file_types));

    // A `{staged_files}`/`{all_files}` template disables `pass_filenames`, so the
    // runner's own file filter never runs — scope the substituted set to the
    // job's include/exclude globs here instead.
    let scoped = filter_files(files, files_pattern.as_ref(), exclude_pattern.as_ref());
    let (command, pass_filenames) = build_command(job, &scoped)?;

    let cache = cache::job_cache(job, cache_mode)?;
    // Tier-2 sccache opt-in; only honoured when the run carries sccache settings.
    let compiler = job.cache.as_ref().is_some_and(|cache| cache.compiler);

    Ok(Hook {
        id: label.to_string(),
        stage,
        command,
        args: job.args.clone(),
        env,
        files: files_pattern,
        exclude: exclude_pattern,
        types,
        priority: job.priority,
        cache,
        compiler,
        // `parallel`/`piped` are stage-level in the lefthook schema. `piped`
        // (serial, abort-on-first-failure) maps to `require_serial` + `fail_fast`.
        parallel: cfg.parallel,
        require_serial: cfg.piped,
        fail_fast: cfg.piped,
        stage_fixed: job.stage_fixed,
        // A job with no include filter always runs (lefthook runs unfiltered
        // commands regardless of the changed-file set); a filtered job is
        // skipped when nothing matches.
        always_run: !has_include,
        pass_filenames,
        fail_text: job.fail_text.clone(),
        ..Hook::default()
    })
}

/// Build the [`HookCommand`] and resolve whether the runner should append the
/// matched files. A `run` command containing `{staged_files}` / `{all_files}`
/// has those tokens substituted here and does **not** receive appended files.
fn build_command(job: &Job, files: &[PathBuf]) -> Result<(HookCommand, bool)> {
    if let Some(run) = &job.run {
        if has_template(run) {
            return Ok((HookCommand::Run(substitute_templates(run, files)), false));
        }
        return Ok((HookCommand::Run(run.clone()), true));
    }
    if let Some(script) = &job.script {
        return Ok((
            HookCommand::Script {
                path: script.clone(),
                runner: job.runner.clone(),
            },
            true,
        ));
    }
    // `HooksConfig::validate` (run at config load) guarantees exactly one of
    // `run`/`script`; this arm is defensive.
    anyhow::bail!("hook job `{:?}` has neither `run` nor `script`", job.name)
}

// ── Guards, tags, patterns, templates ─────────────────────────────────────────

/// Resolve `skip`/`only` guards to a skip decision.
///
/// Only the unconditional [`Guard::Always`] form is evaluated here: a
/// conditional guard ([`Guard::Conditions`]) needs the live git-operation /
/// branch context, which is not available at lowering, so it is **deferred**
/// (treated as not-skipping). `skip = true` drops the item; `only = false`
/// drops it (it would run only when active, and it is never unconditionally
/// active).
fn guard_skips(skip: Option<&Guard>, only: Option<&Guard>) -> bool {
    if matches!(skip, Some(Guard::Always(true))) {
        return true;
    }
    if matches!(only, Some(Guard::Always(false))) {
        return true;
    }
    false
}

fn job_excluded_by_tags(job: &Job, exclude_tags: &[String]) -> bool {
    job.tags.iter().any(|tag| exclude_tags.contains(tag))
}

fn collect_patterns(sources: &[Option<&Patterns>]) -> Vec<String> {
    let mut out = Vec::new();
    for source in sources.iter().flatten() {
        out.extend(source.as_slice().iter().cloned());
    }
    out
}

fn build_glob(patterns: Vec<String>) -> Result<Option<FilePattern>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        FilePattern::glob(patterns).context("invalid hook file glob pattern")?,
    ))
}

/// Scope `files` to a job's compiled include/exclude globs.
///
/// A `None` include matches everything (an unfiltered job); `exclude` removes
/// matches. Used only for template substitution — when `pass_filenames` is left
/// enabled the runner applies the same filter itself.
fn filter_files(
    files: &[PathBuf],
    include: Option<&FilePattern>,
    exclude: Option<&FilePattern>,
) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|path| include.is_none_or(|pattern| pattern.is_match(path.as_path())))
        .filter(|path| exclude.is_none_or(|pattern| !pattern.is_match(path.as_path())))
        .cloned()
        .collect()
}

fn has_template(run: &str) -> bool {
    run.contains("{staged_files}") || run.contains("{all_files}")
}

/// Substitute `{staged_files}` / `{all_files}` with the shell-quoted, space-
/// joined candidate file set.
///
/// Both tokens resolve to the same resolved set (the caller decides whether that
/// is the staged or whole-tree set via `--all-files`). Deferred: per-job glob
/// filtering of the substituted list and other lefthook tokens
/// (`{push_files}`, `{cmd}`, …).
fn substitute_templates(run: &str, files: &[PathBuf]) -> String {
    let joined = files
        .iter()
        .map(|path| shell_quote(&path.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    run.replace("{staged_files}", &joined)
        .replace("{all_files}", &joined)
}

fn stage_steps(cfg: Option<&StageConfig>) -> (Option<String>, Vec<String>, Vec<String>) {
    match cfg {
        None => (None, Vec::new(), Vec::new()),
        Some(cfg) => (
            cfg.precondition.clone(),
            cfg.before.as_ref().map(patterns_to_vec).unwrap_or_default(),
            cfg.after.as_ref().map(patterns_to_vec).unwrap_or_default(),
        ),
    }
}

fn patterns_to_vec(patterns: &Patterns) -> Vec<String> {
    patterns.as_slice().to_vec()
}

/// Single-quote a string for safe interpolation into the `sh -c` command line
/// the runner builds. On Windows the runner uses `cmd /C`, which has no
/// single-quote semantics, so a double-quote wrap is used there.
#[cfg(not(windows))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(windows)]
fn shell_quote(value: &str) -> String {
    // `cmd /C` expands `%VAR%` even inside double quotes and treats `"` as a
    // token boundary, so a user-controlled path could inject or break the
    // command line. Double both before wrapping.
    let escaped = value.replace('%', "%%").replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use poly_config::PolyConfig;

    fn hooks_from(toml: &str) -> HooksConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        std::fs::write(&path, toml).unwrap();
        PolyConfig::load_file(&path).unwrap().hooks
    }

    fn poly() -> PathBuf {
        PathBuf::from("/opt/poly/bin/poly")
    }

    fn ids(spec: &StageSpec) -> Vec<String> {
        spec.hooks.iter().map(|h| h.id.clone()).collect()
    }

    /// Test-local `lower_stage` shadowing the production one (a local item wins
    /// over the `use super::*` glob): it routes lowering through a probe that
    /// reports no external tools, so the default-on `cargo` builtin group stays
    /// deterministic regardless of what the host has installed.
    fn lower_stage(
        hooks: &HooksConfig,
        poly_bin: &Path,
        stage: HookStage,
        files: &[PathBuf],
        cache_mode: &HookCacheMode,
    ) -> Result<StageSpec> {
        struct NoTools;
        impl ToolProbe for NoTools {
            fn is_available(&self, _tool: &str) -> bool {
                false
            }
            fn is_cargo_project(&self) -> bool {
                false
            }
        }
        lower_stage_with_probe(hooks, poly_bin, stage, files, cache_mode, &NoTools)
    }

    #[test]
    fn to_and_from_hook_stage_round_trip_for_every_runner_stage() {
        use clap::ValueEnum as _;
        for stage in HookStage::value_variants() {
            let config_stage = from_hook_stage(*stage);
            assert_eq!(
                to_hook_stage(config_stage),
                Some(*stage),
                "round-trip failed for {stage:?}"
            );
        }
    }

    #[test]
    fn always_pseudo_stage_has_no_runner_counterpart() {
        assert_eq!(to_hook_stage(ConfigStage::Always), None);
    }

    #[test]
    fn every_config_stage_maps_to_a_runner_stage_except_always() {
        for stage in ConfigStage::ALL {
            match stage {
                ConfigStage::Always => assert_eq!(to_hook_stage(*stage), None),
                other => assert!(
                    to_hook_stage(*other).is_some(),
                    "config stage {other} has no runner counterpart"
                ),
            }
        }
    }

    #[test]
    fn legacy_stage_aliases_agree_across_both_enums() {
        // Both enums accept the same pre-commit-style aliases; assert each
        // resolves to the same logical runner stage on both sides.
        for (alias, expected) in [
            ("commit", HookStage::PreCommit),
            ("push", HookStage::PrePush),
            ("merge-commit", HookStage::PreMergeCommit),
        ] {
            let config_stage: ConfigStage = alias.parse().expect("config alias parses");
            assert_eq!(to_hook_stage(config_stage), Some(expected));

            // The runner's `Stage` accepts the same aliases via serde.
            let runner_stage: HookStage =
                serde_json::from_value(serde_json::Value::String(alias.to_string()))
                    .expect("runner alias parses");
            assert_eq!(runner_stage, expected);
        }
    }

    #[test]
    fn builtins_only_lowers_to_poly_entries() {
        let hooks = hooks_from(
            r#"
[hooks]
stages = ["pre-commit"]
[hooks.builtin]
polylint = true
polyfmt = true
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["polylint", "polyfmt"]);
        let HookCommand::Run(line) = &spec.hooks[0].command else {
            panic!("expected run command");
        };
        assert!(line.ends_with(" lint"), "unexpected line: {line}");
        assert!(line.contains("/opt/poly/bin/poly"));
        assert!(spec.hooks[0].pass_filenames);
    }

    #[test]
    fn commit_builtin_defaults_to_commit_msg_stage() {
        let hooks = hooks_from(
            r#"
[hooks.builtin]
commit = true
"#,
        );
        // Not present on pre-commit...
        let pre = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert!(pre.hooks.is_empty());
        // ...but present on commit-msg, with file passing for the message file.
        let msg = lower_stage(
            &hooks,
            &poly(),
            HookStage::CommitMsg,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(ids(&msg), vec!["poly-commit"]);
        assert!(msg.hooks[0].pass_filenames);
    }

    #[test]
    fn inline_jobs_carry_run_args_env_and_stage_fixed() {
        let hooks = hooks_from(
            r#"
[hooks]
env = { GLOBAL = "1" }
[hooks.pre-commit]
parallel = true
[[hooks.pre-commit.jobs]]
name = "fmt"
run = "cargo fmt"
args = ["--check"]
env = { LOCAL = "2" }
stage_fixed = true
files = "**/*.rs"
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["fmt"]);
        let hook = &spec.hooks[0];
        assert!(matches!(&hook.command, HookCommand::Run(line) if line == "cargo fmt"));
        assert_eq!(hook.args, vec!["--check".to_string()]);
        assert_eq!(hook.env.get("GLOBAL").map(String::as_str), Some("1"));
        assert_eq!(hook.env.get("LOCAL").map(String::as_str), Some("2"));
        assert!(hook.stage_fixed);
        assert!(hook.parallel);
        assert!(hook.files.is_some());
        // A job with a file filter does not force `always_run`.
        assert!(!hook.always_run);
    }

    #[test]
    fn script_job_maps_to_script_command() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit.scripts.lint]
script = "lint.sh"
runner = "bash"
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        let HookCommand::Script { path, runner } = &spec.hooks[0].command else {
            panic!("expected script command");
        };
        assert_eq!(path, "lint.sh");
        assert_eq!(runner.as_deref(), Some("bash"));
    }

    #[test]
    fn builtins_and_inline_jobs_are_priority_ordered() {
        let hooks = hooks_from(
            r#"
[hooks.builtin]
polylint = { stages = ["pre-commit"] }
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "early"
run = "echo early"
priority = -5
[[hooks.pre-commit.jobs]]
name = "late"
run = "echo late"
priority = 5
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        // `early` (-5) < polylint (0) < `late` (5).
        assert_eq!(ids(&spec), vec!["early", "polylint", "late"]);
    }

    #[test]
    fn always_jobs_are_appended_to_multiple_stages() {
        let hooks = hooks_from(
            r#"
[hooks.always]
[[hooks.always.jobs]]
name = "everywhere"
run = "echo hi"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "commit-only"
run = "echo commit"
"#,
        );
        let pre = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(ids(&pre), vec!["commit-only", "everywhere"]);
        let push = lower_stage(
            &hooks,
            &poly(),
            HookStage::PrePush,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(ids(&push), vec!["everywhere"]);
    }

    #[test]
    fn patterns_lower_to_file_and_exclude_globs() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "x"
files = ["src/**/*.rs"]
exclude = "src/generated/**"
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        let hook = &spec.hooks[0];
        assert!(hook.files.as_ref().unwrap().is_match(Path::new("src/a.rs")));
        assert!(
            hook.exclude
                .as_ref()
                .unwrap()
                .is_match(Path::new("src/generated/x.rs"))
        );
    }

    #[test]
    fn unconditional_skip_drops_a_job_and_only_false_too() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "skipped"
run = "x"
skip = true
[[hooks.pre-commit.jobs]]
name = "only-false"
run = "y"
only = false
[[hooks.pre-commit.jobs]]
name = "kept"
run = "z"
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["kept"]);
    }

    #[test]
    fn stage_skip_yields_empty_stage() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
skip = true
[[hooks.pre-commit.jobs]]
run = "x"
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert!(spec.hooks.is_empty());
    }

    #[test]
    fn template_tokens_are_substituted_and_disable_file_passing() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "prettier --write {staged_files}"
"#,
        );
        let files = vec![PathBuf::from("a.js"), PathBuf::from("b.js")];
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &files,
            &HookCacheMode::Safe,
        )
        .unwrap();
        let hook = &spec.hooks[0];
        let HookCommand::Run(line) = &hook.command else {
            panic!("expected run command");
        };
        assert!(line.contains("a.js") && line.contains("b.js"), "{line}");
        assert!(!hook.pass_filenames);
    }

    #[test]
    fn template_substitution_is_scoped_by_the_job_glob() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "cargo fmt {staged_files}"
files = "**/*.rs"
"#,
        );
        let files = vec![PathBuf::from("src/a.rs"), PathBuf::from("README.md")];
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &files,
            &HookCacheMode::Safe,
        )
        .unwrap();
        let HookCommand::Run(line) = &spec.hooks[0].command else {
            panic!("expected run command");
        };
        // Only the `.rs` file survives the job's `files` glob.
        assert!(line.contains("a.rs"), "{line}");
        assert!(
            !line.contains("README.md"),
            "non-matching file leaked: {line}"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn template_substitution_shell_quotes_paths_with_spaces() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "echo {staged_files}"
"#,
        );
        let files = vec![PathBuf::from("my file.js")];
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &files,
            &HookCacheMode::Safe,
        )
        .unwrap();
        let HookCommand::Run(line) = &spec.hooks[0].command else {
            panic!("expected run command");
        };
        // The path is single-quoted as one shell token, not split on the space.
        assert!(line.contains("'my file.js'"), "unquoted path: {line}");
    }

    #[test]
    fn stage_skip_suppresses_builtins_too() {
        let hooks = hooks_from(
            r#"
[hooks.builtin]
polylint = true
[hooks.pre-commit]
skip = true
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert!(
            spec.hooks.is_empty(),
            "stage skip must suppress builtins too"
        );
    }

    #[test]
    fn stage_precondition_before_after_carry_over() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
precondition = "command -v cargo"
before = "echo before"
after = ["echo a", "echo b"]
[[hooks.pre-commit.jobs]]
run = "x"
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(spec.precondition.as_deref(), Some("command -v cargo"));
        assert_eq!(spec.before, vec!["echo before"]);
        assert_eq!(spec.after, vec!["echo a", "echo b"]);
    }

    #[test]
    fn exclude_tags_drop_matching_jobs() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
exclude_tags = ["slow"]
[[hooks.pre-commit.jobs]]
name = "fast"
run = "x"
tags = ["quick"]
[[hooks.pre-commit.jobs]]
name = "slow-job"
run = "y"
tags = ["slow"]
"#,
        );
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["fast"]);
    }
}
