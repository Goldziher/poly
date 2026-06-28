//! Lowering for the `file_safety` and `cargo` builtin groups.
//!
//! These two families are richer than the single-tool builtins (`polylint` /
//! `polyfmt` / `commit`) handled inline in [`super`]:
//!
//! - `file_safety` lowers to one hidden `poly hooks check …` invocation whose
//!   flags select the enabled member checks (the runner appends the matched
//!   files); see [`crate::hooks::checks`].
//! - `cargo` lowers to whole-workspace `cargo clippy` / `sort` / `machete` /
//!   `deny` hooks, each capability-probed against `PATH` so an absent tool is
//!   skipped (with a `tracing::info!` notice) rather than failing the run.

use anyhow::Result;
use poly_config::{CargoHooks, FileSafetyHooks, HooksConfig, Stage as ConfigStage};
use poly_hooks::model::{Hook, HookCache};
use tracing::info;

use super::builtin_runs_on;

/// Capability probe: whether an external tool is resolvable on `PATH`.
///
/// Abstracted so the Cargo-builtin gating can be exercised deterministically in
/// tests without depending on what the host has installed.
pub(super) trait ToolProbe {
    /// Whether `tool` (e.g. `"cargo-clippy"`) is available on this host.
    fn is_available(&self, tool: &str) -> bool;
}

/// The production probe: resolves a tool against `PATH` (and Windows `PATHEXT`).
pub(super) struct PathProbe;

impl ToolProbe for PathProbe {
    fn is_available(&self, tool: &str) -> bool {
        which::which(tool).is_ok()
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
    if !safety.enabled
        || !builtin_runs_on(
            &safety.stages,
            &hooks.stages,
            ConfigStage::PreCommit,
            config_stage,
        )?
    {
        return Ok(());
    }
    let Some(flags) = file_safety_flags(safety) else {
        // The group is enabled but every member check is off — nothing to run.
        return Ok(());
    };
    let mut hook = Hook::run("file-safety", format!("{poly} hooks check {flags}"));
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
    command: &'static str,
    compiler: bool,
}

/// The four Cargo builtins, paired with the group's per-tool enable toggles.
fn cargo_tools(cargo: &CargoHooks) -> [CargoTool; 4] {
    [
        CargoTool {
            enabled: cargo.clippy,
            id: "cargo-clippy",
            probe: "cargo-clippy",
            command: "cargo clippy --workspace --all-targets -- -D warnings",
            compiler: true,
        },
        CargoTool {
            enabled: cargo.sort,
            id: "cargo-sort",
            probe: "cargo-sort",
            command: "cargo sort --workspace --check",
            compiler: false,
        },
        CargoTool {
            enabled: cargo.machete,
            id: "cargo-machete",
            probe: "cargo-machete",
            command: "cargo machete",
            compiler: false,
        },
        CargoTool {
            enabled: cargo.deny,
            id: "cargo-deny",
            probe: "cargo-deny",
            command: "cargo deny check",
            compiler: false,
        },
    ]
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
    probe: &dyn ToolProbe,
    out: &mut Vec<Hook>,
) -> Result<()> {
    let cargo = &hooks.builtin.cargo;
    if !cargo.enabled
        || !builtin_runs_on(
            &cargo.stages,
            &hooks.stages,
            ConfigStage::PreCommit,
            config_stage,
        )?
    {
        return Ok(());
    }
    for tool in cargo_tools(cargo) {
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
        // Whole-workspace commands run once at the repo root regardless of which
        // files changed; they take no per-file arguments.
        hook.pass_filenames = false;
        hook.always_run = true;
        hook.compiler = tool.compiler;
        hook.cache = HookCache::Disabled;
        out.push(hook);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use poly_config::{HookCacheMode, HooksConfig, PolyConfig};
    use poly_hooks::Stage as HookStage;
    use poly_hooks::model::{HookCommand, StageSpec};

    use super::super::{lower_stage, lower_stage_with_probe};
    use super::ToolProbe;

    /// A [`ToolProbe`] over a fixed allow-list, so Cargo-builtin gating is
    /// deterministic regardless of what the host has installed.
    struct StubProbe(&'static [&'static str]);

    impl ToolProbe for StubProbe {
        fn is_available(&self, tool: &str) -> bool {
            self.0.contains(&tool)
        }
    }

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
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
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
        // The matched files are appended by the runner, so it passes filenames.
        let hook = &spec.hooks[0];
        assert!(hook.pass_filenames);
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
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
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
        let spec = lower_stage(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
        )
        .unwrap();
        assert!(spec.hooks.is_empty(), "{:?}", ids(&spec));
    }

    #[test]
    fn file_safety_disabled_lowers_to_nothing() {
        let hooks = hooks_from("[hooks.builtin]\nfile_safety = false\n");
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
    fn cargo_lowers_only_tools_present_on_path() {
        let hooks = hooks_from("[hooks.builtin]\ncargo = true\n");
        // Only clippy and deny are "installed".
        let probe = StubProbe(&["cargo-clippy", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["cargo-clippy", "cargo-deny"]);

        let clippy = &spec.hooks[0];
        assert_eq!(run_line(&spec, "cargo-clippy"), clippy_command());
        // Whole-workspace hooks run project-wide and pass no filenames.
        assert!(clippy.always_run);
        assert!(!clippy.pass_filenames);
        // clippy is sccache-eligible; the non-compiling tools are not.
        assert!(clippy.compiler);
        assert!(!spec.hooks[1].compiler);
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
        )
        .unwrap();
        assert_eq!(ids(&spec), vec!["cargo-clippy", "cargo-sort", "cargo-deny"]);
    }

    #[test]
    fn cargo_disabled_lowers_to_nothing_regardless_of_path() {
        let hooks = hooks_from("[hooks.builtin]\npolylint = true\n");
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        let spec = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
        )
        .unwrap();
        // Only the explicitly-enabled polylint builtin is present.
        assert_eq!(ids(&spec), vec!["polylint"]);
    }

    #[test]
    fn cargo_respects_a_non_default_stage() {
        let hooks = hooks_from("[hooks.builtin.cargo]\nstages = [\"pre-push\"]\n");
        let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
        // Not on pre-commit...
        let pre = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PreCommit,
            &[],
            &HookCacheMode::Safe,
            &probe,
        )
        .unwrap();
        assert!(pre.hooks.is_empty());
        // ...but present on pre-push.
        let push = lower_stage_with_probe(
            &hooks,
            &poly(),
            HookStage::PrePush,
            &[],
            &HookCacheMode::Safe,
            &probe,
        )
        .unwrap();
        assert_eq!(
            ids(&push),
            vec!["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]
        );
    }
}
