use super::*;

fn target(run: &str) -> Option<(Target, Vec<&'static str>)> {
    match recognize(run) {
        Recognition::Builtin(r) => Some((r.target, r.catalog_tools.to_vec())),
        _ => None,
    }
}

#[test]
fn tokenizer_keeps_quotes_and_command_substitution_whole() {
    let tokens = tokenize("buf format -w $(printf -- '--path %s ' {staged_files}) 'a b'");
    assert_eq!(
        tokens,
        [
            "buf",
            "format",
            "-w",
            "$(printf -- '--path %s ' {staged_files})",
            "'a b'"
        ]
    );
}

#[test]
fn ruff_subcommands_map_to_fmt_and_lint() {
    assert_eq!(target("ruff format {staged_files}"), Some((Target::Fmt, vec![])));
    assert_eq!(
        target("ruff check --fix --unsafe-fixes {staged_files}"),
        Some((Target::Lint, vec![]))
    );
}

#[test]
fn unsafe_fixes_maps_onto_the_ruff_engine_option() {
    let Recognition::Builtin(r) = recognize("ruff check --fix --unsafe-fixes {staged_files}") else {
        panic!("ruff check must map to a builtin");
    };
    assert_eq!(r.options, [RUFF_UNSAFE_FIXES]);
    assert!(r.fixes);
}

fn fixes(run: &str) -> bool {
    match recognize(run) {
        Recognition::Builtin(r) => r.fixes,
        other => panic!("{run} must map to a builtin, got {other:?}"),
    }
}

#[test]
fn rewriting_invocations_are_fix_mode_and_reporting_ones_are_not() {
    assert!(fixes("ruff format {staged_files}"));
    assert!(fixes("ruff check --fix {staged_files}"));
    assert!(!fixes("ruff check {staged_files}"));
    assert!(fixes("oxfmt --write {staged_files}"));
    assert!(fixes("rubyfmt --in-place {staged_files}"));
    assert!(fixes("golangci-lint fmt -c .golangci.yml {staged_files}"));
    assert!(!fixes("golangci-lint run {staged_files}"));
    assert!(!fixes("actionlint {staged_files}"));
    assert!(fixes("npm-groovy-lint --format {staged_files}"));
}

#[test]
fn in_place_formatters_are_fix_mode_and_their_check_modes_are_not() {
    assert!(fixes(
        "bash dev_tools/checkstyle/run_palantir_format.sh --replace {staged_files}"
    ));
    assert!(!fixes(
        "bash dev_tools/checkstyle/run_checkstyle.sh -c checkstyle.xml {staged_files}"
    ));
    assert!(fixes("pyproject-fmt --keep-full-version {staged_files}"));
    assert!(!fixes("pyproject-fmt --check {staged_files}"));
    assert!(fixes("buildifier -mode=fix -lint=fix {staged_files}"));
    assert!(fixes("buildifier {staged_files}"));
    assert!(!fixes("buildifier -mode=check {staged_files}"));
    assert!(fixes("terraform fmt {staged_files}"));
    assert!(!fixes("terraform fmt -check {staged_files}"));
    assert!(fixes("tofu fmt {staged_files}"));
}

#[test]
fn buildifier_flags_split_between_the_formatting_and_every_role() {
    let settings = settings_of(
        "buildifier -mode=fix -lint=fix --warnings=-native-build {staged_files}",
        "buildifier",
    );
    assert_eq!(settings.format_extra_args, ["-lint=fix"]);
    assert_eq!(settings.extra_args, ["--warnings=-native-build"]);
    assert_eq!(
        target("buildifier -mode=check {staged_files}"),
        Some((Target::Lint, vec!["buildifier"]))
    );
    assert_eq!(
        target("buildifier -mode=fix {staged_files}"),
        Some((Target::Fmt, vec!["buildifier"]))
    );
    assert!(matches!(
        recognize("buildifier -r {staged_files}"),
        Recognition::FlagsNotRepresentable { tool: "buildifier", .. }
    ));
}

#[test]
fn catalog_backed_tools_name_their_catalog_entry() {
    assert_eq!(
        target("golangci-lint fmt -c .golangci.yml {staged_files}"),
        Some((Target::Fmt, vec!["golangci-lint"]))
    );
    assert_eq!(
        target("buf format -w $(printf -- '--path %s ' {staged_files})"),
        Some((Target::Fmt, vec!["buf"]))
    );
    assert_eq!(
        target("actionlint {staged_files}"),
        Some((Target::Lint, vec!["actionlint"]))
    );
}

#[test]
fn behavior_changing_flags_keep_the_command_a_job() {
    assert!(matches!(
        recognize("actionlint -shellcheck= {staged_files}"),
        Recognition::FlagsNotRepresentable { tool: "actionlint", .. }
    ));
    assert!(matches!(
        recognize("checkstyle -x foo {staged_files}"),
        Recognition::FlagsNotRepresentable { tool: "checkstyle", .. }
    ));
}

fn settings_of(run: &str, tool: &str) -> super::ToolSettings {
    let Recognition::Builtin(recognized) = recognize(run) else {
        panic!("`{run}` must map to a builtin");
    };
    recognized
        .settings
        .into_iter()
        .find(|(name, _)| *name == tool)
        .unwrap_or_else(|| panic!("no settings for {tool}"))
        .1
}

#[test]
fn actionlint_ignores_become_extra_args() {
    let settings = settings_of(
        "actionlint -ignore 'SC2129' -ignore 'SC2016' {staged_files}",
        "actionlint",
    );
    assert_eq!(settings.extra_args, ["-ignore", "SC2129", "-ignore", "SC2016"]);
}

fn recognized(run: &str) -> Recognized {
    match recognize(run) {
        Recognition::Builtin(recognized) => recognized,
        other => panic!("`{run}` must map to a builtin, got {other:?}"),
    }
}

#[test]
fn shfmt_indent_becomes_the_shell_engine_width() {
    let found = recognized("shfmt -i 4 -w {staged_files}");
    assert_eq!(found.target, Target::Fmt);
    assert!(found.fixes, "-w rewrites the files");
    assert_eq!(
        found.values,
        [EngineValue {
            table: &["fmt", "shell", "shfmt"],
            key: "indent_width",
            value: 4
        }]
    );
    assert_eq!(recognized("shfmt -i=2 {staged_files}").values[0].value, 2);
    assert!(!recognized("shfmt -i=2 {staged_files}").fixes);
}

#[test]
fn shfmt_indent_zero_means_tabs_and_other_layout_flags_stay_a_job() {
    let tabs = recognized("shfmt -i 0 -w {staged_files}");
    assert!(tabs.values.is_empty());
    assert_eq!(tabs.options[0].key, "use_tabs");
    assert!(matches!(
        recognize("shfmt -i 4 -ci -w {staged_files}"),
        Recognition::FlagsNotRepresentable { tool: "shfmt", .. }
    ));
    assert!(matches!(
        recognize("shfmt -i four {staged_files}"),
        Recognition::FlagsNotRepresentable { tool: "shfmt", .. }
    ));
}

#[test]
fn the_go_wrapper_carries_new_from_rev_instead_of_reporting_it_lost() {
    let settings = settings_of("bash dev_tools/go/lint_changed.sh {staged_files}", "golangci-lint");
    assert_eq!(settings.new_from_rev.as_deref(), Some("HEAD"));
    assert_eq!(settings.extra_args, ["-c", ".golangci.yml"]);
}

#[test]
fn golangci_config_flag_is_kept_as_an_extra_arg() {
    assert_eq!(
        settings_of("golangci-lint run -c custom.yml {staged_files}", "golangci-lint").extra_args,
        ["-c", "custom.yml"]
    );
}

#[test]
fn launcher_scripts_resolve_to_the_tool_they_start() {
    assert_eq!(
        target("bash dev_tools/checkstyle/run_checkstyle.sh -c checkstyle.xml -p checkstyle.properties {staged_files}"),
        Some((Target::Lint, vec!["checkstyle"]))
    );
    assert_eq!(
        settings_of(
            "bash dev_tools/checkstyle/run_checkstyle.sh -c checkstyle.xml -p checkstyle.properties {staged_files}",
            "checkstyle"
        )
        .extra_args,
        ["-c", "checkstyle.xml", "-p", "checkstyle.properties"]
    );
    let palantir = settings_of(
        "bash dev_tools/checkstyle/run_palantir_format.sh --replace {staged_files}",
        "palantir-java-format",
    );
    assert_eq!(palantir.command.as_deref(), Some("aosp"));
    assert_eq!(
        target("bash dev_tools/helm/check_charts.sh {staged_files}"),
        Some((Target::Lint, vec!["helm", "helm-template"]))
    );
    assert_eq!(
        target("bash dev_tools/go/lint_changed.sh {staged_files}"),
        Some((Target::Lint, vec!["golangci-lint"]))
    );
    assert_eq!(
        target("dev_tools/groovy/run_groovy_lint.sh --no-insight --format {staged_files}"),
        Some((Target::Fmt, vec!["npm-groovy-lint"]))
    );
}

#[test]
fn an_unrelated_script_is_not_mistaken_for_a_launcher() {
    assert_eq!(recognize("bash scripts/other.sh {staged_files}"), Recognition::Unknown);
}

#[test]
fn pyproject_fmt_flags_and_buf_lint_map_to_catalog_tools() {
    assert_eq!(
        settings_of("pyproject-fmt --keep-full-version {staged_files}", "pyproject-fmt").extra_args,
        ["--keep-full-version"]
    );
    assert_eq!(
        target("buf lint $(printf -- '--path %s ' {staged_files})"),
        Some((Target::Lint, vec!["buf"]))
    );
}

#[test]
fn the_wrappers_new_from_rev_is_carried_to_the_tool_settings() {
    let Recognition::Builtin(recognized) = recognize("bash dev_tools/go/lint_changed.sh {staged_files}") else {
        panic!("lint_changed.sh must map to a builtin");
    };
    assert!(
        recognized
            .settings
            .iter()
            .any(|(tool, settings)| *tool == "golangci-lint" && settings.new_from_rev.as_deref() == Some("HEAD"))
    );
}

#[test]
fn commands_without_a_file_list_are_never_builtins() {
    assert_eq!(recognize("ruff check ."), Recognition::Unknown);
}

#[test]
fn tools_without_a_counterpart_give_a_reason() {
    assert_eq!(recognize("some-unknown-tool {staged_files}"), Recognition::Unknown);
}

#[test]
fn compound_commands_are_not_recognized() {
    assert_eq!(
        recognize("ruff format {staged_files} && echo done"),
        Recognition::Unknown
    );
}

#[test]
fn placeholder_scan_ignores_shell_expansions() {
    assert_eq!(placeholders("echo ${HOME} {staged_files} {0}"), ["staged_files", "0"]);
    assert_eq!(
        unsupported_placeholders("a {staged_files} {0} {push_files} {git_args} {2}"),
        ["push_files"]
    );
}

#[test]
fn git_argument_placeholders_are_recognised() {
    assert!(uses_git_args("{0}"));
    assert!(uses_git_args("{2}"));
    assert!(uses_git_args("{git_args}"));
    assert!(!uses_git_args("{staged_files}"));
}
