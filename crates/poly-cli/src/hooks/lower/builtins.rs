//! Lowering for the `file_safety` and `cargo` builtin groups.
//!
//! These two families are richer than the single-tool builtins (`lint` /
//! `fmt` / `commit`) handled inline in [`super`]:
//!
//! - `file_safety` lowers to one hidden `poly hooks check …` invocation whose
//!   flags select the enabled member checks (the runner appends the matched
//!   files); see [`crate::hooks::checks`].
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
use poly_hooks::model::{Hook, HookCache};
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
mod tests {
    use std::path::{Path, PathBuf};

    use anyhow::Result;
    use poly_config::{HookCacheMode, HooksConfig, PolyConfig, ToolsConfig};
    use poly_hooks::Stage as HookStage;
    use poly_hooks::model::{HookCache, HookCommand, StageSpec};

    use super::super::lower_stage_with_probe;
    use super::ToolProbe;

    /// A [`ToolProbe`] over a fixed allow-list, so Cargo-builtin gating is
    /// deterministic regardless of what the host has installed.
    struct StubProbe(&'static [&'static str]);

    impl ToolProbe for StubProbe {
        fn is_available(&self, tool: &str) -> bool {
            self.0.contains(&tool)
        }
        fn is_cargo_project(&self) -> bool {
            true
        }
    }

    /// Like [`StubProbe`] but reports the repo is *not* a Cargo project, to
    /// exercise the default-on cargo gate.
    struct NonCargoProbe(&'static [&'static str]);

    impl ToolProbe for NonCargoProbe {
        fn is_available(&self, tool: &str) -> bool {
            self.0.contains(&tool)
        }
        fn is_cargo_project(&self) -> bool {
            false
        }
    }

    /// `lower_stage` over a probe reporting no external tools, so the default-on
    /// `cargo` builtin group never intrudes on tests that don't exercise it.
    fn lower_stage(
        hooks: &HooksConfig,
        poly_bin: &Path,
        stage: HookStage,
        files: &[PathBuf],
        cache_mode: &HookCacheMode,
    ) -> Result<StageSpec> {
        lower_stage_with_probe(
            hooks,
            poly_bin,
            stage,
            files,
            cache_mode,
            &StubProbe(&[]),
            &ToolsConfig::default(),
        )
    }

    fn hooks_from(toml: &str) -> HooksConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        std::fs::write(&path, toml).unwrap();
        PolyConfig::load_file(&path).unwrap().hooks
    }

    fn config_from(toml: &str) -> PolyConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        std::fs::write(&path, toml).unwrap();
        PolyConfig::load_file(&path).unwrap()
    }

    fn poly() -> PathBuf {
        PathBuf::from("/opt/poly/bin/poly")
    }

    fn ids(spec: &StageSpec) -> Vec<String> {
        spec.hooks.iter().map(|hook| hook.id.clone()).collect()
    }

    fn run_line<'a>(spec: &'a StageSpec, id: &str) -> &'a str {
        let hook = spec
            .hooks
            .iter()
            .find(|hook| hook.id == id)
            .unwrap_or_else(|| panic!("hook `{id}` not lowered"));
        match &hook.command {
            HookCommand::Run(line) => line,
            HookCommand::Script { .. } => panic!("expected run command"),
        }
    }

    #[test]
    fn file_safety_bare_toggle_lowers_to_one_check_hook_with_every_flag() {
        let hooks = hooks_from("[hooks.builtin]\nfile_safety = true\n");
        let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
        assert_eq!(ids(&spec), vec!["file-safety"]);
        let line = run_line(&spec, "file-safety");
        assert!(line.contains(" hooks check "), "{line}");
        for flag in [
            "--merge-conflict",
            "--added-large-files",
            "--max-added-kb 500",
            "--private-key",
            "--case-conflict",
            "--executables-have-shebangs",
            "--shebang-scripts-are-executable",
        ] {
            assert!(line.contains(flag), "missing `{flag}` in: {line}");
        }
        let hook = &spec.hooks[0];
        assert!(hook.pass_filenames);
    }

    #[test]
    fn file_safety_exclude_lowers_to_the_hook_exclude_glob() {
        let hooks = hooks_from(
            r#"
[hooks.builtin.file_safety]
exclude = "crates/poly-cli/src/hooks/checks.rs"
"#,
        );
        let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
        let hook = spec
            .hooks
            .iter()
            .find(|hook| hook.id == "file-safety")
            .expect("file-safety lowered");
        let exclude = hook.exclude.as_ref().expect("exclude glob present");
        assert!(exclude.is_match(Path::new("crates/poly-cli/src/hooks/checks.rs")));
        assert!(!exclude.is_match(Path::new("crates/poly-cli/src/hooks/lower.rs")));
    }

    #[test]
    fn file_safety_table_omits_disabled_check_flags_and_honours_max_kb() {
        let hooks = hooks_from(
            r#"
[hooks.builtin.file_safety]
private_key = false
case_conflict = false
max_added_file_kb = 2048
"#,
        );
        let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
        let line = run_line(&spec, "file-safety");
        assert!(line.contains("--merge-conflict"), "{line}");
        assert!(line.contains("--max-added-kb 2048"), "{line}");
        assert!(!line.contains("--private-key"), "{line}");
        assert!(!line.contains("--case-conflict"), "{line}");
    }

    #[test]
    fn file_safety_with_every_check_off_lowers_to_nothing() {
        let hooks = hooks_from(
            r#"
[hooks.builtin.file_safety]
merge_conflict = false
added_large_files = false
private_key = false
case_conflict = false
executables_have_shebangs = false
shebang_scripts_are_executable = false
"#,
        );
        let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn file_safety_disabled_lowers_to_nothing() {
        let hooks = hooks_from("[hooks.builtin]\nfile_safety = false\n");
        let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
        assert!(spec.hooks.is_empty());
    }

    #[test]
    fn cargo_defaults_on_when_a_hooks_section_is_present() {
        let hooks = hooks_from("[hooks]\nstages = [\"pre-commit\"]\n");
        assert!(hooks.present);
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert_eq!(
            ids(&spec),
            vec!["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]
        );
    }

    #[test]
    fn cargo_does_not_default_on_outside_a_cargo_project() {
        let hooks = hooks_from("[hooks]\nstages = [\"pre-commit\"]\n");
        let probe = NonCargoProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn cargo_default_on_is_suppressed_by_explicit_false() {
        let hooks = hooks_from("[hooks.builtin]\ncargo = false\n");
        let probe = StubProbe(&["cargo-clippy"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn cargo_does_not_default_on_without_a_hooks_section() {
        let hooks = hooks_from("");
        assert!(!hooks.present);
        let probe = StubProbe(&["cargo-clippy"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert!(spec.hooks.is_empty());
    }

    #[test]
    fn cargo_lowers_only_tools_present_on_path() {
        let hooks = hooks_from("[hooks.builtin]\ncargo = true\n");
        let probe = StubProbe(&["cargo-clippy", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["cargo-clippy", "cargo-deny"]);

        let clippy = &spec.hooks[0];
        assert_eq!(run_line(&spec, "cargo-clippy"), clippy_command());
        assert!(clippy.always_run);
        assert!(!clippy.pass_filenames);
        assert!(clippy.compiler);
        assert!(!spec.hooks[1].compiler);
        assert!(clippy.workspace);
        assert!(!clippy.skip_in_lint);
        assert!(
            matches!(clippy.cache, HookCache::DeclaredInputs(_)),
            "cargo group is result-cached by default"
        );
    }

    #[test]
    fn cargo_cache_false_disables_the_result_cache() {
        let hooks = hooks_from("[hooks.builtin.cargo]\ncache = false\n");
        let probe = StubProbe(&["cargo-clippy"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert!(matches!(spec.hooks[0].cache, HookCache::Disabled));
    }

    #[test]
    fn cargo_lint_false_sets_skip_in_lint() {
        let off = lower_stage_with_probe(
            &hooks_from("[hooks.builtin.cargo]\nlint = false\n"),
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &StubProbe(&["cargo-clippy"]),
            &ToolsConfig::default(),
        )
        .unwrap();
        assert_eq!(ids(&off), vec!["cargo-clippy"], "still lowered as a git hook");
        assert!(off.hooks[0].skip_in_lint, "lint = false sets skip_in_lint");
    }

    #[test]
    fn cargo_cache_off_mode_disables_the_result_cache() {
        let hooks = hooks_from("[hooks.builtin]\ncargo = true\n");
        let probe = StubProbe(&["cargo-clippy"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Off,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert!(matches!(spec.hooks[0].cache, HookCache::Disabled));
    }

    fn clippy_command() -> &'static str {
        "cargo clippy --workspace --all-targets -- -D warnings"
    }

    #[test]
    fn cargo_with_no_tools_present_lowers_to_nothing() {
        let hooks = hooks_from("[hooks.builtin]\ncargo = true\n");
        let probe = StubProbe(&[]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn cargo_per_tool_toggle_drops_the_disabled_tool_even_when_present() {
        let hooks = hooks_from("[hooks.builtin.cargo]\nmachete = false\n");
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["cargo-clippy", "cargo-sort", "cargo-deny"]);
    }

    #[test]
    fn cargo_defaults_on_alongside_an_explicit_builtin() {
        let hooks = hooks_from("[hooks.builtin]\nlint = true\n");
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        let got = ids(&spec);
        assert!(got.contains(&"lint".to_string()), "{got:?}");
        for tool in ["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"] {
            assert!(got.contains(&tool.to_string()), "missing {tool}: {got:?}");
        }
        assert_eq!(got.len(), 5, "{got:?}");
    }

    #[test]
    fn cargo_respects_a_non_default_stage() {
        let hooks = hooks_from("[hooks.builtin.cargo]\nstages = [\"pre-push\"]\n");
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let pre = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert!(pre.hooks.is_empty());
        let push = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PrePush,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        assert_eq!(
            ids(&push),
            vec!["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]
        );
    }

    #[test]
    fn catalog_tool_on_a_stage_lowers_to_a_per_file_hook_when_present() {
        let config = config_from(
            r#"
[tools.shfmt]
enabled = true
stages = ["pre-commit"]
"#,
        );
        let probe = StubProbe(&["shfmt"]);
        let spec = lower_stage_with_probe(
            &config.hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &config.tools,
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["shfmt"]);
        let hook = &spec.hooks[0];
        assert!(hook.pass_filenames, "catalog hooks run per-file");
        let line = run_line(&spec, "shfmt");
        assert!(
            line.starts_with(super::shell_quote("shfmt").as_str()),
            "runs the tool binary: {line}"
        );
        assert!(!line.contains("$PATH"), "placeholder dropped: {line}");
    }

    #[test]
    fn catalog_tool_is_skipped_when_its_binary_is_absent() {
        let config = config_from(
            r#"
[tools.shfmt]
enabled = true
stages = ["pre-commit"]
"#,
        );
        let probe = StubProbe(&[]);
        let spec = lower_stage_with_probe(
            &config.hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &config.tools,
        )
        .unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn catalog_tool_does_not_lower_on_an_unbound_stage() {
        let config = config_from(
            r#"
[tools.shfmt]
enabled = true
stages = ["pre-push"]
"#,
        );
        let probe = StubProbe(&["shfmt"]);
        let spec = lower_stage_with_probe(
            &config.hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &config.tools,
        )
        .unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn catalog_tool_with_empty_stages_is_inert() {
        let config = config_from("[tools.shfmt]\nenabled = true\n");
        let probe = StubProbe(&["shfmt"]);
        let spec = lower_stage_with_probe(
            &config.hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &config.tools,
        )
        .unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn catalog_tool_env_and_root_are_forwarded_to_hook() {
        let config = config_from(
            r#"
[tools.shfmt]
enabled = true
stages = ["pre-commit"]
root = "packages/shell"

[tools.shfmt.env]
GOPATH = "/home/user/go"
"#,
        );
        let probe = StubProbe(&["shfmt"]);
        let spec = lower_stage_with_probe(
            &config.hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &config.tools,
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["shfmt"]);
        let hook = &spec.hooks[0];
        assert_eq!(
            hook.env.get("GOPATH").map(String::as_str),
            Some("/home/user/go"),
            "env var forwarded to hook"
        );
        assert_eq!(
            hook.cwd.as_deref(),
            Some(std::path::Path::new("packages/shell")),
            "root forwarded to hook.cwd"
        );
    }

    #[test]
    fn cargo_clippy_args_override_appears_in_lowered_hook_command() {
        let hooks = hooks_from(
            r#"
[hooks.builtin.cargo]
clippy_args = ["--workspace", "--exclude=crawlberg-php", "--all-features"]
"#,
        );
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        let line = run_line(&spec, "cargo-clippy");
        assert!(
            line.contains("--exclude=crawlberg-php"),
            "configured flag present: {line}"
        );
        assert!(line.contains("--all-features"), "configured flag present: {line}");
        assert!(line.contains("-D warnings"), "strict warnings always present: {line}");
        assert!(
            !line.contains("--all-targets"),
            "default flag replaced by override: {line}"
        );
    }

    #[test]
    fn cargo_clippy_default_command_is_unchanged_without_override() {
        let hooks = hooks_from("[hooks.builtin]\ncargo = true\n");
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
            &ToolsConfig::default(),
        )
        .unwrap();
        let line = run_line(&spec, "cargo-clippy");
        assert!(line.contains("--workspace"), "default workspace flag: {line}");
        assert!(line.contains("--all-targets"), "default all-targets flag: {line}");
        assert!(line.contains("-D warnings"), "strict warnings always present: {line}");
    }
}
