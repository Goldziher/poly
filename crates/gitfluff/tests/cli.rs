use assert_cmd::{Command, cargo};
use predicates::prelude::*;
use std::env;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn write_message(path: &Path, content: impl AsRef<[u8]>) {
    fs::write(path, content).expect("write message");
}

/// Build a `gitfluff` command rooted at `dir` (a per-test tempdir) so the suite
/// never discovers an ambient `poly.toml [commit]` or `.gitfluff.toml` from the
/// repository it happens to run inside — the tests stay hermetic regardless of
/// the host repo's commit config.
fn gitfluff(dir: &Path) -> Command {
    let mut command = cargo::cargo_bin_cmd!("gitfluff");
    command.current_dir(dir);
    command
}

#[test]
fn lint_passes_for_conventional_commit() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("message.txt");
    write_message(&msg_path, "feat: add login\n");

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn lint_accepts_positional_commit_file() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("message.txt");
    write_message(&msg_path, "feat: add login\n");

    gitfluff(dir.path())
        .arg("lint")
        .arg(&msg_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty());
}

#[test]
fn lint_fails_for_ai_attribution_without_write() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(
        &msg_path,
        "feat: add login\n\n🤖 Generated with Claude\n- Claude\nCo-Authored-By: Claude Sonnet 4.5\n<noreply@anthropic.com>\n",
    );

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "Remove AI co-author attribution lines",
        ))
        .stderr(predicate::str::contains("Remove AI generation notices"));
}

#[test]
fn simple_preset_enforces_single_line() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");

    write_message(&msg_path, "Fix login button alignment\n");
    gitfluff(dir.path())
        .args(["lint", "--preset", "simple", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "fix: add body\n\nextra details\n");
    gitfluff(dir.path())
        .args(["lint", "--preset", "simple", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("single line"));
}

#[test]
fn conventional_body_preset_requires_body() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");

    write_message(&msg_path, "feat: add login\n");
    gitfluff(dir.path())
        .args(["lint", "--preset", "conventional-body", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("must include a body"));

    write_message(&msg_path, "feat: add login\n\nExplain rationale\n");
    gitfluff(dir.path())
        .args(["lint", "--preset", "conventional-body", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();
}

#[test]
fn lint_applies_cleanup_with_write_flag() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(
        &msg_path,
        "feat: add login\n\n🤖 Generated with Claude\n- Claude\nCo-Authored-By: Claude Sonnet 4.5\n<noreply@anthropic.com>\n",
    );

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .arg("--write")
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "Remove AI co-author attribution lines",
        ))
        .stderr(predicate::str::contains("Remove AI generation notices"))
        .stderr(predicate::str::contains("applied cleanup"))
        .stderr(predicate::str::contains(
            "Remove Claude Code attribution block",
        ));

    let rewritten = fs::read_to_string(&msg_path).unwrap();
    assert_eq!(rewritten.trim_end(), "feat: add login");
}

#[test]
fn lint_autofixes_conventional_layout_with_write_flag() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(
        &msg_path,
        "feat: add api\n- Note: handle edge cases  \nRefs: 123\n",
    );

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .arg("--write")
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("applied cleanup"))
        .stderr(predicate::str::contains("Insert blank line before body"))
        .stderr(predicate::str::contains("Insert blank line before footers"))
        .stderr(predicate::str::contains("Trim trailing whitespace"));

    let rewritten = fs::read_to_string(&msg_path).unwrap();
    assert_eq!(
        rewritten,
        "feat: add api\n\n- Note: handle edge cases\n\nRefs: 123\n"
    );
}

#[test]
fn commitlint_conventional_parity_suite() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");

    let run = |message: &str| {
        write_message(&msg_path, format!("{message}\n"));
        gitfluff(dir.path())
            .arg("lint")
            .arg("--from-file")
            .arg(&msg_path)
            .assert()
    };

    run("foo: some message")
        .failure()
        .stderr(predicate::str::contains(
            "type must be one of [build, chore, ci, docs, feat, fix, perf, refactor, revert, style, test]",
        ));

    run("FIX: some message")
        .failure()
        .stderr(predicate::str::contains("type must be lower-case"))
        .stderr(predicate::str::contains(
            "type must be one of [build, chore, ci, docs, feat, fix, perf, refactor, revert, style, test]",
        ));

    run(": some message")
        .failure()
        .stderr(predicate::str::contains("type may not be empty"));

    for invalid in [
        "fix(scope): Some message",
        "fix(scope): Some Message",
        "fix(scope): SomeMessage",
        "fix(scope): SOMEMESSAGE",
    ] {
        run(invalid).failure().stderr(predicate::str::contains(
            "subject must not be sentence-case, start-case, pascal-case, upper-case",
        ));
    }

    run("fix:")
        .failure()
        .stderr(predicate::str::contains("subject may not be empty"))
        .stderr(predicate::str::contains("type may not be empty"));

    run("fix: some message.")
        .failure()
        .stderr(predicate::str::contains(
            "subject may not end with full stop",
        ));

    run("fix: some message that is way too long and breaks the line max-length by several characters since the max is 100")
        .failure()
        .stderr(predicate::str::contains(
            "title line must not be longer than 100 characters",
        ));

    run("fix: some message\n\nbody\nBREAKING CHANGE: It will be significant")
        .success()
        .stderr(predicate::str::contains(
            "footer must have leading blank line",
        ));

    run("fix: some message\n\nbody\n\nBREAKING CHANGE: footer with multiple lines\nhas a message that is way too long and will break the line rule \"line-max-length\" by several characters")
        .failure()
        .stderr(predicate::str::contains(
            "footer's lines must not be longer than 100 characters",
        ));

    run("fix: some message\nbody")
        .success()
        .stderr(predicate::str::contains(
            "body must have leading blank line",
        ));

    run("fix: some message\n\nbody with multiple lines\nhas a message that is way too long and will break the line rule \"line-max-length\" by several characters")
        .failure()
        .stderr(predicate::str::contains(
            "body's lines must not be longer than 100 characters",
        ));

    for valid in [
        "fix: some message",
        "fix(scope): some message",
        "fix(scope): some Message",
        "fix(scope): some message\n\nBREAKING CHANGE: it will be significant!",
        "fix(scope): some message\n\nbody",
        "fix(scope)!: some message\n\nbody",
    ] {
        run(valid).success().stderr(predicate::str::is_empty());
    }
}

#[test]
fn lint_can_fail_after_rewrite_when_configured() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(
        &msg_path,
        "feat: add login\n\n🤖 Generated with Claude\n- Claude\nCo-Authored-By: Claude Sonnet 4.5\n<noreply@anthropic.com>\n",
    );

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"
write = true

[rules]
exit_nonzero_on_rewrite = true
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("rewritten"));

    let rewritten = fs::read_to_string(&msg_path).unwrap();
    assert_eq!(rewritten.trim_end(), "feat: add login");
}

#[test]
fn lint_enforces_require_body_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add login\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
require_body = true
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("must include a body"));
}

#[test]
fn lint_enforces_title_prefix_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-123 * feat: add login\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
title_prefix = "PROJ-[0-9]+"
title_prefix_separator = " * "
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "feat: add login\n");
    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must start"));
}

#[test]
fn lint_enforces_title_suffix_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add login (PROJ-123)\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
title_suffix = "\\(PROJ-[0-9]+\\)"
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "feat: add login\n");
    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must end"));
}

#[test]
fn lint_accepts_title_prefix_default_separator_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-123 * feat: add login\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
title_prefix = "PROJ-[0-9]+"
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "PROJ-123 feat: add login\n");
    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must start"));
}

#[test]
fn lint_accepts_title_prefix_custom_separator_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-123::feat: add login\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
title_prefix = "PROJ-[0-9]+"
title_prefix_separator = "::"
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "PROJ-123 * feat: add login\n");
    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must start"));
}

#[test]
fn lint_accepts_title_suffix_custom_separator_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add login :: PROJ-123\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
title_suffix = "PROJ-[0-9]+"
title_suffix_separator = " :: "
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "feat: add login PROJ-123\n");
    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must end"));
}

#[test]
fn lint_enforces_no_emojis_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add launch \u{1F680}\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
no_emojis = true
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("emoji"));
}

#[test]
fn lint_enforces_ascii_only_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add login\n\nDetails: calf\u{00E9}\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
ascii_only = true
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("ASCII"));
}

#[test]
fn lint_accepts_custom_pattern_flag() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "JIRA-123 Fix login flow\n");

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure();

    gitfluff(dir.path())
        .args(["lint", "--msg-pattern", "^JIRA-[0-9]+\\s.+$", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();
}

#[test]
fn lint_uses_custom_pattern_description() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "update docs\n");

    gitfluff(dir.path())
        .args([
            "lint",
            "--msg-pattern",
            "^JIRA-[0-9]+: .+$",
            "--msg-pattern-description",
            "Ticket prefix required",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Ticket prefix required"));
}

#[test]
fn lint_rejects_emojis_when_enabled() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add launch \u{1F680}\n");

    gitfluff(dir.path())
        .args(["lint", "--no-emojis", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not contain emoji"));

    write_message(&msg_path, "feat: add launch\n");
    gitfluff(dir.path())
        .args(["lint", "--no-emojis", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();
}

#[test]
fn lint_rejects_non_ascii_when_enabled() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add calf\u{00E9}\n");

    gitfluff(dir.path())
        .args(["lint", "--ascii-only", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("ASCII"));

    write_message(&msg_path, "feat: add cafe\n");
    gitfluff(dir.path())
        .args(["lint", "--ascii-only", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();
}

#[test]
fn lint_accepts_required_title_prefix() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-123 * feat: add login\n");

    gitfluff(dir.path())
        .args(["lint", "--title-prefix", "PROJ-[0-9]+", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "feat: add login\n");
    gitfluff(dir.path())
        .args(["lint", "--title-prefix", "PROJ-[0-9]+", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must start"));
}

#[test]
fn lint_accepts_required_title_suffix() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add login (PROJ-123)\n");

    gitfluff(dir.path())
        .args(["lint", "--title-suffix", "\\(PROJ-[0-9]+\\)", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "feat: add login\n");
    gitfluff(dir.path())
        .args(["lint", "--title-suffix", "\\(PROJ-[0-9]+\\)", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must end"));
}

#[test]
fn lint_accepts_title_prefix_with_custom_separator_flag() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-123::feat: add login\n");

    gitfluff(dir.path())
        .args([
            "lint",
            "--title-prefix",
            "PROJ-[0-9]+",
            "--title-prefix-separator",
            "::",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "PROJ-123 feat: add login\n");
    gitfluff(dir.path())
        .args([
            "lint",
            "--title-prefix",
            "PROJ-[0-9]+",
            "--title-prefix-separator",
            "::",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must start"));
}

#[test]
fn lint_accepts_title_suffix_with_custom_separator_flag() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add login :: PROJ-123\n");

    gitfluff(dir.path())
        .args([
            "lint",
            "--title-suffix",
            "PROJ-[0-9]+",
            "--title-suffix-separator",
            " :: ",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .success();

    write_message(&msg_path, "feat: add login PROJ-123\n");
    gitfluff(dir.path())
        .args([
            "lint",
            "--title-suffix",
            "PROJ-[0-9]+",
            "--title-suffix-separator",
            " :: ",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must end"));
}

#[test]
fn lint_cli_overrides_title_prefix_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "CLI-999 * feat: add login\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
title_prefix = "CFG-[0-9]+"
title_prefix_separator = " * "
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must start"));

    gitfluff(dir.path())
        .args(["lint", "--title-prefix", "CLI-[0-9]+", "--from-file"])
        .arg(&msg_path)
        .assert()
        .success();
}

#[test]
fn lint_cli_overrides_title_prefix_separator_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-123 * feat: add login\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
title_prefix = "PROJ-[0-9]+"
title_prefix_separator = "::"
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("title must start"));

    gitfluff(dir.path())
        .args([
            "lint",
            "--title-prefix",
            "PROJ-[0-9]+",
            "--title-prefix-separator",
            " * ",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .success();
}

#[test]
fn lint_cli_overrides_no_emojis_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add launch \u{1F680}\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
no_emojis = false
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();

    gitfluff(dir.path())
        .args(["lint", "--no-emojis", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("emoji"));
}

#[test]
fn lint_cli_overrides_ascii_only_from_config() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add calf\u{00E9}\n");

    fs::write(
        dir.path().join(".gitfluff.toml"),
        r#"
preset = "conventional"

[rules]
ascii_only = false
"#,
    )
    .unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();

    gitfluff(dir.path())
        .args(["lint", "--ascii-only", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("ASCII"));
}

#[test]
fn lint_rejects_emojis_in_body_when_enabled() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "feat: add launch\n\nNotes: \u{1F680}\n");

    gitfluff(dir.path())
        .args(["lint", "--no-emojis", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("emoji"));
}

#[test]
fn lint_title_prefix_applies_before_message_pattern() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-1 * feat: add login\n");

    gitfluff(dir.path())
        .args([
            "lint",
            "--title-prefix",
            "PROJ-[0-9]+",
            "--msg-pattern",
            "^(feat|fix): .+$",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .success();

    gitfluff(dir.path())
        .args([
            "lint",
            "--title-prefix",
            "PROJ-[0-9]+",
            "--msg-pattern",
            "^fix: .+$",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Commit message must match pattern `^fix: .+$`",
        ));
}

#[test]
fn lint_rejects_invalid_title_prefix_regex_flag() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "PROJ-1 * feat: add login\n");

    gitfluff(dir.path())
        .args(["lint", "--title-prefix", "PROJ-[0-9]+(", "--from-file"])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid title prefix regex"));
}

#[test]
fn lint_skips_when_merge_commit_in_progress() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "Merge branch 'feature' into main\n");

    let git_dir = dir.path().join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("MERGE_HEAD"), "deadbeef").unwrap();

    gitfluff(dir.path())
        .arg("lint")
        .arg("--from-file")
        .arg(&msg_path)
        .assert()
        .success();
}

#[test]
fn ai_cleanup_removes_claude_signature_variants() {
    let samples = [
        "feat: keep login\n\n🤖 Generated with [Claude\nCode](https://claude.com/claude-code)\n\n  Co-Authored-By: Claude Sonnet 4.5\n  <noreply@anthropic.com>\n",
        "feat: keep login\n\nGenerated with Claude Code\n\nCo-Authored-By: Claude Sonnet 4.5\n<noreply@anthropic.com>\n",
    ];

    for content in samples {
        let dir = tempdir().unwrap();
        let msg_path = dir.path().join("msg.txt");
        write_message(&msg_path, content);

        gitfluff(dir.path())
            .arg("lint")
            .arg("--write")
            .arg("--from-file")
            .arg(&msg_path)
            .assert()
            .success();

        let cleaned = fs::read_to_string(&msg_path).unwrap();
        assert_eq!(cleaned.trim_end(), "feat: keep login");
    }
}

#[test]
fn cleanup_pattern_sanitizes_message() {
    let dir = tempdir().unwrap();
    let msg_path = dir.path().join("msg.txt");
    write_message(&msg_path, "TEMP: fix bug\n\nDetails here\n");

    gitfluff(dir.path())
        .args([
            "lint",
            "--cleanup-pattern",
            "^TEMP: ",
            "--cleanup-replacement",
            "feat: ",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("cleanup available"));

    gitfluff(dir.path())
        .args([
            "lint",
            "--cleanup-pattern",
            "^TEMP: ",
            "--cleanup-replacement",
            "feat: ",
            "--write",
            "--from-file",
        ])
        .arg(&msg_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("applied cleanup"));

    let rewritten = fs::read_to_string(&msg_path).unwrap();
    assert!(rewritten.starts_with("feat: fix bug"));
}

#[test]
fn hook_install_creates_commit_msg_script() {
    let dir = tempdir().unwrap();
    let git_dir = dir.path().join(".git");
    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();

    gitfluff(dir.path())
        .args(["hook", "install", "commit-msg"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains("Installed commit-msg hook"));

    let script = fs::read_to_string(hooks_dir.join("commit-msg")).unwrap();
    assert!(script.contains("gitfluff lint \"$1\""));
}

#[test]
fn hook_behaves_like_precommit_example() {
    let dir = tempdir().unwrap();
    let git_dir = dir.path().join(".git");
    fs::create_dir_all(git_dir.join("hooks")).unwrap();

    gitfluff(dir.path())
        .args(["hook", "install", "commit-msg", "--write"])
        .assert()
        .success();

    let commit_msg_file = dir.path().join("COMMIT_EDITMSG");
    write_message(
        &commit_msg_file,
        "feat: add login\n\n🤖 Generated with Claude\nCo-Authored-By: Claude <noreply@anthropic.com>\n",
    );

    let script_path = dir.path().join(".git/hooks/commit-msg");
    let gitfluff_bin_dir = cargo::cargo_bin!("gitfluff")
        .parent()
        .expect("bin directory")
        .to_path_buf();
    let path_var = env::var("PATH").unwrap_or_default();
    let mut hook_cmd = Command::new("sh");
    hook_cmd.arg(&script_path).arg(&commit_msg_file).env(
        "PATH",
        format!("{}:{}", gitfluff_bin_dir.display(), path_var),
    );
    hook_cmd.current_dir(dir.path());
    hook_cmd.assert().success();

    let cleaned = fs::read_to_string(&commit_msg_file).unwrap();
    assert_eq!(cleaned.trim_end(), "feat: add login");
}
