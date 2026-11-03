//! The `poly commit` flow: load the message, resolve the rules, lint, report and write back.

use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use gitfluff::config::{CleanupRuleConfig, ExcludeRuleConfig};
use gitfluff::{LintSettings, git_sequencer_in_progress, has_generated_subject, lint_message, resolve_lint_options};
use poly_config::BaseConfigResolver;

use super::args::CommitArgs;
use super::config;
use crate::global::ColorChoice;

/// Delimiter separating a regex from its custom message or replacement text. A multi-character
/// delimiter, because a single `:` cannot be told apart from the colon in `(?:...)` or `(?i)`.
const RULE_ARG_DELIMITER: &str = "->";

/// Lint (and optionally rewrite) a commit message according to the resolved preset, the
/// `[commit]` config and the command-line overrides. Returns the process exit code.
///
/// `resolver` resolves the `poly.toml` `extends` bases the commit config may inherit from.
///
/// # Errors
///
/// Returns an error if the message source is missing, the config is invalid, a preset is
/// unknown, a user-supplied regex fails to compile, or writing the cleaned message back fails.
pub fn run_commit_lint(args: CommitArgs, resolver: &dyn BaseConfigResolver) -> Result<i32> {
    let message_data = load_message(&args)?;
    let cwd = std::env::current_dir().context("failed to discover current directory")?;

    if has_generated_subject(&message_data.text) {
        return Ok(0);
    }

    // The git-state skip is keyed on the working directory, so it applies only when the message
    // is that repository's pending commit message file: a `--message` string may be a PR title
    // validated from a checkout that happens to be mid-merge.
    if let MessageSource::File(path) = &message_data.source
        && git_sequencer_in_progress(&cwd, path)
    {
        return Ok(0);
    }

    let mut reporter = Reporter::new(args.color);
    let loaded_config = config::load(args.config.as_deref(), &cwd, resolver)?;
    let settings = settings_from_args(&args)?;

    let resolved = resolve_lint_options(
        loaded_config.as_ref().map(|(path, cfg)| (path.as_path(), cfg)),
        &settings,
    )?;
    let mut options = resolved.options;
    let mut write_requested = resolved.write;
    let exit_nonzero_on_rewrite = resolved.exit_nonzero_on_rewrite;

    // A literal message has nowhere to persist a rewrite. An explicit `--write` still prints the
    // cleaned message to stdout; a `write` that only came from the config is ignored.
    if write_requested && !args.write && message_data.source == MessageSource::Literal {
        reporter.warn(
            "`write` is ignored for `--message`: a literal message has nowhere to persist a \
             rewrite, so violations are reported as-is",
        )?;
        write_requested = false;
    }

    options.autofix = write_requested;

    let outcome = lint_message(&message_data.text, &options);

    let summary_label = if write_requested {
        "applied cleanup"
    } else {
        "cleanup available"
    };
    for summary in &outcome.cleanup_summaries {
        reporter.info(format!("{summary_label}: {summary}"))?;
    }

    let active_violations = if write_requested {
        for fixed in outcome
            .violations_before
            .iter()
            .filter(|msg| !outcome.violations_after.contains(msg))
        {
            reporter.info(format!("fixed: {fixed}"))?;
        }
        for warning in &outcome.warnings_after {
            reporter.warn(warning)?;
        }
        for violation in &outcome.violations_after {
            reporter.error(violation)?;
        }
        &outcome.violations_after
    } else {
        for warning in &outcome.warnings_before {
            reporter.warn(warning)?;
        }
        for violation in &outcome.violations_before {
            reporter.error(violation)?;
        }
        &outcome.violations_before
    };

    if let Some(notice) = &resolved.custom_pattern_notice
        && !active_violations.is_empty()
    {
        reporter.info(notice)?;
    }

    let did_rewrite = write_requested && outcome.cleaned_message != message_data.text;

    if write_requested {
        apply_write(&message_data, &outcome.cleaned_message)?;
    }

    if active_violations.is_empty() {
        if did_rewrite && exit_nonzero_on_rewrite {
            reporter.info("commit message was rewritten; please re-run the commit to review changes")?;
            Ok(1)
        } else {
            Ok(0)
        }
    } else {
        Ok(1)
    }
}

/// Translate the command-line overrides into gitfluff's [`LintSettings`].
fn settings_from_args(args: &CommitArgs) -> Result<LintSettings> {
    let mut settings = LintSettings {
        preset: args.preset.clone(),
        msg_pattern: args.msg_pattern.clone(),
        msg_pattern_description: args.msg_pattern_description.clone(),
        no_emojis: args.no_emojis,
        ascii_only: args.ascii_only,
        // A separator flag has a default, so it overrides the config's only alongside its pattern.
        title_prefix: args.title_prefix.clone(),
        title_prefix_separator: args.title_prefix.as_ref().map(|_| args.title_prefix_separator.clone()),
        title_suffix: args.title_suffix.clone(),
        title_suffix_separator: args.title_suffix.as_ref().map(|_| args.title_suffix_separator.clone()),
        no_ai_cleanup: args.no_ai_cleanup,
        single_line: args.single_line,
        require_body: args.require_body,
        write: args.write,
        exit_nonzero_on_rewrite: args.exit_nonzero_on_rewrite,
        ..Default::default()
    };

    for exclude in &args.exclude {
        let (pattern, message) = parse_exclude_arg(exclude)?;
        settings.excludes.push(ExcludeRuleConfig { pattern, message });
    }

    for cleanup in &args.cleanup {
        let (find, replace) = parse_cleanup_arg(cleanup)?;
        settings.cleanups.push(CleanupRuleConfig {
            find,
            replace,
            description: None,
        });
    }

    if let Some(pattern) = &args.cleanup_pattern {
        settings.cleanups.push(CleanupRuleConfig {
            find: pattern.clone(),
            replace: args.cleanup_replacement.clone().unwrap_or_default(),
            description: args.cleanup_description.clone(),
        });
    }

    Ok(settings)
}

fn apply_write(message: &MessageData, cleaned: &str) -> Result<()> {
    match &message.source {
        MessageSource::File(path) => {
            if cleaned != message.text {
                fs::write(path, cleaned)
                    .with_context(|| format!("failed to write cleaned commit message to {}", path.display()))?;
            }
        }
        MessageSource::Stdin | MessageSource::Literal => {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(cleaned.as_bytes())
                .context("failed to write cleaned message to stdout")?;
        }
    }
    Ok(())
}

const NO_SOURCE: &str = "no commit message source provided (pass COMMIT_FILE, --from-file, --stdin, or --message)";

fn load_message(args: &CommitArgs) -> Result<MessageData> {
    let read_file = |path: &PathBuf| {
        fs::read_to_string(path)
            .with_context(|| format!("failed to read commit message from {}", path.display()))
            .map(|text| MessageData {
                text,
                source: MessageSource::File(path.clone()),
            })
    };

    if let Some(path) = &args.from_file {
        read_file(path)
    } else if let Some(path) = &args.commit_file {
        read_file(path)
    } else if args.stdin {
        let mut text = String::new();
        io::stdin()
            .read_to_string(&mut text)
            .context("failed to read commit message from stdin")?;
        Ok(MessageData {
            text,
            source: MessageSource::Stdin,
        })
    } else if let Some(message) = &args.message {
        Ok(MessageData {
            text: message.clone(),
            source: MessageSource::Literal,
        })
    } else {
        Err(anyhow!(NO_SOURCE))
    }
}

fn parse_exclude_arg(raw: &str) -> Result<(String, Option<String>)> {
    let (pattern, message) = match raw.split_once(RULE_ARG_DELIMITER) {
        Some((pattern, message)) => {
            let message = message.trim();
            (pattern, (!message.is_empty()).then(|| message.to_string()))
        }
        None => (raw, None),
    };

    if pattern.trim().is_empty() {
        return Err(anyhow!(
            "exclude argument must be `REGEX` or `REGEX{RULE_ARG_DELIMITER}MESSAGE` with a non-empty regex (got `{raw}`)"
        ));
    }

    Ok((pattern.to_string(), message))
}

fn parse_cleanup_arg(raw: &str) -> Result<(String, String)> {
    let Some((find, replace)) = raw.split_once(RULE_ARG_DELIMITER) else {
        return Err(anyhow!(
            "cleanup argument must use `FIND{RULE_ARG_DELIMITER}REPLACE` format (got `{raw}`)"
        ));
    };

    if find.trim().is_empty() {
        return Err(anyhow!(
            "cleanup argument must use `FIND{RULE_ARG_DELIMITER}REPLACE` format with a non-empty regex (got `{raw}`)"
        ));
    }

    Ok((find.to_string(), replace.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageData {
    text: String,
    source: MessageSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MessageSource {
    File(PathBuf),
    Stdin,
    Literal,
}

/// Writes `gitfluff: <level>: <line>` diagnostics to stderr, coloured the way the standalone
/// tool does so the output stays identical across both entry points.
struct Reporter {
    color: bool,
    stderr: io::Stderr,
}

impl Reporter {
    fn new(choice: ColorChoice) -> Self {
        let color = match choice {
            ColorChoice::Auto => io::stderr().is_terminal(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        };
        Self {
            color,
            stderr: io::stderr(),
        }
    }

    fn error(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        self.write_line("error", msg.as_ref(), "\x1b[31m")
    }

    fn info(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        self.write_line("info", msg.as_ref(), "\x1b[36m")
    }

    fn warn(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        self.write_line("warn", msg.as_ref(), "\x1b[33m")
    }

    fn write_line(&mut self, level: &str, msg: &str, color: &str) -> io::Result<()> {
        const RESET: &str = "\x1b[0m";
        let mut stderr = self.stderr.lock();
        for line in msg.split('\n') {
            if self.color {
                writeln!(stderr, "gitfluff: {color}{level}{RESET}: {line}")?;
            } else {
                writeln!(stderr, "gitfluff: {level}: {line}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclude_arg_splits_on_arrow_only() {
        assert_eq!(
            parse_exclude_arg("(?i)^wip->no WIP").unwrap(),
            ("(?i)^wip".to_string(), Some("no WIP".to_string()))
        );
        assert_eq!(parse_exclude_arg("(?:a|b)").unwrap(), ("(?:a|b)".to_string(), None));
        assert!(parse_exclude_arg("->msg").is_err());
    }

    #[test]
    fn cleanup_arg_requires_the_arrow() {
        assert_eq!(parse_cleanup_arg("a->").unwrap(), ("a".to_string(), String::new()));
        assert!(parse_cleanup_arg("a").is_err());
    }

    #[test]
    fn separators_override_the_config_only_with_their_pattern() {
        let mut args = blank_args();
        let settings = settings_from_args(&args).unwrap();
        assert_eq!(settings.title_prefix_separator, None);
        assert_eq!(settings.title_suffix_separator, None);

        args.title_prefix = Some("^\\w+".into());
        let settings = settings_from_args(&args).unwrap();
        assert_eq!(settings.title_prefix_separator.as_deref(), Some(" * "));
        assert_eq!(settings.title_suffix_separator, None);
    }

    fn blank_args() -> CommitArgs {
        CommitArgs {
            from_file: None,
            stdin: false,
            message: None,
            commit_file: None,
            preset: None,
            msg_pattern: None,
            msg_pattern_description: None,
            exclude: Vec::new(),
            cleanup: Vec::new(),
            cleanup_pattern: None,
            cleanup_replacement: None,
            cleanup_description: None,
            no_emojis: false,
            ascii_only: false,
            title_prefix: None,
            title_prefix_separator: " * ".into(),
            title_suffix: None,
            title_suffix_separator: " ".into(),
            no_ai_cleanup: false,
            config: None,
            write: false,
            color: ColorChoice::Never,
            single_line: false,
            require_body: false,
            exit_nonzero_on_rewrite: false,
        }
    }
}
