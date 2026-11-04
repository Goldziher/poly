//! The installed shim re-execs itself under `mise exec` when mise provides
//! `poly` in the repository, so a git client without mise activation (an IDE, a
//! GUI) still resolves the mise-pinned `poly` and tools.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use poly_hooks::HookType;
use poly_hooks::install::{InstallOptions, install_with};
use tempfile::TempDir;

fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake `mise` that logs every call with the auto-install setting it saw.
/// `which poly` succeeds only when `tools/poly` exists, as real mise does only
/// when a trusted config pins an installed poly. `exec -- <cmd>` runs `<cmd>`
/// with `tools` prepended to `PATH`. Like `mise.exe` on Windows, it runs `<cmd>`
/// directly, so a shim that relied on its own shebang would fail when not
/// executable.
fn fake_mise(path: &Path, tools: &Path, log: &Path) {
    script(
        path,
        &format!(
            "echo \"mise auto=${{MISE_EXEC_AUTO_INSTALL-unset}} $*\" >> '{log}'\n\
             case \"$1\" in\n\
             which) [ -x '{tools}/poly' ] && {{ echo '{tools}/poly'; exit 0; }}\n\
             echo 'mise ERROR poly is not currently active' >&2; exit 1 ;;\n\
             exec) shift; [ \"$1\" = -- ] && shift; PATH='{tools}':\"$PATH\" exec \"$@\" ;;\n\
             *) exit 2 ;;\n\
             esac\n",
            log = log.display(),
            tools = tools.display(),
        ),
    );
}

fn fake_poly(path: &Path, label: &str) {
    script(
        path,
        &format!("printf '{label} %s guard=%s stdin=%s\\n' \"$*\" \"${{POLY_HOOK_MISE-unset}}\" \"$(cat)\"\n"),
    );
}

const PINNED_POLY: &str = "[tools]\n\"github:Goldziher/poly\" = \"0.32.3\"\n";

/// A scratch repository root with `bin/mise` on `PATH`, mise's tool dir
/// `tools/`, and the fake mise's call log.
struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Self {
        let repo = Self {
            dir: TempDir::new().unwrap(),
        };
        fake_mise(&repo.bin().join("mise"), &repo.tools(), &repo.log());
        repo
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn bin(&self) -> std::path::PathBuf {
        self.root().join("bin")
    }

    fn tools(&self) -> std::path::PathBuf {
        self.root().join("tools")
    }

    fn log(&self) -> std::path::PathBuf {
        self.root().join("mise.log")
    }

    fn hooks(&self) -> std::path::PathBuf {
        self.root().join(".git/hooks")
    }

    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.bin().display())
    }

    fn mise_log(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    /// Install the `pre-push` shim and run it from the repository root, as git
    /// does: `sh .git/hooks/pre-push` with a relative `$0`.
    fn run_pre_push(&self, stdin: &str) -> Output {
        install_with(&self.hooks(), &[HookType::PrePush], InstallOptions::default()).unwrap();
        let mut child = Command::new(".git/hooks/pre-push")
            .args(["origin", "url"])
            .current_dir(self.root())
            .env_clear()
            .env("PATH", self.path())
            .env("HOME", self.root().join("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn the_shim_runs_the_mise_pinned_poly_when_poly_is_not_on_path() {
    let repo = Repo::new();
    fake_poly(&repo.tools().join("poly"), "pinned");

    let output = repo.run_pre_push("refs/heads/main abc");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "pinned hooks hook-impl --hook-type=pre-push -- origin url guard=unset stdin=refs/heads/main abc\n"
    );
    assert_eq!(
        repo.mise_log(),
        "mise auto=0 which poly\nmise auto=0 exec -- sh .git/hooks/pre-push origin url\n",
        "expected one probe, one re-exec through sh with the relative hook path, \
         auto-install off for both, and no recursion"
    );
}

#[test]
fn the_mise_pinned_poly_wins_over_a_poly_already_on_path() {
    let repo = Repo::new();
    fake_poly(&repo.tools().join("poly"), "pinned");
    fake_poly(&repo.bin().join("poly"), "homebrew");

    let output = repo.run_pre_push("");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("pinned "), "{}", stdout(&output));
}

#[test]
fn the_shim_finds_mise_in_the_installer_default_when_it_is_off_path() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    let tools = dir.path().join("tools");
    fake_mise(&home.join(".local/bin/mise"), &tools, &dir.path().join("mise.log"));
    fake_poly(&tools.join("poly"), "pinned");
    let hooks = dir.path().join("hooks");
    install_with(&hooks, &[HookType::PrePush], InstallOptions::default()).unwrap();

    let output = Command::new(hooks.join("pre-push"))
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("pinned "));
}

#[test]
fn a_mise_that_does_not_provide_poly_is_skipped_silently() {
    let repo = Repo::new();
    fake_poly(&repo.bin().join("poly"), "path");

    let output = repo.run_pre_push("refs/heads/main abc");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stderr(&output),
        "",
        "a repository without mise-pinned poly is the normal case"
    );
    assert_eq!(
        stdout(&output),
        "path hooks hook-impl --hook-type=pre-push -- origin url guard=unset stdin=refs/heads/main abc\n"
    );
    assert_eq!(
        repo.mise_log(),
        "mise auto=0 which poly\n",
        "mise must not be re-execed"
    );
}

#[test]
fn a_mise_config_without_poly_is_skipped_silently() {
    let repo = Repo::new();
    fake_poly(&repo.bin().join("poly"), "path");
    std::fs::write(repo.root().join("mise.toml"), "[tools]\nnode = \"22\"\n").unwrap();

    let output = repo.run_pre_push("");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stderr(&output), "");
    assert!(stdout(&output).starts_with("path "));
}

/// A config that pins poly while mise cannot provide it (not installed, or the
/// config is untrusted) falls back to the poly on PATH, says how to fix it, and
/// never installs anything.
#[test]
fn a_pinned_poly_that_mise_cannot_provide_falls_back_with_a_notice() {
    let repo = Repo::new();
    fake_poly(&repo.bin().join("poly"), "path");
    std::fs::write(repo.root().join("mise.toml"), PINNED_POLY).unwrap();

    let output = repo.run_pre_push("refs/heads/main abc");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stderr(&output),
        format!(
            "poly: mise.toml pins poly but `{} which poly` failed here; using poly from PATH. \
             Run `mise trust` and `mise install`, or set POLY_HOOK_MISE=1 to skip mise.\n",
            repo.bin().join("mise").display()
        )
    );
    assert_eq!(
        stdout(&output),
        "path hooks hook-impl --hook-type=pre-push -- origin url guard=unset stdin=refs/heads/main abc\n"
    );
    assert_eq!(repo.mise_log(), "mise auto=0 which poly\n");
}

#[test]
fn a_pinned_poly_that_mise_cannot_provide_and_no_poly_reports_poly_missing() {
    let repo = Repo::new();
    std::fs::write(repo.root().join("mise.toml"), PINNED_POLY).unwrap();

    let output = repo.run_pre_push("");

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("poly: mise.toml pins poly but"), "{stderr}");
    assert!(stderr.contains("the `poly` binary is not on your PATH"), "{stderr}");
}

#[test]
fn a_set_guard_skips_mise() {
    let repo = Repo::new();
    fake_poly(&repo.tools().join("poly"), "pinned");
    fake_poly(&repo.bin().join("poly"), "path");
    install_with(&repo.hooks(), &[HookType::PrePush], InstallOptions::default()).unwrap();

    let output = Command::new(".git/hooks/pre-push")
        .current_dir(repo.root())
        .env_clear()
        .env("PATH", repo.path())
        .env("HOME", repo.root())
        .env("POLY_HOOK_MISE", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).starts_with("path "));
    assert_eq!(repo.mise_log(), "", "mise must not run when POLY_HOOK_MISE is set");
}

/// `mise.exe` on Windows cannot run a shebang script, so the re-exec must go
/// through `sh`; a hook file without the exec bit stands in for that here.
#[test]
fn the_re_exec_does_not_depend_on_the_hook_being_executable() {
    let repo = Repo::new();
    fake_poly(&repo.tools().join("poly"), "pinned");
    install_with(&repo.hooks(), &[HookType::PrePush], InstallOptions::default()).unwrap();
    let hook = repo.hooks().join("pre-push");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();

    let output = Command::new("/bin/sh")
        .arg(".git/hooks/pre-push")
        .current_dir(repo.root())
        .env_clear()
        .env("PATH", repo.path())
        .env("HOME", repo.root())
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("pinned "));
}

/// A chained `.legacy` hook runs once, inside the re-exec, with the hook's
/// arguments and stdin, before the mise-pinned poly.
#[test]
fn a_kept_legacy_hook_runs_once_under_the_re_exec() {
    let repo = Repo::new();
    fake_poly(&repo.tools().join("poly"), "pinned");
    script(
        &repo.hooks().join("pre-push"),
        "printf 'legacy %s stdin=%s mise_poly=%s\\n' \"$*\" \"$(cat)\" \"$(command -v poly)\"\n",
    );

    let output = repo.run_pre_push("refs/heads/main abc");

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "legacy origin url stdin=refs/heads/main abc mise_poly={}\n\
             pinned hooks hook-impl --hook-type=pre-push -- origin url guard=unset stdin=refs/heads/main abc\n",
            repo.tools().join("poly").display()
        )
    );
    assert_eq!(
        repo.mise_log(),
        "mise auto=0 which poly\nmise auto=0 exec -- sh .git/hooks/pre-push origin url\n"
    );
}
