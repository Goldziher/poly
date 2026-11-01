//! Native CMD effect tests: shape-only quoting assertions cannot prove child argv.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::model::Hook;
use crate::runner::exec::build_command;

const CAPTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/windows_capture.py"
));

struct Probe {
    _temporary: TempDir,
    cwd: PathBuf,
    python: PathBuf,
    capture: PathBuf,
}

impl Probe {
    fn new() -> Self {
        let base = std::env::var_os("POLY_TEST_PYTHON").expect("set POLY_TEST_PYTHON to the managed native Python");
        let identity = Command::new(&base)
            .args([
                "-c",
                "import sys; assert sys.platform == 'win32'; print(sys.version.split()[0])",
            ])
            .output()
            .expect("managed Python must launch");
        assert!(identity.status.success(), "{identity:?}");
        let manifest: toml::Value =
            toml::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../mise.toml")))
                .expect("committed toolchain manifest must parse");
        let expected = manifest["tools"]["python"]
            .as_str()
            .expect("native Python must be pinned");
        assert_eq!(String::from_utf8_lossy(&identity.stdout).trim(), expected);
        let temporary = tempfile::Builder::new()
            .prefix("poly cmd transport ")
            .tempdir()
            .unwrap();
        let environment = temporary.path().join("python environment with spaces");
        let setup = Command::new(base)
            .args(["-m", "venv", "--without-pip"])
            .arg(&environment)
            .output()
            .expect("stdlib venv must launch without installing packages");
        assert!(setup.status.success(), "{setup:?}");
        let cwd = temporary.path().join("working directory with spaces");
        std::fs::create_dir(&cwd).unwrap();
        let probe = Self {
            python: environment.join("Scripts/python.exe"),
            capture: cwd.join("capture.json"),
            cwd: dunce::canonicalize(cwd).unwrap(),
            _temporary: temporary,
        };
        probe.assert_direct_endpoint();
        probe
    }

    fn assert_direct_endpoint(&self) {
        let output = Command::new(&self.python)
            .args(["-c", CAPTURE.trim(), "direct endpoint"])
            .env("POLY_CMD_CAPTURE", &self.capture)
            .current_dir(&self.cwd)
            .output()
            .expect("spaced native Python must launch directly");
        assert!(output.status.success(), "{output:?}");
        self.assert_capture(&["direct endpoint"]);
        std::fs::remove_file(&self.capture).unwrap();
    }

    fn hook(&self, args: &[&str]) -> Hook {
        let line = format!("\"{}\" -c \"{}\"", self.python.display(), CAPTURE.trim());
        let mut hook = Hook::run("windows-cmd-transport", line);
        hook.args = args.iter().map(|value| (*value).to_owned()).collect();
        hook.pass_filenames = false;
        hook
    }

    fn execute(&self, hook: &Hook, files: &[&Path]) -> std::process::Output {
        let mut command = build_command(hook, &self.cwd, files, None, None);
        command.check(false);
        command
            .env("POLY_CMD_CAPTURE", &self.capture)
            .env("POLY_CMD_PROBE", "expanded sentinel");
        command.output().expect("the actual CMD process must launch")
    }

    fn assert_capture(&self, args: &[&str]) {
        let bytes = std::fs::read(&self.capture).expect("child must produce a nonempty JSON capture");
        let actual: Value = serde_json::from_slice(&bytes).expect("capture must be valid JSON");
        assert_eq!(actual, json!({"argv": args, "cwd": self.cwd}));
    }

    fn assert_success(&self, hook: &Hook, files: &[&Path], args: &[&str]) {
        let output = self.execute(hook, files);
        assert!(output.status.success(), "{output:?}");
        self.assert_capture(args);
    }
}

#[test]
fn windows_cmd_transport_round_trips_quoted_source_spaced_executable_and_cwd() {
    let probe = Probe::new();
    let args = ["validate", "--no-local", "two words", "quote\"inside", "trailing\\"];
    probe.assert_success(&probe.hook(&args), &[], &args);
}

#[test]
fn windows_cmd_transport_preserves_empty_quotes_spaces_and_backslashes() {
    let probe = Probe::new();
    let args = [
        "",
        "   ",
        "\"",
        " leading ",
        "\\",
        "\\\\",
        "\\\\\\",
        "a\\\"b",
        "a\\\\\"b",
        "雪",
    ];
    probe.assert_success(&probe.hook(&args), &[], &args);
}

#[test]
fn windows_cmd_transport_keeps_appended_operators_literal() {
    let probe = Probe::new();
    let args = [
        "x & echo injected>marker.txt",
        "x|more",
        "x>marker.txt",
        "x<marker.txt",
        "(parentheses)",
        "caret^value",
        "foo\" & echo injected>marker.txt & \"bar",
    ];
    probe.assert_success(&probe.hook(&args), &[], &args);
    assert!(!probe.cwd.join("marker.txt").exists());
}

#[test]
fn windows_cmd_transport_preserves_literal_percent_and_exclamation() {
    let probe = Probe::new();
    let args = [
        "%POLY_CMD_PROBE%",
        "!POLY_CMD_PROBE!",
        "100%done",
        "100%%done",
        "%",
        "!",
        "tail%",
    ];
    probe.assert_success(&probe.hook(&args), &[], &args);
}

#[test]
fn windows_cmd_transport_preserves_configured_chaining_pipes_and_redirection() {
    let probe = Probe::new();
    let line = format!(
        "\"{}\" -c \"{}\" && echo trusted>operator.txt && echo piped|findstr piped>piped.txt",
        probe.python.display(),
        CAPTURE.trim()
    );
    let mut hook = Hook::run("windows-cmd-transport", line);
    hook.pass_filenames = false;
    probe.assert_success(&hook, &[], &[]);
    assert_eq!(std::fs::read(probe.cwd.join("operator.txt")).unwrap(), b"trusted\r\n");
    assert_eq!(std::fs::read(probe.cwd.join("piped.txt")).unwrap(), b"piped\r\n");
}

#[test]
fn windows_cmd_transport_preserves_configured_variable_expansion() {
    let probe = Probe::new();
    let line = format!(
        "\"{}\" -c \"{}\" \"%POLY_CMD_PROBE%\"",
        probe.python.display(),
        CAPTURE.trim()
    );
    let mut hook = Hook::run("windows-cmd-transport", line);
    hook.pass_filenames = false;
    probe.assert_success(&hook, &[], &["expanded sentinel"]);
}

#[test]
fn windows_cmd_transport_forwards_only_requested_filenames_once() {
    let probe = Probe::new();
    let names = ["first file.py", "second & file.py"];
    for name in names {
        std::fs::write(probe.cwd.join(name), "pass\n").unwrap();
    }
    let files: Vec<&Path> = names.iter().map(Path::new).collect();
    let mut hook = probe.hook(&["--marker"]);
    hook.pass_filenames = true;
    probe.assert_success(&hook, &files, &["--marker", names[0], names[1]]);
    hook.pass_filenames = false;
    probe.assert_success(&hook, &files, &["--marker"]);
}

#[test]
fn windows_cmd_transport_preserves_nonzero_child_status_and_capture() {
    let probe = Probe::new();
    let line = format!("\"{}\" -c \"{}; sys.exit(17)\"", probe.python.display(), CAPTURE.trim());
    let mut hook = Hook::run("windows-cmd-transport", line);
    hook.pass_filenames = false;
    let output = probe.execute(&hook, &[]);
    assert_eq!(output.status.code(), Some(17), "{output:?}");
    probe.assert_capture(&[]);
}
