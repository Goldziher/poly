//! End-to-end coverage for work poly did **not** do: explicitly named paths no
//! engine matches, and files a backend declined.
//!
//! `poly lint packages/csharp/App.csproj` used to print `No issues found. (0
//! file(s) linted)` and exit 0 — nothing was examined, and nothing said so. The
//! mixed case is what made it dangerous in practice: five explicit paths in,
//! `No issues found. (4 file(s) linted)` out, with no indication of which path
//! was dropped or why. The reporter found it by bisecting one path at a time.
//!
//! Exit 0 for an unhandled extension is defensible; silence is not. The strict
//! mode (`--deny-skips` / `--max-skips`) is the opt-in for consumers who want a
//! skipped file to fail their gate, and it names every file it fails on.
//!
//! These shell out to the built binary so they cover arg parsing → runner →
//! report → exit code, which is the layer the guarantee lives at.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// A `.csproj` is XML-ish but no poly engine claims the extension, so it is the
/// exact shape the reporter hit.
const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n  </PropertyGroup>\n</Project>\n";

/// A repo holding one unmatched path plus four files poly does handle — the
/// reported "five in, four linted" shape.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let write = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).expect("write fixture");
    write("App.csproj", CSPROJ);
    write("a.py", "x = 1\n");
    write("b.py", "y = 2\n");
    write("c.json", "{ \"a\": 1 }\n");
    write("d.md", "# Title\n");
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run poly")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The reported defect: the only named path matches no engine, so nothing was
/// examined — but the summary read as a verified pass.
#[test]
fn unmatched_explicit_path_is_named_in_the_summary() {
    let dir = repo();
    for subcommand in [
        vec!["lint", "--no-workspace", "App.csproj"],
        vec!["fmt", "--check", "App.csproj"],
    ] {
        let output = poly(dir.path(), &subcommand);
        let text = combined(&output);

        assert!(
            text.contains("no matching engine"),
            "{subcommand:?} must say the path matched no engine, got:\n{text}"
        );
        assert!(
            text.contains("App.csproj"),
            "{subcommand:?} must name the dropped path, got:\n{text}"
        );
    }
}

/// The reporter explicitly did not ask for a non-zero exit here: an unhandled
/// extension is defensible, silence is the defect. Locking the exit code down
/// keeps a later "while we're here" change from breaking their gate.
#[test]
fn unmatched_explicit_path_keeps_exit_zero() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "App.csproj"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "an unhandled extension must not fail the run: {}",
        combined(&output)
    );
}

/// Five paths in, four linted — the shape that took bisecting one path at a
/// time to diagnose.
#[test]
fn five_paths_four_linted_names_the_fifth() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "App.csproj", "a.py", "b.py", "c.json", "d.md"],
    );
    let text = combined(&output);

    assert!(text.contains("4 file(s) linted"), "got:\n{text}");
    assert!(
        text.contains("App.csproj"),
        "the dropped path must be named, not left to bisection, got:\n{text}"
    );
}

/// A directory walk naturally contains files no engine handles. Emitting a note
/// for each would make every run noisy, so the accounting is for *explicitly*
/// named paths only.
#[test]
fn directory_walk_stays_quiet_about_unmatched_files() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "."]);
    let text = combined(&output);

    assert!(
        !text.contains("no matching engine"),
        "a directory walk must not narrate unmatched files, got:\n{text}"
    );
}

/// `--deny-skips` is the strict mode: any skipped file fails the run. It must
/// name what it failed on — a gate that fails without saying which file is the
/// same defect in a different costume.
#[test]
fn deny_skips_fails_and_names_the_skipped_file() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--deny-skips", "App.csproj"]);
    let text = combined(&output);

    assert_eq!(
        output.status.code(),
        Some(2),
        "--deny-skips must fail the run, got:\n{text}"
    );
    assert!(text.contains("App.csproj"), "got:\n{text}");
}

/// A hash-stamped generated file is skipped by `poly fmt` (reformatting it
/// invalidates the stamp). `--deny-skips` turns that silent skip into a failure
/// that names the file and the reason.
#[test]
fn deny_skips_covers_engine_declined_files_in_fmt() {
    let dir = repo();
    std::fs::write(dir.path().join("gen.py"), "# @checksum: abc\nx  =  1\n").expect("write gen.py");

    let output = poly(dir.path(), &["fmt", "--check", "--no-cache", "--deny-skips", "gen.py"]);
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(text.contains("gen.py"), "got:\n{text}");
    assert!(
        text.contains("hash-stamped generated file"),
        "the reason must travel with the name, got:\n{text}"
    );
}

/// Without the flag a skip is still reported, but the run keeps its previous
/// exit code — the strict mode is opt-in.
#[test]
fn skips_alone_do_not_change_the_exit_code() {
    let dir = repo();
    std::fs::write(dir.path().join("gen.py"), "# @checksum: abc\nx  =  1\n").expect("write gen.py");

    let output = poly(dir.path(), &["fmt", "--check", "--no-cache", "gen.py"]);
    assert_eq!(output.status.code(), Some(0), "got:\n{}", combined(&output));
}

/// `--max-skips=N` is the budgeted form: at or under the budget the run passes,
/// over it the run fails.
#[test]
fn max_skips_budget_passes_at_the_limit_and_fails_above_it() {
    let dir = repo();

    let at_limit = poly(
        dir.path(),
        &["lint", "--no-workspace", "--max-skips", "1", "App.csproj"],
    );
    assert_eq!(
        at_limit.status.code(),
        Some(0),
        "one skip is within a budget of one: {}",
        combined(&at_limit)
    );

    let over_limit = poly(
        dir.path(),
        &["lint", "--no-workspace", "--max-skips", "0", "App.csproj"],
    );
    assert_eq!(
        over_limit.status.code(),
        Some(2),
        "a budget of zero is exceeded by one skip: {}",
        combined(&over_limit)
    );
}

/// The reporter's real ask: assert on the *set* of skipped files, structurally,
/// instead of reconstructing it from a heuristic and scraping the human summary.
#[test]
fn json_output_carries_skipped_paths_structurally() {
    let dir = repo();
    std::fs::write(dir.path().join("gen.py"), "# @checksum: abc\nx  =  1\n").expect("write gen.py");

    for subcommand in [
        vec!["lint", "--no-workspace", "--format", "json", "App.csproj"],
        vec!["fmt", "--check", "--no-cache", "--format", "json", "App.csproj"],
    ] {
        let output = poly(dir.path(), &subcommand);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let value: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("{subcommand:?} stdout must be JSON ({e}): {stdout}"));
        let entries = value.as_array().expect("top level stays an array");
        let skipped = entries
            .iter()
            .find(|entry| entry["path"].as_str().is_some_and(|p| p.ends_with("App.csproj")))
            .unwrap_or_else(|| panic!("{subcommand:?} must carry the unmatched path: {stdout}"));
        assert!(
            skipped["skipped"].is_string(),
            "{subcommand:?} must carry a machine-readable skip reason: {stdout}"
        );
    }
}

/// `--verbose` names every declined file, so a consumer can see the set without
/// switching to JSON.
#[test]
fn verbose_names_engine_declined_files() {
    let dir = repo();
    std::fs::write(dir.path().join("gen.py"), "# @checksum: abc\nx  =  1\n").expect("write gen.py");

    let output = poly(dir.path(), &["fmt", "--check", "--no-cache", "--verbose", "."]);
    let text = combined(&output);
    assert!(text.contains("gen.py"), "got:\n{text}");
}

/// A clean run over handled files must gain no new output at all — the common
/// path stays quiet.
#[test]
fn clean_run_gains_no_skip_narration() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--check", "--no-cache", "a.py", "b.py"]);
    let text = combined(&output);

    assert!(!text.contains("skipped"), "got:\n{text}");
    assert!(!text.contains("no matching engine"), "got:\n{text}");
}

/// A path the exclude set dropped is already explained by the discovery note.
/// Reporting it a second time as "no matching engine" would be a wrong reason
/// for a right outcome, so the two are told apart.
#[test]
fn force_excluded_path_is_not_reported_as_unmatched() {
    let dir = repo();
    std::fs::write(dir.path().join("poly.toml"), "[discovery]\nexclude = [\"a.py\"]\n").expect("write poly.toml");

    let output = poly(dir.path(), &["fmt", "--check", "--no-cache", "--force-exclude", "a.py"]);
    let text = combined(&output);

    assert!(
        text.contains("dropped by --force-exclude"),
        "the exclusion must still be explained, got:\n{text}"
    );
    assert!(
        !text.contains("no matching engine"),
        "an excluded path is not an unmatched one, got:\n{text}"
    );
}
