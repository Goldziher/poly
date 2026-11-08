//! `poly commit` end to end: the `[commit]` table of `poly.toml`, flag overrides, exit codes
//! and the rewrite-in-place flow, run against a tempdir so no ambient config is discovered.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::tempdir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn poly_commit(dir: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("commit")
        .args(args)
        .current_dir(dir)
        .env("POLY_OFFLINE", "1")
        // cargo puts a second `poly` under target/*/deps on PATH on Windows, and the shadow
        // warning it triggers would land in the stderr these tests assert on.
        .env("POLY_NO_SHADOW_WARN", "1")
        .output()
        .expect("run poly commit")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn conventional_message_passes_silently() {
    let dir = tempdir().unwrap();
    let output = poly_commit(dir.path(), &["--message", "feat: add login"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
}

#[test]
fn non_conventional_message_fails_with_gitfluff_prefix() {
    let dir = tempdir().unwrap();
    let output = poly_commit(dir.path(), &["--message", "nope: not a type"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("gitfluff: error: "), "{}", stderr(&output));
}

#[test]
fn poly_toml_commit_rules_apply() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("poly.toml"),
        "[commit.rules]\nno_emojis = true\n[[commit.rules.excludes]]\npattern = \"^feat: wip\"\nmessage = \"no wip\"\n",
    )
    .unwrap();

    let wip = poly_commit(dir.path(), &["--message", "feat: wip thing"]);
    assert_eq!(wip.status.code(), Some(1));
    assert!(stderr(&wip).contains("no wip"), "{}", stderr(&wip));

    let emoji = poly_commit(dir.path(), &["--message", "feat: add \u{1F680} launch"]);
    assert_eq!(emoji.status.code(), Some(1), "{}", stderr(&emoji));
}

#[test]
fn write_strips_ai_trailer_from_the_message_file() {
    let dir = tempdir().unwrap();
    let msg = dir.path().join("MSG");
    fs::write(
        &msg,
        "feat: add login\n\nbody\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n",
    )
    .unwrap();

    let output = poly_commit(dir.path(), &["--write", "MSG"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(&msg).unwrap(), "feat: add login\n\nbody\n");
}

#[test]
fn missing_message_source_is_an_error() {
    let dir = tempdir().unwrap();
    let output = poly_commit(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no commit message source"));
}

#[test]
fn exclude_splits_the_custom_message_on_arrow() {
    let dir = tempdir().unwrap();
    let output = poly_commit(
        dir.path(),
        &["--exclude", "WIP->Not allowed", "--message", "feat: WIP thing"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("Not allowed"), "{}", stderr(&output));
}

#[test]
fn exclude_colon_belongs_to_the_regex() {
    let dir = tempdir().unwrap();
    let output = poly_commit(
        dir.path(),
        &["--exclude", "WIP:Not allowed", "--message", "feat: WIP thing"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

fn code(output: &Output) -> Option<i32> {
    output.status.code()
}

fn message_file(dir: &Path, content: &str) -> std::path::PathBuf {
    let path = dir.join("msg.txt");
    fs::write(&path, content).unwrap();
    path
}

fn lint_file(dir: &Path, content: &str, args: &[&str]) -> Output {
    let path = message_file(dir, content);
    let mut all: Vec<&str> = args.to_vec();
    let path_str = path.to_str().unwrap();
    all.extend(["--from-file", path_str]);
    poly_commit(dir, &all)
}

fn assert_fails_with(output: &Output, needle: &str) {
    assert_eq!(code(output), Some(1), "{}", stderr(output));
    assert!(
        stderr(output).contains(needle),
        "missing `{needle}` in: {}",
        stderr(output)
    );
}

fn with_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(POLY)
        .arg("commit")
        .args(args)
        .current_dir(dir)
        .env("POLY_OFFLINE", "1")
        .env("POLY_NO_SHADOW_WARN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

const AI_TRAILER: &str = "feat: add login\n\nbody\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n";

#[test]
fn exit_nonzero_on_rewrite_flag_fails_after_cleanup() {
    let dir = tempdir().unwrap();
    let output = lint_file(dir.path(), AI_TRAILER, &["--write", "--exit-nonzero-on-rewrite"]);
    assert_fails_with(&output, "rewritten");
    assert_eq!(
        fs::read_to_string(dir.path().join("msg.txt")).unwrap(),
        "feat: add login\n\nbody\n"
    );
}

#[test]
fn exit_nonzero_on_rewrite_from_config() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("poly.toml"),
        "[commit]\nwrite = true\n[commit.rules]\nexit_nonzero_on_rewrite = true\n",
    )
    .unwrap();
    let output = lint_file(dir.path(), AI_TRAILER, &[]);
    assert_fails_with(&output, "rewritten");
}

#[test]
fn stdin_message_is_written_back_to_stdout() {
    let dir = tempdir().unwrap();
    let output = with_stdin(dir.path(), &["--stdin", "--write"], AI_TRAILER);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "feat: add login\n\nbody\n");
}

#[test]
fn commit_file_is_skipped_only_when_it_is_the_pending_message() {
    let dir = tempdir().unwrap();
    let git_dir = dir.path().join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("MERGE_HEAD"), "deadbeef").unwrap();
    fs::write(git_dir.join("COMMIT_EDITMSG"), "Merge branch 'x'\n").unwrap();
    let pending = git_dir.join("COMMIT_EDITMSG");

    let skipped = poly_commit(dir.path(), &[pending.to_str().unwrap()]);
    assert_eq!(code(&skipped), Some(0), "{}", stderr(&skipped));

    let other = message_file(dir.path(), "not conventional\n");
    let linted = poly_commit(dir.path(), &[other.to_str().unwrap()]);
    assert_eq!(code(&linted), Some(1), "{}", stderr(&linted));
}

#[test]
fn generated_subjects_are_not_linted() {
    let dir = tempdir().unwrap();
    for subject in ["fixup! whatever", "Revert \"feat: add login\"", "squash! x", "amend! y"] {
        let output = poly_commit(dir.path(), &["--message", subject]);
        assert_eq!(code(&output), Some(0), "{subject}: {}", stderr(&output));
    }
}

#[test]
fn explicit_config_path_is_used() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("custom.toml");
    fs::write(&config, "preset = \"simple\"\n").unwrap();
    let args = ["--config", config.to_str().unwrap(), "--message"];

    let ok = poly_commit(dir.path(), &[args[0], args[1], args[2], "Plain subject"]);
    assert_eq!(code(&ok), Some(0), "{}", stderr(&ok));
    let multi = poly_commit(dir.path(), &[args[0], args[1], args[2], "Plain subject\n\nbody"]);
    assert_fails_with(&multi, "single line");
}

#[test]
fn gitfluff_toml_takes_precedence_over_poly_toml() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("poly.toml"), "[commit]\npreset = \"conventional\"\n").unwrap();
    fs::write(dir.path().join(".gitfluff.toml"), "preset = \"simple\"\n").unwrap();
    let output = poly_commit(dir.path(), &["--message", "Plain subject"]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
}

#[test]
fn gitfluff_toml_rejects_unknown_keys() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(".gitfluff.toml"), "[rules]\nno_emoji = true\n").unwrap();
    let output = poly_commit(dir.path(), &["--message", "feat: x"]);
    assert_ne!(code(&output), Some(0));
    let err = stderr(&output);
    assert!(err.contains("invalid config") && err.contains("no_emoji"), "{err}");
}

#[test]
fn cleanup_flag_reports_then_applies() {
    let dir = tempdir().unwrap();
    let report = lint_file(
        dir.path(),
        "TEMP: fix bug\n\nDetails\n",
        &["--cleanup-pattern", "^TEMP: ", "--cleanup-replacement", "feat: "],
    );
    assert_fails_with(&report, "cleanup available");

    let apply = lint_file(
        dir.path(),
        "TEMP: fix bug\n\nDetails\n",
        &[
            "--cleanup-pattern",
            "^TEMP: ",
            "--cleanup-replacement",
            "feat: ",
            "--write",
        ],
    );
    assert_eq!(code(&apply), Some(0), "{}", stderr(&apply));
    assert!(stderr(&apply).contains("applied cleanup"));
    assert!(
        fs::read_to_string(dir.path().join("msg.txt"))
            .unwrap()
            .starts_with("feat: fix bug")
    );

    let arrow = lint_file(dir.path(), "wip: thing\n", &["--cleanup", "^wip->feat", "--write"]);
    assert_eq!(code(&arrow), Some(0), "{}", stderr(&arrow));
    assert_eq!(fs::read_to_string(dir.path().join("msg.txt")).unwrap(), "feat: thing\n");
}

#[test]
fn msg_pattern_and_description_replace_the_spec_check() {
    let dir = tempdir().unwrap();
    let ok = lint_file(
        dir.path(),
        "JIRA-123 Fix login flow\n",
        &["--msg-pattern", "^JIRA-[0-9]+\\s.+$"],
    );
    assert_eq!(code(&ok), Some(0), "{}", stderr(&ok));
    let bad = lint_file(
        dir.path(),
        "update docs\n",
        &[
            "--msg-pattern",
            "^JIRA-[0-9]+: .+$",
            "--msg-pattern-description",
            "Ticket prefix required",
        ],
    );
    assert_fails_with(&bad, "Ticket prefix required");
}

#[test]
fn title_prefix_and_suffix_with_separators() {
    let dir = tempdir().unwrap();
    let prefix = ["--title-prefix", "PROJ-[0-9]+"];
    assert_eq!(
        code(&lint_file(dir.path(), "PROJ-123 * feat: add login\n", &prefix)),
        Some(0)
    );
    assert_fails_with(&lint_file(dir.path(), "feat: add login\n", &prefix), "title must start");

    let sep = ["--title-prefix", "PROJ-[0-9]+", "--title-prefix-separator", "::"];
    assert_eq!(
        code(&lint_file(dir.path(), "PROJ-123::feat: add login\n", &sep)),
        Some(0)
    );
    assert_fails_with(
        &lint_file(dir.path(), "PROJ-123 feat: add login\n", &sep),
        "title must start",
    );

    let suffix = ["--title-suffix", "PROJ-[0-9]+", "--title-suffix-separator", " :: "];
    assert_eq!(
        code(&lint_file(dir.path(), "feat: add login :: PROJ-123\n", &suffix)),
        Some(0)
    );
    assert_fails_with(
        &lint_file(dir.path(), "feat: add login PROJ-123\n", &suffix),
        "title must end",
    );
}

#[test]
fn presets_enforce_their_layout() {
    let dir = tempdir().unwrap();
    assert_eq!(
        code(&lint_file(
            dir.path(),
            "Fix login button alignment\n",
            &["--preset", "simple"]
        )),
        Some(0)
    );
    assert_fails_with(
        &lint_file(dir.path(), "fix: add body\n\nextra details\n", &["--preset", "simple"]),
        "single line",
    );
    assert_fails_with(
        &lint_file(dir.path(), "feat: add login\n", &["--preset", "conventional-body"]),
        "must include a body",
    );
    let ok = lint_file(
        dir.path(),
        "feat: add login\n\nExplain rationale\n",
        &["--preset", "conventional-body"],
    );
    assert_eq!(code(&ok), Some(0), "{}", stderr(&ok));
}

#[test]
fn emoji_and_ascii_flags() {
    let dir = tempdir().unwrap();
    assert_eq!(
        code(&lint_file(dir.path(), "feat: add \u{1F680} launch\n", &[])),
        Some(0)
    );
    assert_fails_with(
        &lint_file(dir.path(), "feat: add \u{1F680} launch\n", &["--no-emojis"]),
        "emoji",
    );
    assert_eq!(
        code(&lint_file(dir.path(), "feat: add r\u{e9}sum\u{e9}\n", &[])),
        Some(0)
    );
    assert_fails_with(
        &lint_file(dir.path(), "feat: add r\u{e9}sum\u{e9}\n", &["--ascii-only"]),
        "ASCII",
    );
}

#[test]
fn no_ai_cleanup_keeps_the_trailer() {
    let dir = tempdir().unwrap();
    let output = lint_file(dir.path(), AI_TRAILER, &["--write", "--no-ai-cleanup"]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(dir.path().join("msg.txt")).unwrap(), AI_TRAILER);
}

#[test]
fn config_write_is_ignored_for_a_literal_message_with_a_warning() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("poly.toml"), "[commit]\nwrite = true\n").unwrap();
    let output = poly_commit(dir.path(), &["--message", "bad subject"]);
    assert_fails_with(&output, "`write` is ignored for `--message`");
    assert!(output.stdout.is_empty());
}

#[test]
fn extends_base_commit_rules_are_resolved() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("base.toml"), "[commit.rules]\nno_emojis = true\n").unwrap();
    fs::write(dir.path().join("poly.toml"), "extends = [\"base.toml\"]\n").unwrap();
    let output = poly_commit(dir.path(), &["--message", "feat: add \u{1F680} launch"]);
    assert_eq!(code(&output), Some(1), "{}", stderr(&output));
}

#[test]
fn custom_pattern_notice_names_the_commit_rules_table() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("poly.toml"),
        "[commit.rules.message]\npattern = \"^JIRA-[0-9]+ .+$\"\n",
    )
    .unwrap();
    let output = poly_commit(dir.path(), &["--message", "feat: nope"]);
    assert_fails_with(&output, "`[commit.rules.message]` in ");

    let clean = poly_commit(dir.path(), &["--message", "JIRA-1 nope"]);
    assert_eq!(code(&clean), Some(0), "{}", stderr(&clean));
    assert!(clean.stderr.is_empty());
}

#[test]
fn a_header_without_type_and_subject_says_what_shape_it_needs() {
    let dir = tempdir().unwrap();
    for message in ["added stuff", "feat:no space"] {
        let output = poly_commit(dir.path(), &["--message", message]);
        assert_eq!(output.status.code(), Some(1), "{message}");
        let text = stderr(&output);
        assert!(
            text.contains("hint: the header must be `type(scope): subject`") && text.contains("space after the colon"),
            "{message}: {text}"
        );
    }
}

#[test]
fn a_plain_ai_banner_in_the_body_fails_like_the_trailer_form() {
    let dir = tempdir().unwrap();
    let output = poly_commit(
        dir.path(),
        &["--message", "feat: add login\n\nGenerated with Claude Code"],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let opted_out = poly_commit(
        dir.path(),
        &[
            "--no-ai-cleanup",
            "--message",
            "feat: add login\n\nGenerated with Claude Code",
        ],
    );
    assert_eq!(opted_out.status.code(), Some(0), "{}", stderr(&opted_out));
}
