//! A bad `[components]` section is a config error for every command that loads the config, not
//! only for `poly config validate`: exit 2, nothing checked.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn repo(config: &str) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("poly.toml"), config).expect("write poly.toml");
    std::fs::write(tmp.path().join("a.py"), "x = 1\n").expect("write a.py");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    tmp
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(cwd)
        .env("POLY_CACHE_HOME", cwd.join(".cache"))
        .env("POLY_OFFLINE", "1")
        .output()
        .expect("poly invocation")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn lint_and_fmt_reject_the_error_class_configs() {
    let cases = [
        ("[components]\nenable = [\"bogus\"]\n", "`bogus`"),
        ("[components]\nenable = [\"python\"]\ndisable = [\"python\"]\n", "both"),
    ];
    for (config, expected) in cases {
        let tmp = repo(config);
        for args in [&["lint", "--workspace", "never", "."][..], &["fmt", "."][..]] {
            let output = run(tmp.path(), args);
            let stderr = stderr_of(&output);
            assert_eq!(output.status.code(), Some(2), "{args:?} on {config}: {stderr}");
            assert!(
                stderr.contains("invalid [components] config") && stderr.contains(expected),
                "{args:?} on {config}: {stderr}"
            );
        }
    }
}

#[test]
fn warning_class_configs_run_and_warn_on_stderr() {
    let cases = [
        // poly: allow[typo] a deliberate misspelling of a component name
        ("[components]\ndisable = [\"pyhton\"]\n", "`pyhton`"),
        (
            "[components]\ndisable = [\"python\"]\n[lint.python.ruff]\nselect = [\"E\"]\n",
            "component `python` is disabled",
        ),
    ];
    for (config, expected) in cases {
        let tmp = repo(config);
        let output = run(tmp.path(), &["lint", "--workspace", "never", "."]);
        let stderr = stderr_of(&output);
        assert!(!stderr.contains("invalid [components] config"), "{config}: {stderr}");
        assert!(
            stderr.contains("warning:") && stderr.contains(expected),
            "{config}: {stderr}"
        );
        assert_eq!(stderr.matches(expected).count(), 1, "once per run: {stderr}");
    }
}

#[test]
fn an_extends_base_engine_table_with_a_child_disable_warns_and_runs() {
    let tmp = repo("extends = [\"base.toml\"]\n[components]\ndisable = [\"python\"]\n");
    std::fs::write(tmp.path().join("base.toml"), "[engines.ruff]\nenabled = true\n").expect("base");
    let output = run(tmp.path(), &["lint", "--workspace", "never", "."]);
    let stderr = stderr_of(&output);
    assert!(!stderr.contains("invalid [components] config"), "{stderr}");
    assert!(
        stderr.contains("warning:") && stderr.contains("engine `ruff`"),
        "{stderr}"
    );
}

#[test]
fn a_nested_config_disabling_a_component_under_a_root_engine_table_warns_and_runs() {
    let tmp = repo("[engines.ruff]\nenabled = true\n");
    std::fs::create_dir(tmp.path().join("sub")).expect("sub");
    std::fs::write(
        tmp.path().join("sub/poly.toml"),
        "[components]\ndisable = [\"python\"]\n",
    )
    .expect("nested");
    std::fs::write(tmp.path().join("sub/b.py"), "y = 2\n").expect("b.py");
    let output = run(tmp.path(), &["lint", "--workspace", "never", "."]);
    let stderr = stderr_of(&output);
    assert!(!stderr.contains("invalid [components] config"), "{stderr}");
    assert!(
        stderr.contains("warning:") && stderr.contains("engine `ruff`"),
        "{stderr}"
    );
}

#[test]
fn recovery_commands_work_on_a_bad_components_config() {
    let tmp = repo("[components]\nenable = [\"bogus\"]\n");
    for args in [
        &["config", "show"][..],
        &["components", "status", "."][..],
        &["components", "list"][..],
        &["components", "uninstall", "python"][..],
    ] {
        let output = run(tmp.path(), args);
        let stderr = stderr_of(&output);
        assert!(output.status.code().is_some_and(|c| c != 2), "{args:?}: {stderr}");
        assert!(!stderr.contains("invalid [components] config"), "{args:?}: {stderr}");
    }
}

#[test]
fn config_validate_keeps_reporting_the_warning_class_as_errors() {
    // poly: allow[typo] a deliberate misspelling of a component name
    let tmp = repo("[components]\ndisable = [\"pyhton\"]\n");
    let output = run(tmp.path(), &["config", "validate"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_valid_components_section_still_runs() {
    let tmp = repo("[components]\nenable = [\"python\"]\ndisable = [\"ruby\"]\n");
    let output = run(tmp.path(), &["components", "status", "."]);
    assert!(output.status.code().is_some_and(|c| c != 2), "{}", stderr_of(&output));
    assert!(!stderr_of(&output).contains("invalid [components] config"));
}
