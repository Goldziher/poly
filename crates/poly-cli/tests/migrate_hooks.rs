//! `poly migrate` over lefthook and pre-commit configs: the armis-shaped fixture
//! (extends, nested groups, glob, stage_fixed, `{staged_files}`) plus focused
//! throwaway repos for each mapping rule.

use std::fs;
use std::path::{Path, PathBuf};

use poly_cli::MigrateArgs;
use poly_cli::migrate::deletion::Action;
use poly_cli::migrate::importers::{Absorb, ImportResult};
use poly_cli::migrate::{MigrationPlan, build_plan, run_migrate};
use poly_config::{HooksConfig, Job, Stage, ToolsConfig};
use tempfile::tempdir;

fn armis() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrate_hooks/armis")
}

fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    for (name, content) in files {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    dir
}

fn plan(dir: &Path) -> MigrationPlan {
    build_plan(dir).unwrap()
}

fn result<'a>(plan: &'a MigrationPlan, tool: &str) -> &'a ImportResult {
    plan.results
        .iter()
        .find(|r| r.tool == tool)
        .unwrap_or_else(|| panic!("no `{tool}` result"))
}

fn unrepresented(plan: &MigrationPlan, tool: &str) -> Vec<String> {
    match &result(plan, tool).absorb {
        Absorb::Partial(keys) => keys.clone(),
        other => panic!("{tool} should be partial, got {other:?}"),
    }
}

fn hooks_of(plan: &MigrationPlan) -> HooksConfig {
    let table: toml::Table = toml::from_str(&plan.doc.to_string()).unwrap();
    if let Some(tools) = table.get("tools") {
        let tools: ToolsConfig = tools.clone().try_into().unwrap();
        tools.validate().unwrap();
    }
    let hooks = table
        .get("hooks")
        .cloned()
        .unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
    let config: HooksConfig = hooks.try_into().unwrap();
    config.validate().unwrap();
    config
}

fn pre_commit_job<'a>(config: &'a HooksConfig, name: &str) -> &'a Job {
    let stage = &config.stage_configs[&Stage::PreCommit];
    stage
        .commands
        .get(name)
        .or_else(|| stage.scripts.get(name))
        .unwrap_or_else(|| panic!("no pre-commit job `{name}`"))
}

fn globs(patterns: Option<&poly_config::Patterns>) -> Vec<String> {
    patterns.map(|p| p.as_slice().to_vec()).unwrap_or_default()
}

fn migrate_args(dir: &Path, write: bool, recurse: bool) -> MigrateArgs {
    MigrateArgs {
        path: Some(dir.to_path_buf()),
        write,
        report: !write,
        recurse,
        verify: false,
        allow_dirty: true,
        strip_superseded: false,
    }
}

#[test]
fn armis_fixture_renders_the_expected_hooks() {
    let plan = plan(&armis());
    let rendered: String = plan
        .doc
        .to_string()
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("armis_hooks_poly_toml", rendered);
    hooks_of(&plan);
}

#[test]
fn armis_extends_files_are_all_read() {
    let plan = plan(&armis());
    let sources = &result(&plan, "lefthook").sources;
    assert_eq!(sources.len(), 11, "lefthook.yml plus its ten extends: {sources:?}");
}

#[test]
fn armis_nested_groups_become_ordered_priority_bands() {
    let config = hooks_of(&plan(&armis()));
    let stage = &config.stage_configs[&Stage::PreCommit];
    assert!(stage.parallel, "parallel chains and the check group share bands");
    let priority = |name: &str| pre_commit_job(&config, name).priority;
    assert!(
        priority("oxfmt") > 0,
        "oxfmt follows oxlint in its chain: {}",
        priority("oxfmt")
    );
    assert!(
        priority("pyproject-format") > priority("oxfmt"),
        "pyproject-format runs after every format chain"
    );
    assert!(
        priority("prek-hooks") > priority("pyproject-format"),
        "prek-hooks follows pyproject-format in the fix group"
    );
    for check in [
        "va-check",
        "go-lint",
        "java-checkstyle",
        "actionlint",
        "test-file-names",
    ] {
        assert!(
            priority(check) > priority("prek-hooks"),
            "check job {check} runs after the fix group"
        );
    }
    assert_eq!(priority("va-check"), priority("go-lint"), "the check group is parallel");
}

#[test]
fn armis_direct_tool_calls_map_to_builtins_and_catalog_tools() {
    let plan = plan(&armis());
    let config = hooks_of(&plan);
    assert!(
        config.builtin.fmt.enabled,
        "ruff format / rubyfmt / golangci-lint fmt / buf format -> fmt"
    );
    assert!(config.builtin.lint.enabled, "oxlint / ruff check -> lint");
    let lint_files = globs(config.builtin.lint.files.as_ref());
    assert!(lint_files.contains(&"*.{py,pyi}".to_string()), "{lint_files:?}");
    assert!(
        lint_files.contains(&"*.{js,jsx,ts,tsx,mjs,cjs}".to_string()),
        "{lint_files:?}"
    );
    let tools: toml::Table = toml::from_str(&plan.doc.to_string()).unwrap();
    for tool in ["golangci-lint", "buf"] {
        assert_eq!(
            tools["tools"][tool]["enabled"].as_bool(),
            Some(true),
            "[tools.{tool}] must be enabled"
        );
    }
    let stage = &config.stage_configs[&Stage::PreCommit];
    for absorbed in [
        "ruff-format",
        "ruff-check",
        "oxlint",
        "go-format",
        "ruby-format",
        "buf-format",
    ] {
        assert!(
            !stage.commands.contains_key(absorbed),
            "{absorbed} is a builtin, not a command"
        );
    }
}

#[test]
fn armis_wrapper_scripts_become_script_jobs_with_runner_and_args() {
    let config = hooks_of(&plan(&armis()));
    let gate = pre_commit_job(&config, "rms-check");
    assert_eq!(gate.script.as_deref(), Some(".lefthook/pnpm_gate.sh"));
    assert_eq!(gate.runner.as_deref(), Some("bash"));
    assert_eq!(gate.args, ["products/rms", "run type-check", "run test"]);
    assert!(gate.workspace, "no {{staged_files}} means no appended filenames");
    assert_eq!(globs(gate.exclude.as_ref()), ["products/rms/dev_tools/**"]);

    let names = pre_commit_job(&config, "test-file-names");
    assert_eq!(
        names.script.as_deref(),
        Some("dev_tools/validate_test_names/check_names.sh")
    );
    assert!(
        !names.workspace,
        "a trailing {{staged_files}} becomes appended filenames"
    );
    assert_eq!(
        globs(names.exclude.as_ref()),
        ["playground/**", "armis/tenant_server/fe/qa_tests/**"]
    );
}

#[test]
fn armis_run_jobs_keep_staged_files_templates_and_stage_fixed() {
    let config = hooks_of(&plan(&armis()));
    let pyproject = pre_commit_job(&config, "pyproject-format");
    assert_eq!(
        pyproject.run.as_deref(),
        Some("pyproject-fmt --keep-full-version {staged_files}")
    );
    assert!(pyproject.stage_fixed);
    assert_eq!(globs(pyproject.files.as_ref()), ["pyproject.toml", "**/pyproject.toml"]);
    assert_eq!(
        globs(pyproject.exclude.as_ref()),
        ["*armis/public_sdks/python/public/pyproject.toml*"],
        "the regex-escaped dot in the exclude is translated"
    );
    let buf = pre_commit_job(&config, "buf-lint");
    assert_eq!(
        buf.run.as_deref(),
        Some("buf lint $(printf -- '--path %s ' {staged_files})")
    );
    let actionlint = pre_commit_job(&config, "actionlint");
    assert!(
        actionlint.run.as_deref().unwrap().contains("-ignore 'SC2129'"),
        "behavior-changing flags keep actionlint a command"
    );
}

#[test]
fn armis_unmappable_constructs_are_reported_and_sources_kept() {
    let plan = plan(&armis());
    let lossy = unrepresented(&plan, "lefthook").join("\n");
    for needle in [
        "post-checkout/check-env-lock: NOT MIGRATED",
        "post-merge/check-env-lock: NOT MIGRATED",
        "--unsafe-fixes",
        "prek-hooks",
        "builtin fmt:",
    ] {
        assert!(lossy.contains(needle), "missing `{needle}` in:\n{lossy}");
    }
    let kept = plan
        .actions
        .iter()
        .filter(|a| matches!(a, Action::Keep { .. }))
        .filter(|a| a.path().to_string_lossy().contains("lefthook"))
        .count();
    assert_eq!(kept, 11, "every lefthook source is kept");
    assert!(
        !plan.actions.iter().any(|a| matches!(a, Action::DeleteFile(_))),
        "nothing is deleted when anything is unmapped"
    );
}

#[test]
fn armis_pre_commit_hooks_map_or_are_reported() {
    let plan = plan(&armis());
    let config = hooks_of(&plan);
    let safety = &config.builtin.file_safety;
    assert!(safety.enabled);
    assert_eq!(safety.max_added_file_kb, 1000, "--maxkb=1000");
    assert!(safety.private_key && safety.added_large_files);
    assert!(!safety.merge_conflict, "check-merge-conflict is not in the config");

    let lossy = unrepresented(&plan, "pre-commit").join("\n");
    assert!(lossy.contains("detect-aws-credentials"), "{lossy}");
    assert!(lossy.contains("trailing-whitespace: approximated"), "{lossy}");
    assert!(lossy.contains("buildifier: args --warnings=-native-build"), "{lossy}");
}

#[test]
fn a_fully_mapped_lefthook_config_is_deleted_on_write() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  parallel: true\n  jobs:\n    - name: ruff-format\n      glob: '*.py'\n      run: ruff format {staged_files}\n    - name: ruff-check\n      glob: '*.py'\n      run: ruff check {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "lefthook").absorb, Absorb::Full);

    run_migrate(migrate_args(dir.path(), true, false));
    assert!(
        !dir.path().join("lefthook.yml").exists(),
        "fully absorbed source is deleted"
    );
    let written = fs::read_to_string(dir.path().join("poly.toml")).unwrap();
    assert!(written.contains("[hooks.builtin]"), "{written}");
    assert!(written.contains("*.py"), "{written}");
    assert!(written.contains("fmt = "), "{written}");
    assert!(written.contains("lint = "), "{written}");
}

#[test]
fn a_partially_mapped_config_is_kept_and_rewriting_is_idempotent() {
    let dir = repo(&[(
        "lefthook.yml",
        "post-merge:\n  jobs:\n    - name: lock\n      run: bash check.sh {0}\npre-commit:\n  jobs:\n    - name: fmt\n      run: ruff format {staged_files}\n",
    )]);
    run_migrate(migrate_args(dir.path(), true, false));
    assert!(
        dir.path().join("lefthook.yml").exists(),
        "unmigrated {{0}} job keeps the source"
    );
    let first = fs::read_to_string(dir.path().join("poly.toml")).unwrap();
    run_migrate(migrate_args(dir.path(), true, false));
    assert_eq!(first, fs::read_to_string(dir.path().join("poly.toml")).unwrap());
}

#[test]
fn recurse_finds_directories_that_only_hold_a_hook_config() {
    let dir = repo(&[(
        "svc/lefthook.yml",
        "pre-commit:\n  jobs:\n    - run: ruff format {staged_files}\n",
    )]);
    run_migrate(migrate_args(dir.path(), true, true));
    assert!(dir.path().join("svc/poly.toml").is_file());
}

#[test]
fn legacy_commands_and_scripts_are_ordered_by_priority_then_name() {
    let dir = repo(&[
        (
            "lefthook.yml",
            "pre-commit:\n  piped: true\n  commands:\n    zz-last:\n      run: make last\n      priority: 2\n    aa-first:\n      run: make first\n      priority: 1\n  scripts:\n    lint.sh:\n      runner: bash\n",
        ),
        (".lefthook/pre-commit/lint.sh", "echo lint\n"),
    ]);
    let plan = plan(dir.path());
    let config = hooks_of(&plan);
    let stage = &config.stage_configs[&Stage::PreCommit];
    assert!(stage.piped, "piped carries over");
    assert!(stage.commands["aa-first"].priority < stage.commands["zz-last"].priority);
    let script = &stage.scripts["lint"];
    assert_eq!(script.script.as_deref(), Some(".lefthook/pre-commit/lint.sh"));
    assert_eq!(script.runner.as_deref(), Some("bash"));
    assert!(script.priority > stage.commands["zz-last"].priority);
}

#[test]
fn skip_and_only_guards_keep_run_conditions_and_report_the_rest() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  jobs:\n    - name: a\n      run: make a\n      skip: [merge, rebase]\n    - name: b\n      run: make b\n      only:\n        - run: test -f marker\n    - name: c\n      run: make c\n      skip: true\n",
    )]);
    let plan = plan(dir.path());
    let config = hooks_of(&plan);
    assert!(pre_commit_job(&config, "a").skip.is_none());
    assert!(
        pre_commit_job(&config, "b").only.is_some(),
        "a `run` condition survives"
    );
    assert!(pre_commit_job(&config, "c").skip.is_some());
    let lossy = unrepresented(&plan, "lefthook").join("\n");
    assert!(
        lossy.contains("pre-commit/a: `skip` condition(s) merge, rebase"),
        "{lossy}"
    );
}

#[test]
fn a_missing_extends_target_is_reported() {
    let dir = repo(&[(
        "lefthook.yml",
        "extends:\n  - gone.yml\npre-commit:\n  jobs:\n    - run: make x\n",
    )]);
    let plan = plan(dir.path());
    assert!(
        unrepresented(&plan, "lefthook")
            .join("\n")
            .contains("extends `gone.yml` not found")
    );
}

#[test]
fn extends_merge_jobs_by_name_with_the_main_file_winning() {
    let dir = repo(&[
        (
            "lefthook.yml",
            "extends: [base.yml]\npre-commit:\n  jobs:\n    - name: lint\n      run: make lint-strict\n",
        ),
        (
            "base.yml",
            "pre-commit:\n  jobs:\n    - name: lint\n      run: make lint\n      glob: '*.py'\n    - name: extra\n      run: make extra\n",
        ),
    ]);
    let config = hooks_of(&plan(dir.path()));
    let lint = pre_commit_job(&config, "lint");
    assert_eq!(lint.run.as_deref(), Some("make lint-strict"));
    assert_eq!(
        globs(lint.files.as_ref()),
        ["*.py"],
        "unspecified keys merge in from the base"
    );
    pre_commit_job(&config, "extra");
}

#[test]
fn shared_builtin_drops_exclusions_only_some_jobs_had() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  parallel: true\n  jobs:\n    - name: a\n      glob: '*.py'\n      exclude: 'gen/**'\n      run: ruff check {staged_files}\n    - name: b\n      glob: '*.pyi'\n      run: ruff check {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    let config = hooks_of(&plan);
    assert!(
        config.builtin.lint.exclude.is_none(),
        "an exclusion b did not have must not hide b's files"
    );
    assert!(unrepresented(&plan, "lefthook").join("\n").contains("gen/**"));
}

#[test]
fn pre_commit_prek_job_is_dropped_once_the_config_is_fully_absorbed() {
    let dir = repo(&[
        (
            "lefthook.yml",
            "pre-commit:\n  jobs:\n    - name: prek\n      run: prek run --hook-stage pre-commit --files {staged_files}\n",
        ),
        (
            ".pre-commit-config.yaml",
            "repos:\n  - repo: https://github.com/pre-commit/pre-commit-hooks\n    rev: v6.0.0\n    hooks:\n      - id: check-yaml\n      - id: check-merge-conflict\n",
        ),
    ]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "pre-commit").absorb, Absorb::Full);
    assert_eq!(result(&plan, "lefthook").absorb, Absorb::Full);
    let config = hooks_of(&plan);
    assert!(
        config
            .stage_configs
            .get(&Stage::PreCommit)
            .is_none_or(|s| s.commands.is_empty())
    );
    assert!(config.builtin.file_safety.merge_conflict);
    assert!(
        !config.builtin.file_safety.private_key,
        "only requested members stay on"
    );
}

#[test]
fn local_pre_commit_hooks_become_command_jobs() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: local\n    hooks:\n      - id: pyright\n        name: pyright\n        entry: uv run pyright\n        language: system\n        types: [python]\n        pass_filenames: false\n        exclude: ^(scripts|commands)/\n      - id: lint-md\n        entry: markdownlint\n        language: system\n        files: \\.md$\n        args: [--fix, 'a b']\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "pre-commit").absorb, Absorb::Full);
    let config = hooks_of(&plan);
    let pyright = pre_commit_job(&config, "pyright");
    assert_eq!(pyright.run.as_deref(), Some("uv run pyright"));
    assert!(pyright.workspace, "pass_filenames: false");
    assert_eq!(pyright.file_types, ["python"]);
    assert_eq!(globs(pyright.exclude.as_ref()), ["scripts/*", "commands/*"]);
    let md = pre_commit_job(&config, "lint-md");
    assert_eq!(md.run.as_deref(), Some("markdownlint --fix 'a b'"));
    assert_eq!(globs(md.files.as_ref()), ["*.md"]);
}

#[test]
fn unknown_pre_commit_hooks_are_reported_not_dropped() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://example.com/hooks\n    rev: v1\n    hooks:\n      - id: mystery-hook\n",
    )]);
    let plan = plan(dir.path());
    let lossy = unrepresented(&plan, "pre-commit").join("\n");
    assert!(lossy.contains("mystery-hook"), "{lossy}");
    assert!(lossy.contains("NOT MIGRATED"), "{lossy}");
    assert!(
        plan.actions
            .iter()
            .any(|a| matches!(a, Action::Keep { path, .. } if path.ends_with(".pre-commit-config.yaml"))),
        "the pre-commit config is kept"
    );
}

#[test]
fn no_hook_config_leaves_the_plan_untouched() {
    let dir = repo(&[("README.md", "hi\n")]);
    let plan = plan(dir.path());
    assert!(
        plan.results
            .iter()
            .all(|r| r.tool != "lefthook" && r.tool != "pre-commit")
    );
}
