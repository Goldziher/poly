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
    fn guard_passes(&self, _command: &str) -> bool {
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
    fn guard_passes(&self, _command: &str) -> bool {
        true
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

/// Lower the full cargo group (all tools present) as a `pre-commit` stage.
fn lower_all_cargo_tools(config_toml: &str) -> StageSpec {
    let hooks = hooks_from(config_toml);
    let probe = StubProbe(&["cargo-clippy", "cargo-sort", "cargo-machete", "cargo-deny"]);
    lower_stage_with_probe(
        &hooks,
        &poly(),
        HookStage::PreCommit,
        &[],
        &HookCacheMode::Safe,
        &probe,
        &ToolsConfig::default(),
    )
    .unwrap()
}

#[test]
fn cargo_fix_mode_rewrites_sort_machete_and_clippy_and_leaves_deny() {
    let mut spec = lower_all_cargo_tools("[hooks]\nstages = [\"pre-commit\"]\n");
    super::apply_cargo_fix_mode(&mut spec);

    assert_eq!(
        run_line(&spec, "cargo-sort"),
        "cargo sort --workspace",
        "sort must drop --check to sort in place"
    );
    assert_eq!(
        run_line(&spec, "cargo-machete"),
        "cargo-machete --fix",
        "machete must gain --fix"
    );
    let clippy = run_line(&spec, "cargo-clippy");
    assert_eq!(
        clippy, "cargo clippy --fix --allow-dirty --allow-staged --workspace --all-targets -- -D warnings",
        "clippy must run --fix against the dirty worktree, preserving -D warnings"
    );
    assert_eq!(
        run_line(&spec, "cargo-deny"),
        "cargo deny check",
        "deny has no autofix and must stay check-only"
    );
}

#[test]
fn cargo_fix_mode_preserves_a_clippy_args_override() {
    let mut spec = lower_all_cargo_tools("[hooks.builtin.cargo]\nclippy_args = [\"--all-features\"]\n");
    super::apply_cargo_fix_mode(&mut spec);
    assert_eq!(
        run_line(&spec, "cargo-clippy"),
        "cargo clippy --fix --allow-dirty --allow-staged --all-features -- -D warnings",
        "the fix flags precede the user's clippy_args override"
    );
}

#[test]
fn cargo_check_commands_are_untouched_without_fix_mode() {
    // The git-hook / commit-gate path never calls apply_cargo_fix_mode, so the
    // lowered commands stay check-only.
    let spec = lower_all_cargo_tools("[hooks]\nstages = [\"pre-commit\"]\n");
    assert_eq!(run_line(&spec, "cargo-sort"), "cargo sort --workspace --check");
    assert_eq!(run_line(&spec, "cargo-machete"), "cargo-machete");
    assert_eq!(
        run_line(&spec, "cargo-clippy"),
        "cargo clippy --workspace --all-targets -- -D warnings"
    );
    assert_eq!(run_line(&spec, "cargo-deny"), "cargo deny check");
}
