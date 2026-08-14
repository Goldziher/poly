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

/// A repo whose languages poly routes but holds no lint rules for, beside one
/// it does lint. `poly lint .` here reported `No issues found. (2 file(s)
/// linted)` and exited 0 with nothing in the process knowing Kotlin.
fn no_rules_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let write = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).expect("write fixture");
    write("a.kt", "fun main() {}\n");
    write("d.py", "x = 1\n");
    dir
}

/// The reported defect end to end: the Kotlin file leaves the linted count and
/// arrives in the summary with the language named.
#[test]
fn language_with_no_lint_rules_leaves_the_linted_count_and_is_named() {
    let dir = no_rules_repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "."]);
    let text = combined(&output);

    assert!(
        text.contains("1 file(s) linted, 1 skipped (no lint rules for Kotlin)"),
        "the count must exclude the language nothing lints, got:\n{text}"
    );
    assert!(
        !text.contains("2 file(s) linted"),
        "counting the Kotlin file is the defect itself, got:\n{text}"
    );
    assert!(text.contains("a.kt: no lint rules for Kotlin"), "got:\n{text}");
}

/// A language with no rules is ordinary coverage, not a failure: the default
/// verdict stays 0 and the strictness flags are how a consumer opts into caring.
#[test]
fn language_with_no_lint_rules_keeps_exit_zero_by_default() {
    let dir = no_rules_repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "."]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a language poly has no rules for must not fail a run: {}",
        combined(&output)
    );
}

/// The reporter's actual ask: a gate that fails when poly examined nothing.
/// `--deny-skips` must fire on a no-rules language and name it — this is the
/// whole reason the file has to be in the skipped set rather than merely
/// mentioned somewhere.
#[test]
fn deny_skips_fails_on_a_language_with_no_lint_rules() {
    let dir = no_rules_repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--deny-skips", "."],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("error: skipped") && text.contains("a.kt: no lint rules for Kotlin"),
        "the failure must name the file and the reason, got:\n{text}"
    );
    assert!(
        text.contains("refusing to report success for 1 skipped file(s)"),
        "got:\n{text}"
    );
}

/// `--max-skips` budgets the same set, so a repo with a known set of unlintable
/// languages can hold the number steady instead of letting it grow unnoticed.
#[test]
fn max_skips_budgets_languages_with_no_lint_rules() {
    let dir = no_rules_repo();

    let at_limit = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--max-skips", "1", "."],
    );
    assert_eq!(at_limit.status.code(), Some(0), "got:\n{}", combined(&at_limit));

    std::fs::write(dir.path().join("b.swift"), "let x = 1\n").expect("write b.swift");
    let over_limit = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--max-skips", "1", "."],
    );
    let text = combined(&over_limit);
    assert_eq!(over_limit.status.code(), Some(2), "got:\n{text}");
    assert!(text.contains("no lint rules for Swift"), "got:\n{text}");
}

/// The machine-readable path: the reason travels in the JSON document, so a
/// consumer can assert on the uncovered set structurally.
#[test]
fn json_carries_the_no_lint_rules_reason() {
    let dir = no_rules_repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--format", "json", "."],
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout stays valid JSON");
    let entries = value.as_array().expect("top level stays an array");
    let entry = entries
        .iter()
        .find(|entry| entry["path"].as_str().is_some_and(|p| p.ends_with("a.kt")))
        .unwrap_or_else(|| panic!("the uncovered file must be in the document: {stdout}"));
    assert_eq!(entry["skipped"].as_str(), Some("no lint rules for Kotlin"));
}

/// A walked file poly cannot identify at all is counted and named, but it is not
/// a skip: itemising every snapshot, lockfile and image a walk passes over would
/// fire `--deny-skips` in every repository and bury the skips that mean
/// something.
#[test]
fn unknown_extension_in_a_walk_is_counted_not_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("c.xyz"), "zzz\n").expect("write c.xyz");
    std::fs::write(dir.path().join("d.py"), "x = 1\n").expect("write d.py");

    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--deny-skips", "."],
    );
    let text = combined(&output);

    assert_eq!(
        output.status.code(),
        Some(0),
        "an unreadable file type in a walk is not a skip: {text}"
    );
    assert!(
        text.contains("1 file(s) linted, 1 file(s) of unrecognized type not checked"),
        "got:\n{text}"
    );
    assert!(
        text.contains("were not identified as any language and no engine saw them"),
        "got:\n{text}"
    );
    assert!(text.contains("c.xyz"), "the file must be named, got:\n{text}");
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
        text.contains("matched exclusions (use --include-excluded to check them)"),
        "the exclusion must still be explained, got:\n{text}"
    );
    assert!(
        !text.contains("no matching engine"),
        "an excluded path is not an unmatched one, got:\n{text}"
    );
}

#[test]
fn explicit_paths_honor_excludes_unless_the_caller_overrides_them() {
    let dir = repo();
    std::fs::write(dir.path().join("poly.toml"), "[discovery]\nexclude = [\"a.py\"]\n").expect("write poly.toml");
    let original = "x   =    1\n";
    std::fs::write(dir.path().join("a.py"), original).expect("write excluded file");

    let excluded = poly(dir.path(), &["fmt", "--fix", "--no-cache", "a.py"]);
    assert!(
        combined(&excluded).contains("matched exclusions (use --include-excluded to check them)"),
        "explicit paths must honor excludes by default"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.py")).expect("read excluded file"),
        original
    );

    let included = poly(
        dir.path(),
        &["fmt", "--fix", "--no-cache", "--include-excluded", "a.py"],
    );
    assert_eq!(included.status.code(), Some(1), "got:\n{}", combined(&included));
    assert_ne!(
        std::fs::read_to_string(dir.path().join("a.py")).expect("read included file"),
        original
    );
}

#[test]
fn bare_jinja_template_is_preserved_and_reports_how_to_opt_in() {
    let dir = repo();
    let template = dir.path().join("service.jinja");
    let content = "{% if docs %}\n/// <summary>\n/// {{ docs }}\n/// </summary>\n{% endif %}\n";
    std::fs::write(&template, content).expect("write template");

    let output = poly(dir.path(), &["fmt", "--fix", "--no-cache", "service.jinja"]);
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(0), "got:\n{text}");
    assert!(text.contains("ambiguous template target"), "got:\n{text}");
    assert!(
        text.contains("add .html or .xml before the template extension"),
        "got:\n{text}"
    );
    assert_eq!(std::fs::read_to_string(template).expect("read template"), content);
}

fn multiroot_repo() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("poly.toml"),
        "[discovery]\nexclude = [\"docs/snippets/**\"]\n",
    )
    .expect("write config");
    let excluded = dir.path().join("docs/snippets/bad.py");
    std::fs::create_dir_all(excluded.parent().expect("excluded parent")).expect("create excluded tree");
    std::fs::write(&excluded, "x   =    1\n").expect("write excluded file");
    std::fs::write(dir.path().join("included.py"), "y   =    2\n").expect("write included file");
    (dir, excluded)
}

#[test]
fn excluded_directory_stays_excluded_in_every_multiroot_order() {
    for paths in [["docs/snippets", "included.py"], ["included.py", "docs/snippets"]] {
        let (dir, excluded) = multiroot_repo();
        let output = poly(dir.path(), &["fmt", "--fix", "--no-cache", paths[0], paths[1]]);
        let text = combined(&output);

        assert_eq!(output.status.code(), Some(1), "order {paths:?} failed:\n{text}");
        assert_eq!(
            std::fs::read_to_string(excluded).expect("read excluded"),
            "x   =    1\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("included.py")).unwrap(),
            "y = 2\n"
        );
        assert!(
            text.contains("matched exclusions (use --include-excluded to check them)"),
            "order {paths:?} was not explained:\n{text}"
        );
    }
}

#[test]
fn unanchored_rule_excludes_a_named_root_it_matches() {
    let (dir, excluded) = multiroot_repo();
    std::fs::write(
        dir.path().join("poly.toml"),
        "[discovery]\nexclude = [\"**/snippets/**\"]\n",
    )
    .unwrap();

    let output = poly(
        dir.path(),
        &["fmt", "--fix", "--no-cache", "included.py", "docs/snippets"],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(1), "got:\n{text}");
    assert_eq!(std::fs::read_to_string(excluded).unwrap(), "x   =    1\n");
    assert!(text.contains("matched exclusions (use --include-excluded to check them)"));
}

#[test]
fn include_excluded_overrides_an_explicit_excluded_directory() {
    let (dir, excluded) = multiroot_repo();
    let output = poly(
        dir.path(),
        &[
            "fmt",
            "--fix",
            "--no-cache",
            "--include-excluded",
            "docs/snippets",
            "included.py",
        ],
    );

    assert_eq!(output.status.code(), Some(1), "got:\n{}", combined(&output));
    assert_eq!(std::fs::read_to_string(excluded).unwrap(), "x = 1\n");
}

#[test]
fn include_excluded_keeps_descendant_exclusions_active() {
    let (dir, excluded) = multiroot_repo();
    let private = dir.path().join("docs/snippets/private/secret.py");
    std::fs::create_dir_all(private.parent().unwrap()).unwrap();
    std::fs::write(&private, "secret   =    3\n").unwrap();
    std::fs::write(
        dir.path().join("poly.toml"),
        "[discovery]\nexclude = [\"docs/snippets/**\", \"**/private/**\"]\n",
    )
    .unwrap();

    let output = poly(
        dir.path(),
        &["fmt", "--fix", "--no-cache", "--include-excluded", "docs/snippets"],
    );

    assert_eq!(output.status.code(), Some(1), "got:\n{}", combined(&output));
    assert_eq!(std::fs::read_to_string(excluded).unwrap(), "x = 1\n");
    assert_eq!(std::fs::read_to_string(private).unwrap(), "secret   =    3\n");
}
