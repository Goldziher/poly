//! Lowering for the `file_safety` and `cargo` builtin groups.
//!
//! These two families are richer than the single-tool builtins (`lint` /
//! `fmt` / `commit`) handled inline in [`super`]:
//!
//! - `file_safety` lowers to one hidden `poly hooks check …` invocation whose
//!   flags select the enabled member checks (the runner appends the matched
//!   files); the check flags themselves are lowered by the CLI's
//!   `poly hooks check` module.
//! - `cargo` lowers to whole-workspace `cargo clippy` / `sort` / `machete` /
//!   `deny` hooks, each capability-probed against `PATH` so an absent tool is
//!   skipped (with a `tracing::info!` notice) rather than failing the run.

use std::path::Path;

use anyhow::{Context as _, Result};
use poly_catalog::{Catalog, Command as CatalogCommand, PATH_PLACEHOLDER};
use poly_config::{
    CargoHooks, FileSafetyHooks, HookCacheMode, HooksConfig, Stage as ConfigStage, ToolConfig, ToolsConfig,
};
use poly_hooks::filter::FilePattern;
use poly_hooks::model::{Hook, HookCache, HookCommand, StageSpec};
use tracing::info;

use super::{builtin_runs_on, shell_quote};

/// Input globs the `cargo` group is result-cached on: any change to Rust
/// sources, a manifest, the lockfile, the `cargo deny` policy, or the toolchain
/// pin re-runs the whole group. Conservative on purpose — it never yields a
/// false hit, at the cost of occasionally re-running when an unrelated one of
/// these changes.
const CARGO_CACHE_INPUTS: &[&str] = &[
    "**/*.rs",
    "**/Cargo.toml",
    "Cargo.lock",
    "deny.toml",
    "rust-toolchain.toml",
    "rust-toolchain",
];

/// Resolve the result-cache policy for the whole `cargo` group: declared-inputs
/// caching by default, disabled when the group opts out (`cargo = { cache =
/// false }`) or the global `[cache.results] hooks` mode is `off`.
fn cargo_cache(cargo: &CargoHooks, cache_mode: &HookCacheMode) -> Result<HookCache> {
    if !cargo.cache || matches!(cache_mode, HookCacheMode::Off) {
        return Ok(HookCache::Disabled);
    }
    let pattern = FilePattern::glob(CARGO_CACHE_INPUTS.iter().map(|glob| (*glob).to_string()).collect())
        .context("building the cargo builtin cache-input globs")?;
    Ok(HookCache::DeclaredInputs(pattern))
}

/// Capability probe: whether an external tool is resolvable on `PATH`.
///
/// Abstracted so the Cargo-builtin gating can be exercised deterministically in
/// tests without depending on what the host has installed.
pub(super) trait ToolProbe {
    /// Whether `tool` (e.g. `"cargo-clippy"`) is available on this host.
    fn is_available(&self, tool: &str) -> bool;

    /// Whether the repository is a Cargo project (a `Cargo.toml` at its root).
    ///
    /// Gates the *default-on* `cargo` builtin group so it never tries to run
    /// `cargo clippy` in a non-Rust repo. An explicit `cargo = true` bypasses
    /// this — that is the user's deliberate choice.
    fn is_cargo_project(&self) -> bool;
}

/// The production probe: resolves a tool against `PATH` (and Windows `PATHEXT`)
/// and detects a Cargo project relative to the repository root.
pub(super) struct PathProbe<'a> {
    /// Repository root, used to look for a `Cargo.toml`.
    pub root: &'a Path,
}

impl ToolProbe for PathProbe<'_> {
    fn is_available(&self, tool: &str) -> bool {
        which::which(tool).is_ok()
    }

    fn is_cargo_project(&self) -> bool {
        self.root.join("Cargo.toml").is_file()
    }
}

/// Append the `file_safety` builtin as a single hidden `poly hooks check …`
/// invocation carrying a flag per enabled member check.
///
/// `poly` is the shell-quoted path to the running `poly` binary. The hook
/// passes filenames (the runner appends the matched files) and is never
/// result-cached: the executable-bit and case-conflict checks depend on state
/// outside the content digest, and the checks are cheap regardless.
pub(super) fn append_file_safety(
    hooks: &HooksConfig,
    poly: &str,
    config_stage: ConfigStage,
    out: &mut Vec<Hook>,
) -> Result<()> {
    let safety = &hooks.builtin.file_safety;
    if !safety.enabled || !builtin_runs_on(&safety.stages, &hooks.stages, ConfigStage::PreCommit, config_stage)? {
        return Ok(());
    }
    let Some(flags) = file_safety_flags(safety) else {
        return Ok(());
    };
    let mut hook = Hook::run("file-safety", format!("{poly} hooks check {flags}"));
    let (files, exclude) = super::builtin_globs(safety.files.as_ref(), safety.exclude.as_ref())?;
    hook.files = files;
    hook.exclude = exclude;
    hook.cache = HookCache::Disabled;
    out.push(hook);
    Ok(())
}

/// Build the `poly hooks check` flag string for the enabled member checks, or
/// `None` when no check is enabled.
fn file_safety_flags(safety: &FileSafetyHooks) -> Option<String> {
    let mut flags: Vec<String> = Vec::new();
    if safety.merge_conflict {
        flags.push("--merge-conflict".to_string());
    }
    if safety.added_large_files {
        flags.push("--added-large-files".to_string());
        flags.push(format!("--max-added-kb {}", safety.max_added_file_kb));
    }
    if safety.private_key {
        flags.push("--private-key".to_string());
    }
    if safety.case_conflict {
        flags.push("--case-conflict".to_string());
    }
    if safety.executables_have_shebangs {
        flags.push("--executables-have-shebangs".to_string());
    }
    if safety.shebang_scripts_are_executable {
        flags.push("--shebang-scripts-are-executable".to_string());
    }
    (!flags.is_empty()).then(|| flags.join(" "))
}

/// One whole-workspace Cargo tool: its hook id, the `PATH` binary that gates it,
/// the command line, and whether it benefits from sccache compiler-wrapping.
struct CargoTool {
    enabled: bool,
    id: &'static str,
    probe: &'static str,
    command: String,
    compiler: bool,
}

/// Build the `cargo clippy` command line from the resolved [`CargoHooks`].
///
/// When `clippy_args` is `Some`, the provided list **replaces** the default
/// `--workspace --all-targets` flags; `-- -D warnings` is always appended.
fn clippy_command(cargo: &CargoHooks) -> String {
    match &cargo.clippy_args {
        Some(args) => format!("cargo clippy {} -- -D warnings", args.join(" ")),
        None => "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
    }
}

/// The four Cargo builtins, paired with the group's per-tool enable toggles.
fn cargo_tools(cargo: &CargoHooks) -> [CargoTool; 4] {
    [
        CargoTool {
            enabled: cargo.clippy,
            id: "cargo-clippy",
            probe: "cargo-clippy",
            command: clippy_command(cargo),
            compiler: true,
        },
        CargoTool {
            enabled: cargo.sort,
            id: "cargo-sort",
            probe: "cargo-sort",
            command: "cargo sort --workspace --check".to_string(),
            compiler: false,
        },
        CargoTool {
            enabled: cargo.machete,
            id: "cargo-machete",
            probe: "cargo-machete",
            command: "cargo-machete".to_string(),
            compiler: false,
        },
        CargoTool {
            enabled: cargo.deny,
            id: "cargo-deny",
            probe: "cargo-deny",
            command: "cargo deny check".to_string(),
            compiler: false,
        },
    ]
}

/// Resolve the effective `cargo` builtin group, or `None` when it is inactive.
///
/// Precedence: an explicit `[hooks.builtin] cargo` value wins (`cargo = false`
/// disables, `cargo = true` / a table enables). When the key is absent, the
/// group runs by default **iff** a `[hooks]` section was configured — so a repo
/// that has adopted poly hooks gets clippy/sort/machete/deny (each still
/// capability-probed), while a repo with no `[hooks]` section never does.
fn resolve_cargo_group(hooks: &HooksConfig, cargo_project: bool) -> Option<CargoHooks> {
    match &hooks.builtin.cargo {
        Some(cargo) if cargo.enabled => Some(cargo.clone()),
        Some(_) => None,
        None if hooks.present && cargo_project => Some(CargoHooks {
            enabled: true,
            ..CargoHooks::default()
        }),
        None => None,
    }
}

/// Append the enabled, present `cargo` builtins as whole-workspace hooks.
///
/// Each tool is capability-probed: an absent tool is skipped with a
/// `tracing::info!` notice rather than failing the run. The hooks run
/// project-wide (`always_run`, no `pass_filenames`) and are not result-cached,
/// since a whole-workspace tool depends on far more than the matched file set.
pub(super) fn append_cargo(
    hooks: &HooksConfig,
    config_stage: ConfigStage,
    cache_mode: &HookCacheMode,
    probe: &dyn ToolProbe,
    out: &mut Vec<Hook>,
) -> Result<()> {
    let Some(cargo) = resolve_cargo_group(hooks, probe.is_cargo_project()) else {
        return Ok(());
    };
    if !builtin_runs_on(&cargo.stages, &hooks.stages, ConfigStage::PreCommit, config_stage)? {
        return Ok(());
    }
    let cache = cargo_cache(&cargo, cache_mode)?;
    for tool in cargo_tools(&cargo) {
        if !tool.enabled {
            continue;
        }
        if !probe.is_available(tool.probe) {
            info!(
                tool = tool.id,
                probe = tool.probe,
                "cargo builtin skipped: tool not found on PATH"
            );
            continue;
        }
        let mut hook = Hook::run(tool.id, tool.command);
        hook.pass_filenames = false;
        hook.always_run = true;
        hook.compiler = tool.compiler;
        hook.workspace = true;
        hook.skip_in_lint = !cargo.lint;
        hook.cache = cache.clone();
        out.push(hook);
    }
    Ok(())
}

/// Rewrite the retained whole-project cargo hooks in `spec` to their autofix
/// command lines, for `poly lint --fix` / `poly fmt --fix`.
///
/// This is applied **only** on the fix paths, after
/// [`super::super::workspace_lint`] has reduced the lowered stage to its
/// workspace hooks. The git-hook / commit-gate lowering never calls it, so a
/// commit stays check-only — the fix flag can never leak into a hook run.
pub(crate) fn apply_cargo_fix_mode(spec: &mut StageSpec) {
    for hook in &mut spec.hooks {
        if let HookCommand::Run(line) = &hook.command
            && let Some(fixed) = cargo_fix_command(&hook.id, line)
        {
            hook.command = HookCommand::Run(fixed);
        }
    }
}

/// Map a cargo builtin's check command to its autofix form, or `None` when the
/// tool has no autofix (`cargo-deny`) or the id is unknown.
///
/// The transforms mirror the check commands built in [`cargo_tools`] — keep the
/// two in sync:
/// - `cargo-sort`: drop the trailing `--check` (sorts in place).
/// - `cargo-machete`: append `--fix` (prunes unused deps).
/// - `cargo-clippy`: insert `--fix --allow-dirty --allow-staged` (the worktree
///   is dirty by construction on a `--fix` run); the always-appended
///   `-- -D warnings` and any `clippy_args` override are preserved.
fn cargo_fix_command(id: &str, check: &str) -> Option<String> {
    match id {
        "cargo-sort" => check.strip_suffix(" --check").map(str::to_owned),
        "cargo-machete" => Some(format!("{check} --fix")),
        "cargo-clippy" => check
            .strip_prefix("cargo clippy ")
            .map(|rest| format!("cargo clippy --fix --allow-dirty --allow-staged {rest}")),
        _ => None,
    }
}

/// Append a per-file hook for every enabled `[tools.<name>]` (ADR 0013) bound to
/// `config_stage`.
///
/// A catalog tool is **off by default** and bound to a stage only by an explicit
/// `stages = [...]` entry (an empty `stages` means "not a hook" — it is unbound),
/// so this never intrudes on a repo that has not opted a tool in. Each tool is
/// capability-probed against `PATH`: an absent binary is skipped with a
/// `tracing::info!` notice rather than failing the run, mirroring [`append_cargo`].
///
/// Dispatch is **per-file** (the mdsf-native model): the hook passes filenames,
/// and the catalog `$PATH` placeholder — the slot mdsf substitutes the file path
/// into — is dropped from the argv so the matched files the runner appends take
/// its place. There is deliberately no project-wide mode.
pub(super) fn append_catalog_tools(
    tools: &ToolsConfig,
    config_stage: ConfigStage,
    probe: &dyn ToolProbe,
    out: &mut Vec<Hook>,
) -> Result<()> {
    if tools.is_empty() {
        return Ok(());
    }
    let catalog = Catalog::get();
    for (name, tool_config) in tools.iter() {
        if !tool_config.enabled || !tool_config.stages.contains(&config_stage) {
            continue;
        }
        let Some(tool) = catalog.tool(name) else {
            continue;
        };
        if !probe.is_available(&tool.binary) {
            info!(
                tool = name.as_str(),
                binary = tool.binary.as_str(),
                "catalog tool skipped: binary not found on PATH"
            );
            continue;
        }
        let Some(command) = resolve_catalog_command(tool, tool_config) else {
            continue;
        };
        let arguments = tool_config.args.clone().unwrap_or_else(|| command.arguments.clone());
        let line = catalog_command_line(&tool.binary, &arguments);

        let mut hook = Hook::run(name, line);
        let (files, exclude) = super::builtin_globs(tool_config.files.as_ref(), tool_config.exclude.as_ref())?;
        hook.files = files;
        hook.exclude = exclude;
        hook.cache = HookCache::Disabled;
        hook.env.clone_from(&tool_config.env);
        hook.cwd = tool_config.root.as_ref().map(std::path::PathBuf::from);
        out.push(hook);
    }
    Ok(())
}

/// Resolve which catalog [`CatalogCommand`] an enabled tool runs: an explicit
/// `command = "..."` selects by name; otherwise prefer the tool's format command,
/// then its lint command. `None` when the tool exposes neither.
fn resolve_catalog_command<'a>(tool: &'a poly_catalog::Tool, tool_config: &ToolConfig) -> Option<&'a CatalogCommand> {
    match tool_config.command.as_deref() {
        Some(name) => tool.command(name),
        None => tool
            .format_command()
            .map(|(_, command)| command)
            .or_else(|| tool.lint_command().map(|(_, command)| command)),
    }
}

/// Build the shell command line for a per-file catalog hook: the binary followed
/// by its argv with the [`PATH_PLACEHOLDER`] dropped (the runner appends the
/// matched files in its place), each token shell-quoted.
fn catalog_command_line(binary: &str, arguments: &[String]) -> String {
    std::iter::once(binary)
        .map(String::from)
        .chain(
            arguments
                .iter()
                .filter(|argument| *argument != PATH_PLACEHOLDER)
                .cloned(),
        )
        .map(|token| shell_quote(&token))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
