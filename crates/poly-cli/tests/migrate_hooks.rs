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
        priority("prek-hooks") > priority("oxfmt"),
        "prek-hooks follows every format chain in the fix group"
    );
    for check in ["va-check", "test-file-names"] {
        assert!(
            priority(check) > priority("prek-hooks"),
            "check job {check} runs after the fix group"
        );
    }
    assert_eq!(
        priority("va-check"),
        priority("rms-check"),
        "the check group is parallel"
    );
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
    let lint_files: Vec<String> = config
        .builtin
        .lint
        .jobs
        .iter()
        .flat_map(|job| globs(job.files.as_ref()))
        .collect();
    assert!(lint_files.contains(&"*.{py,pyi}".to_string()), "{lint_files:?}");
    assert!(
        lint_files.contains(&"*.{js,jsx,ts,tsx,mjs,cjs}".to_string()),
        "{lint_files:?}"
    );
    let tools: toml::Table = toml::from_str(&plan.doc.to_string()).unwrap();
    for tool in [
        "golangci-lint",
        "buf",
        "actionlint",
        "buildifier",
        "checkstyle",
        "palantir-java-format",
        "pyproject-fmt",
        "helm",
        "helm-template",
        "npm-groovy-lint",
    ] {
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
        "buf-lint",
        "actionlint",
        "pyproject-format",
        "groovy-lint",
        "groovy-format",
    ] {
        assert!(
            !stage.commands.contains_key(absorbed),
            "{absorbed} is a builtin, not a command"
        );
        assert!(
            !stage.scripts.contains_key(absorbed),
            "{absorbed} is a builtin, not a script"
        );
    }
    for wrapper in ["java-format", "java-checkstyle", "go-lint", "helm-check"] {
        assert!(
            !stage.scripts.contains_key(wrapper),
            "the {wrapper} wrapper script is now a [tools] entry"
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
fn armis_tool_flags_move_to_the_tools_table() {
    let plan = plan(&armis());
    let table: toml::Table = toml::from_str(&plan.doc.to_string()).unwrap();
    let tools = &table["tools"];
    let strings = |tool: &str, key: &str| -> Vec<String> {
        tools[tool][key]
            .as_array()
            .unwrap_or_else(|| panic!("[tools.{tool}] has no {key}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        strings("actionlint", "extra_args"),
        ["-ignore", "SC2129", "-ignore", "SC2016"]
    );
    assert_eq!(
        strings("checkstyle", "extra_args"),
        ["-c", "checkstyle.xml", "-p", "checkstyle.properties"]
    );
    assert_eq!(strings("golangci-lint", "extra_args"), ["-c", ".golangci.yml"]);
    assert_eq!(
        tools["golangci-lint"]["new_from_rev"].as_str(),
        Some("HEAD"),
        "the wrapper's --new-from-rev is carried, not reported lost"
    );
    assert_eq!(strings("pyproject-fmt", "extra_args"), ["--keep-full-version"]);
    assert_eq!(strings("buildifier", "format_extra_args"), ["-lint=fix"]);
    assert_eq!(tools["palantir-java-format"]["command"].as_str(), Some("aosp"));
    assert_eq!(
        strings("pyproject-fmt", "exclude"),
        ["*armis/public_sdks/python/public/pyproject.toml*"],
        "the regex-escaped dot in the exclude is translated"
    );
}

#[test]
fn armis_wrapper_behaviour_poly_does_not_reproduce_is_reported() {
    let lossy = unrepresented(&plan(&armis()), "lefthook").join("\n");
    for needle in ["GOOS", "chart_values", ".java-version"] {
        assert!(lossy.contains(needle), "missing `{needle}` in:\n{lossy}");
    }
}

#[test]
fn armis_unmappable_constructs_are_reported_and_sources_kept() {
    let plan = plan(&armis());
    let lossy = unrepresented(&plan, "lefthook").join("\n");
    assert!(lossy.contains("prek-hooks"), "missing `prek-hooks` in:\n{lossy}");
    for resolved in ["NOT MIGRATED", "--unsafe-fixes", "exclude pattern(s) dropped"] {
        assert!(!lossy.contains(resolved), "`{resolved}` is now handled:\n{lossy}");
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
    assert!(safety.aws_credentials, "detect-aws-credentials is a file_safety member");
    assert!(!lossy.contains("detect-aws-credentials"), "{lossy}");
    assert!(lossy.contains("trailing-whitespace: approximated"), "{lossy}");
    assert!(
        !lossy.contains("buildifier: args"),
        "buildifier's args now configure [tools.buildifier]: {lossy}"
    );
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
        "post-merge:\n  jobs:\n    - name: lock\n      run: bash check.sh {push_files}\npre-commit:\n  jobs:\n    - name: fmt\n      run: ruff format {staged_files}\n",
    )]);
    run_migrate(migrate_args(dir.path(), true, false));
    assert!(
        dir.path().join("lefthook.yml").exists(),
        "an unmigrated {{push_files}} job keeps the source"
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
    assert!(
        !dir.path().join("svc/lefthook.yml").exists(),
        "poly runs a nested [hooks] table on commit, so a fully absorbed nested source is deleted"
    );
}

#[test]
fn a_nested_hook_config_that_loses_something_is_kept() {
    let dir = repo(&[(
        "svc/lefthook.yml",
        "post-merge:\n  jobs:\n    - name: lock\n      run: bash check.sh {push_files}\npre-commit:\n  jobs:\n    - run: ruff format {staged_files}\n",
    )]);
    run_migrate(migrate_args(dir.path(), true, true));
    assert!(dir.path().join("svc/poly.toml").is_file());
    assert!(dir.path().join("svc/lefthook.yml").is_file(), "an unmigrated job keeps the source");
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
fn jobs_sharing_a_builtin_keep_their_own_exclusions() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  parallel: true\n  jobs:\n    - name: a\n      glob: '*.py'\n      exclude: 'gen/**'\n      run: ruff check {staged_files}\n    - name: b\n      glob: '*.pyi'\n      run: ruff check {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(
        result(&plan, "lefthook").absorb,
        Absorb::Full,
        "no exclusion is dropped"
    );
    let lint = hooks_of(&plan).builtin.lint;
    assert!(lint.exclude.is_none(), "the shared builtin carries no scope of its own");
    let scopes: Vec<(Vec<String>, Vec<String>)> = lint
        .jobs
        .iter()
        .map(|job| (globs(job.files.as_ref()), globs(job.exclude.as_ref())))
        .collect();
    assert_eq!(
        scopes,
        [
            (vec!["*.py".to_string()], vec!["gen/**".to_string()]),
            (vec!["*.pyi".to_string()], Vec::new()),
        ]
    );
}

#[test]
fn identical_scopes_collapse_into_one_flat_builtin() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  parallel: true\n  jobs:\n    - name: a\n      glob: '*.py'\n      run: ruff check {staged_files}\n    - name: b\n      glob: '*.py'\n      run: oxlint {staged_files}\n",
    )]);
    let lint = hooks_of(&plan(dir.path())).builtin.lint;
    assert!(lint.jobs.is_empty());
    assert_eq!(globs(lint.files.as_ref()), ["*.py"]);
}

#[test]
fn file_safety_members_keep_the_exclusions_of_their_own_hook() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://github.com/pre-commit/pre-commit-hooks\n    rev: v6.0.0\n    hooks:\n      - id: check-merge-conflict\n        exclude: ^demo/data/\n      - id: detect-private-key\n      - id: check-case-conflict\n        exclude: ^demo/data/\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "pre-commit").absorb, Absorb::Full);
    let safety = hooks_of(&plan).builtin.file_safety;
    let scopes: Vec<(Vec<String>, Vec<String>)> = safety
        .jobs
        .iter()
        .map(|job| {
            let mut checks = job.checks.clone();
            checks.sort();
            (checks, globs(job.exclude.as_ref()))
        })
        .collect();
    let scoped = (
        vec!["case_conflict".to_string(), "merge_conflict".to_string()],
        vec!["demo/data/*".to_string()],
    );
    assert!(scopes.contains(&scoped), "{scopes:?}");
    assert!(
        scopes.contains(&(vec!["private_key".to_string()], Vec::new())),
        "{scopes:?}"
    );
}

fn parsed(plan: &MigrationPlan) -> toml::Table {
    toml::from_str(&plan.doc.to_string()).unwrap()
}

#[test]
fn rewriting_jobs_become_fix_mode_builtins_and_checking_jobs_do_not() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  parallel: true\n  jobs:\n    - name: fmt\n      glob: '*.py'\n      stage_fixed: true\n      run: ruff format {staged_files}\n    - name: check\n      glob: '*.rs'\n      run: oxlint {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "lefthook").absorb, Absorb::Full);
    let builtin = hooks_of(&plan).builtin;
    assert!(builtin.fmt.fix, "`ruff format` rewrites its files");
    assert!(!builtin.lint.fix, "`oxlint` without --fix only reports");
    let doc = parsed(&plan);
    assert_eq!(doc["hooks"]["builtin"]["fmt"]["fix"].as_bool(), Some(true));
    assert!(doc["hooks"]["builtin"]["lint"].get("fix").is_none());
}

#[test]
fn a_fixer_without_stage_fixed_is_reported_as_restaged() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  jobs:\n    - name: fmt\n      run: ruff format {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    let notes = result(&plan, "lefthook").notes.join("\n");
    assert!(notes.contains("re-stages them: pre-commit/fmt"), "{notes}");
}

#[test]
fn layered_builtins_keep_their_priority() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  jobs:\n    - group:\n        piped: true\n        jobs:\n          - name: fmt\n            glob: '*.py'\n            run: ruff format {staged_files}\n          - name: fix\n            glob: '*.py'\n            run: ruff check --fix {staged_files}\n",
    )]);
    let builtin = hooks_of(&plan(dir.path())).builtin;
    assert_eq!(builtin.fmt.priority, 0);
    assert_eq!(builtin.lint.priority, 1, "ruff check runs after ruff format");
}

#[test]
fn unsafe_fixes_becomes_the_ruff_engine_option() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  jobs:\n    - name: fix\n      glob: '*.py'\n      stage_fixed: true\n      run: ruff check --fix --unsafe-fixes {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "lefthook").absorb, Absorb::Full);
    assert_eq!(
        parsed(&plan)["lint"]["python"]["ruff"]["unsafe_fixes"].as_bool(),
        Some(true)
    );
}

#[test]
fn a_lefthook_shfmt_indent_flag_becomes_the_shell_engine_width() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  jobs:\n    - name: shfmt\n      glob: '*.sh'\n      stage_fixed: true\n      run: shfmt -i 4 -w {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "lefthook").absorb, Absorb::Full);
    assert_eq!(parsed(&plan)["fmt"]["shell"]["shfmt"]["indent_width"].as_integer(), Some(4));
    assert!(hooks_of(&plan).builtin.fmt.enabled);
}

#[test]
fn a_pre_commit_shfmt_indent_argument_becomes_the_shell_engine_width() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://github.com/scop/pre-commit-shfmt\n    rev: v3.10.0-1\n    hooks:\n      - id: shfmt\n        args: [-i, '4', -w]\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "pre-commit").absorb, Absorb::Full);
    assert_eq!(parsed(&plan)["fmt"]["shell"]["shfmt"]["indent_width"].as_integer(), Some(4));
}

#[test]
fn a_shfmt_indent_of_zero_means_tabs() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  jobs:\n    - name: shfmt\n      glob: '*.sh'\n      run: shfmt -i 0 {staged_files}\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(parsed(&plan)["fmt"]["shell"]["shfmt"]["use_tabs"].as_bool(), Some(true));
}

#[test]
fn git_argument_placeholders_are_migrated_not_dropped() {
    let dir = repo(&[
        (
            "lefthook.yml",
            "post-checkout:\n  jobs:\n    - name: lock\n      run: bash .lefthook/lock.sh {0}\npost-merge:\n  jobs:\n    - name: second\n      run: echo {2} {git_args}\n",
        ),
        (".lefthook/lock.sh", "echo lock\n"),
    ]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "lefthook").absorb, Absorb::Full);
    let config = hooks_of(&plan);
    let lock = &config.stage_configs[&Stage::PostCheckout].commands["lock"];
    assert_eq!(lock.run.as_deref(), Some("bash .lefthook/lock.sh {0}"));
    assert!(lock.workspace, "git arguments are not file lists");
    let second = &config.stage_configs[&Stage::PostMerge].commands["second"];
    assert_eq!(second.run.as_deref(), Some("echo {2} {git_args}"));
}

#[test]
fn aws_credentials_maps_to_a_file_safety_member() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://github.com/pre-commit/pre-commit-hooks\n    rev: v6.0.0\n    hooks:\n      - id: detect-aws-credentials\n        args: [--allow-missing-credentials]\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "pre-commit").absorb, Absorb::Full);
    assert!(hooks_of(&plan).builtin.file_safety.aws_credentials);
}

#[test]
fn aws_credentials_without_allow_missing_is_reported_as_relaxed() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://github.com/pre-commit/pre-commit-hooks\n    rev: v6.0.0\n    hooks:\n      - id: detect-aws-credentials\n",
    )]);
    let lossy = unrepresented(&plan(dir.path()), "pre-commit").join("\n");
    assert!(
        lossy.contains("pre-commit fails when no AWS credentials are configured"),
        "{lossy}"
    );
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

#[test]
fn recurse_skips_virtualenvs_gitignored_trees_and_build_output() {
    let ruff = "[tool.ruff.lint]\nselect = [\"E\"]\n";
    let dir = repo(&[
        ("svc/pyproject.toml", ruff),
        (".venv/lib/python3.12/site-packages/pkg/pyproject.toml", ruff),
        ("renamed_env/pyvenv.cfg", "home = /usr/bin\n"),
        ("renamed_env/lib/pkg/pyproject.toml", ruff),
        ("generated/pyproject.toml", ruff),
        ("bazel-out/pkg/pyproject.toml", ruff),
        (".gitignore", "generated/\n"),
    ]);
    run_migrate(migrate_args(dir.path(), true, true));
    assert!(
        dir.path().join("svc/poly.toml").is_file(),
        "the real project is migrated"
    );
    for skipped in [
        ".venv/lib/python3.12/site-packages/pkg",
        "renamed_env/lib/pkg",
        "generated",
        "bazel-out/pkg",
    ] {
        assert!(
            !dir.path().join(skipped).join("poly.toml").exists(),
            "{skipped} must not receive a poly.toml"
        );
    }
}

#[test]
fn a_nested_ruff_config_replaces_the_ancestors_and_says_so() {
    let ruff = "[tool.ruff.lint]\nselect = [\"E\"]\n[tool.ruff.lint.per-file-ignores]\n\"a.py\" = [\"F401\"]\n";
    let dir = repo(&[("pyproject.toml", ruff), ("svc/pyproject.toml", ruff)]);
    run_migrate(migrate_args(dir.path(), true, true));
    let root: toml::Table = toml::from_str(&fs::read_to_string(dir.path().join("poly.toml")).unwrap()).unwrap();
    let nested: toml::Table = toml::from_str(&fs::read_to_string(dir.path().join("svc/poly.toml")).unwrap()).unwrap();
    assert!(root["lint"]["python"]["ruff"].get("inherit_mode").is_none());
    assert_eq!(
        nested["lint"]["python"]["ruff"]["inherit_mode"].as_str(),
        Some("replace")
    );
    assert_eq!(nested["per-file-ignores"]["inherit_mode"].as_str(), Some("replace"));
}

#[test]
fn the_root_hook_config_is_still_deleted_when_fully_absorbed_under_recurse() {
    let dir = repo(&[
        (
            "lefthook.yml",
            "pre-commit:\n  jobs:\n    - run: ruff format {staged_files}\n",
        ),
        ("svc/pyproject.toml", "[tool.ruff.lint]\nselect = [\"E\"]\n"),
    ]);
    run_migrate(migrate_args(dir.path(), true, true));
    assert!(!dir.path().join("lefthook.yml").exists());
}

#[test]
fn pre_commit_checks_and_local_reporters_run_after_the_fixers() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://github.com/pre-commit/pre-commit-hooks\n    rev: v6.0.0\n    hooks:\n      - id: trailing-whitespace\n      - id: check-yaml\n  - repo: local\n    hooks:\n      - id: gate\n        entry: make gate\n        language: system\n      - id: md\n        entry: markdownlint --fix\n        language: system\n",
    )]);
    let config = hooks_of(&plan(dir.path()));
    assert_eq!(config.builtin.fmt.priority, 0, "the fixer keeps the first band");
    assert_eq!(config.builtin.lint.priority, 1, "check-yaml only reports");
    let stage = &config.stage_configs[&Stage::PreCommit];
    assert_eq!(stage.commands["gate"].priority, 1, "a reporting local hook follows the fixers");
    assert_eq!(stage.commands["md"].priority, 0, "a local hook that rewrites is a fixer");
}

#[test]
fn flat_lefthook_checks_run_after_flat_lefthook_fixers() {
    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  parallel: true\n  jobs:\n    - name: fmt\n      glob: '*.py'\n      run: ruff format {staged_files}\n    - name: check\n      glob: '*.rs'\n      run: oxlint {staged_files}\n",
    )]);
    let builtin = hooks_of(&plan(dir.path())).builtin;
    assert_eq!(builtin.fmt.priority, 0);
    assert_eq!(builtin.lint.priority, 1);
}

#[test]
fn armis_check_only_builtins_follow_every_fixing_builtin() {
    let builtin = hooks_of(&plan(&armis())).builtin;
    let jobs: Vec<_> = builtin.fmt.jobs.iter().chain(builtin.lint.jobs.iter()).collect();
    let priority = |job: &&poly_config::BuiltinJob| job.priority.unwrap_or(0);
    let last_fixer = jobs
        .iter()
        .filter(|job| job.fix == Some(true))
        .map(priority)
        .max()
        .expect("armis has fixing builtins");
    for job in jobs.iter().filter(|job| job.fix != Some(true)) {
        assert!(
            priority(job) > last_fixer,
            "check-only {:?} runs at {} but a fixer runs at {last_fixer}",
            globs(job.files.as_ref()),
            priority(job)
        );
    }
}

#[test]
fn in_place_formatters_migrate_as_fix_mode_builtins() {
    let armis_plan = plan(&armis());
    let fmt_fix = |files: &str| {
        hooks_of(&armis_plan)
            .builtin
            .fmt
            .jobs
            .iter()
            .find(|job| globs(job.files.as_ref()).iter().any(|glob| glob == files))
            .and_then(|job| job.fix)
    };
    assert_eq!(fmt_fix("*.java"), Some(true), "palantir --replace rewrites");
    assert_eq!(fmt_fix("pyproject.toml"), Some(true), "pyproject-fmt rewrites in place");
    assert_eq!(fmt_fix("*BUILD"), Some(true), "buildifier -mode=fix rewrites");

    let dir = repo(&[(
        "lefthook.yml",
        "pre-commit:\n  parallel: true\n  jobs:\n    - name: bazel\n      glob: '*.bzl'\n      run: buildifier -mode=fix -lint=fix {staged_files}\n    - name: bazel-check\n      glob: 'BUILD'\n      run: buildifier -mode=check {staged_files}\n    - name: tf\n      glob: '*.tf'\n      run: terraform fmt -check {staged_files}\n",
    )]);
    let builtin = hooks_of(&plan(dir.path())).builtin;
    let fix_of = |jobs: &[poly_config::BuiltinJob], files: &str| {
        jobs.iter()
            .find(|job| globs(job.files.as_ref()).iter().any(|glob| glob == files))
            .map(|job| job.fix)
    };
    assert_eq!(fix_of(&builtin.fmt.jobs, "*.bzl"), Some(Some(true)));
    assert_eq!(fix_of(&builtin.fmt.jobs, "*.tf"), Some(None), "-check only reports");
    assert_eq!(globs(builtin.lint.files.as_ref()), ["BUILD"], "-mode=check is the lint role");
    assert!(!builtin.lint.fix);
}

#[test]
fn requirements_txt_fixer_maps_to_the_opt_in_file_safety_member() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://github.com/pre-commit/pre-commit-hooks\n    rev: v6.0.0\n    hooks:\n      - id: requirements-txt-fixer\n",
    )]);
    let plan = plan(dir.path());
    assert_eq!(result(&plan, "pre-commit").absorb, Absorb::Full, "the hook is no longer unmapped");
    let safety = hooks_of(&plan).builtin.file_safety;
    assert!(safety.requirements_txt_fixer);
    assert!(!safety.merge_conflict, "only the requested member is on");
    assert_eq!(globs(safety.files.as_ref()), ["*requirements*.txt"]);
}

fn precommit_repo(repo_url: &str, rev: &str, hook: &str) -> tempfile::TempDir {
    repo(&[(
        ".pre-commit-config.yaml",
        &format!("repos:\n  - repo: {repo_url}\n    rev: {rev}\n    hooks:\n      - id: {hook}\n"),
    )])
}

#[test]
fn a_pre_commit_rev_for_a_downloadable_tool_at_an_unknown_release_is_a_loss() {
    let dir = precommit_repo("https://github.com/golangci/golangci-lint", "v2.1.6", "golangci-lint");
    let plan = plan(dir.path());
    let lossy = unrepresented(&plan, "pre-commit").join("\n");
    assert!(lossy.contains("pins golangci-lint 2.1.6, which poly cannot download"), "{lossy}");
    let tools = parsed(&plan);
    assert!(tools["tools"]["golangci-lint"].get("version").is_none());
}

#[test]
fn a_pre_commit_rev_poly_can_download_becomes_the_tool_version() {
    let dir = precommit_repo("https://github.com/keith/pre-commit-buildifier", "8.5.1", "buildifier");
    let tools = parsed(&plan(dir.path()));
    assert_eq!(tools["tools"]["buildifier"]["version"].as_str(), Some("8.5.1"));
    assert_eq!(tools["tools"]["buildifier"]["install"].as_str(), Some("auto"));
}

#[test]
fn a_pre_commit_rev_missing_from_a_downloadable_tools_releases_keeps_the_source() {
    let dir = precommit_repo("https://github.com/keith/pre-commit-buildifier", "7.0.0", "buildifier");
    let lossy = unrepresented(&plan(dir.path()), "pre-commit").join("\n");
    assert!(lossy.contains("pins buildifier 7.0.0, which poly cannot download"), "{lossy}");
}

#[test]
fn tool_versions_pins_are_reported_for_a_lefthook_catalog_tool() {
    let dir = repo(&[
        (".tool-versions", "nodejs 22.1.0\ngolangci-lint 1.64.8\n"),
        (
            "lefthook.yml",
            "pre-commit:\n  jobs:\n    - name: go\n      glob: '*.go'\n      run: golangci-lint run {staged_files}\n",
        ),
    ]);
    let lossy = unrepresented(&plan(dir.path()), "lefthook").join("\n");
    assert!(lossy.contains(".tool-versions pins golangci-lint 1.64.8, which poly cannot download"), "{lossy}");
}

#[test]
fn a_tool_versions_pin_for_a_downloadable_tool_at_an_unknown_release_is_a_loss() {
    let dir = repo(&[
        (".checkstyle-version", "1.0.0\n"),
        (
            "lefthook.yml",
            "pre-commit:\n  jobs:\n    - name: java\n      glob: '*.java'\n      run: checkstyle -c checkstyle.xml {staged_files}\n",
        ),
    ]);
    let lossy = unrepresented(&plan(dir.path()), "lefthook").join("\n");
    assert!(lossy.contains(".checkstyle-version pins checkstyle 1.0.0, which poly cannot download"), "{lossy}");
}

#[test]
fn hadolint_buf_and_tflint_pre_commit_hooks_map_to_catalog_tools() {
    let dir = repo(&[(
        ".pre-commit-config.yaml",
        "repos:\n  - repo: https://github.com/hadolint/hadolint\n    rev: v2.12.0\n    hooks:\n      - id: hadolint\n  - repo: https://github.com/bufbuild/buf\n    rev: v1.50.0\n    hooks:\n      - id: buf-lint\n      - id: buf-format\n  - repo: https://github.com/antonbabenko/pre-commit-terraform\n    rev: v1.96.0\n    hooks:\n      - id: terraform_tflint\n",
    )]);
    let plan = plan(dir.path());
    // The catalog downloads other releases of hadolint and buf, so the older revs
    // these hooks pin are reported as a loss rather than silently replaced.
    let lossy = unrepresented(&plan, "pre-commit").join("\n");
    assert!(lossy.contains("pins hadolint 2.12.0, which poly cannot download"), "{lossy}");
    assert!(lossy.contains("pins buf 1.50.0, which poly cannot download"), "{lossy}");
    let doc = parsed(&plan);
    for tool in ["hadolint", "buf", "tflint"] {
        assert_eq!(doc["tools"][tool]["enabled"].as_bool(), Some(true), "[tools.{tool}]");
    }
    assert_eq!(doc["hooks"]["builtin"]["fmt"]["fix"].as_bool(), Some(true), "buf format rewrites");
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn migrating_the_armis_fixture_writes_a_config_the_loader_accepts() {
    let dir = tempdir().unwrap();
    copy_tree(&armis(), dir.path());

    run_migrate(migrate_args(dir.path(), true, false));

    let written = dir.path().join("poly.toml");
    assert!(written.is_file(), "--write must produce poly.toml");
    let config = poly_config::PolyConfig::load_file(&written)
        .unwrap_or_else(|error| panic!("the migrated config must load: {error:#}\n{}", fs::read_to_string(&written).unwrap()));
    assert!(config.hooks.present);
    assert!(config.hooks.builtin.fmt.enabled && config.hooks.builtin.lint.enabled);
    assert!(config.hooks.builtin.file_safety.enabled);
    assert!(config.tools.iter().any(|(name, tool)| name == "buildifier" && tool.enabled));
    assert!(
        config.hooks.stage_configs.contains_key(&Stage::PreCommit),
        "the lefthook jobs that stay jobs are written"
    );
    assert!(dir.path().join("lefthook.yml").exists(), "unmapped constructs keep the source");
}
