//! Application orchestration for the `gitfluff` command line: wires CLI parsing,
//! config loading, presets, and the linting core into the lint/cleanup flow that
//! the standalone binary (and, later, an in-process `poly commit`) drives.

use std::fs;
use std::io::IsTerminal;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use crate::cli::{Cli, ColorMode, Commands, HookCommand, HookInstallArgs, LintArgs};
use crate::config::load_config;
use crate::hooks::install_hook;
use crate::lint::{
    BodyPolicy, LintOptions, build_cleanup_rule, build_exclude_rule, build_message_pattern,
    build_title_prefix_rule, build_title_suffix_rule, lint_message,
};
use crate::presets::resolve_preset;

const AI_EXCLUDE_RULES: &[(&str, &str)] = &[
    (
        "(?mi)^Co-Authored-By:.*(?:Claude|Anthropic|ChatGPT|GPT|OpenAI).*$",
        "Remove AI co-author attribution lines",
    ),
    (
        "🤖 Generated with",
        "Remove AI generation notices from commit messages",
    ),
];

const AI_CLEANUP_RULES: &[(&str, &str, &str)] = &[
    (
        "(?ims)\\n?\\s*(?:🤖\\s*)?Generated with.*?(?:Co-Authored-By:.*(?:Claude|Anthropic).*(?:\\n\\s*<[^>\\n]+>)?)+\\s*",
        "\n",
        "Remove Claude Code attribution block",
    ),
    (
        "(?m)^.*🤖 Generated with.*\n?",
        "",
        "Remove AI generation banner",
    ),
    (
        "(?mi)^Generated with Claude.*\n?",
        "",
        "Remove plain Claude generation banner",
    ),
    (
        "(?mi)^Co-Authored-By:.*(?:Claude|Anthropic).*\n?",
        "",
        "Drop Co-Authored-By lines referencing AI assistants",
    ),
    ("(?mi)^-\\s*Claude.*\n?", "", "Remove Claude bullet entries"),
    (
        "(?s)\\A\\s*\n+",
        "",
        "Trim leading blank lines introduced by cleanup",
    ),
    (
        "(?s)\n\\s*\n\\z",
        "\n",
        "Trim trailing blank lines introduced by cleanup",
    ),
    ("\n{3,}", "\n\n", "Collapse excessive blank lines"),
];

const DEFAULT_TITLE_PREFIX_SEPARATOR: &str = " * ";
const DEFAULT_TITLE_SUFFIX_SEPARATOR: &str = " ";

/// Run the full `gitfluff` CLI and translate any error into the process exit
/// code (`2`), reporting it to stderr exactly as the standalone binary does.
///
/// This is the single entry point the thin `main.rs` binary defers to.
pub fn main_entry() -> i32 {
    match run() {
        Ok(code) => code,
        Err(err) => {
            let mut reporter = Reporter::new(ColorMode::Auto);
            let _ = reporter.error(format_error(&err));
            2
        }
    }
}

/// Parse the command line and dispatch to the matching subcommand, returning the
/// intended process exit code.
///
/// # Errors
///
/// Returns an error if a subcommand fails (e.g. invalid config, unreadable
/// commit-message source, or an invalid regex in a rule).
pub fn run() -> Result<i32> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Lint(args) => run_lint(*args),
        Commands::Hook(HookCommand::Install(args)) => run_hook_install(args),
    }
}

/// Install a git hook (currently `commit-msg`) into the surrounding repository.
///
/// # Errors
///
/// Returns an error if the `.git` directory cannot be located or the hook file
/// cannot be written.
pub fn run_hook_install(args: HookInstallArgs) -> Result<i32> {
    let cwd = std::env::current_dir().context("failed to discover current directory")?;
    let path = install_hook(&cwd, args.kind, args.write, args.force)?;
    println!(
        "gitfluff: info: Installed {} hook at {}",
        hook_label(args.kind),
        path.display()
    );
    Ok(0)
}

/// Lint (and optionally rewrite) a commit message according to the resolved
/// preset, configuration file, and command-line overrides.
///
/// # Errors
///
/// Returns an error if the message source is missing, the config is invalid, a
/// preset is unknown, a user-supplied regex fails to compile, or writing the
/// cleaned message back fails.
pub fn run_lint(args: LintArgs) -> Result<i32> {
    let message_data = load_message(&args)?;
    let cwd = std::env::current_dir().context("failed to discover current directory")?;

    if is_merge_commit_in_progress(&cwd) {
        return Ok(0);
    }

    let mut reporter = Reporter::new(args.color);
    let loaded_config = load_config(args.config.as_deref(), &cwd)?;

    let preset_name = args
        .preset
        .clone()
        .or_else(|| {
            loaded_config
                .as_ref()
                .and_then(|(_, cfg)| cfg.preset.clone())
        })
        .unwrap_or_else(|| "conventional".to_string());

    let preset =
        resolve_preset(&preset_name).ok_or_else(|| anyhow!("unknown preset `{}`", preset_name))?;

    let mut enforce_spec = preset.enforce_spec;
    let mut message_pattern = Some(build_message_pattern(
        preset.message_pattern,
        Some(preset.description.to_string()),
    )?);

    if let Some((_, cfg)) = &loaded_config
        && let Some(rule) = &cfg.rules.message
    {
        message_pattern = Some(build_message_pattern(
            &rule.pattern,
            rule.description.clone(),
        )?);
        enforce_spec = false;
    }

    if let Some(pattern) = &args.msg_pattern {
        let desc = args
            .msg_pattern_description
            .clone()
            .or_else(|| Some(format!("Commit message must match pattern `{pattern}`")));
        message_pattern = Some(build_message_pattern(pattern, desc)?);
        enforce_spec = false;
    } else if args.msg_pattern_description.is_some()
        && let Some(mp) = message_pattern.as_mut()
    {
        mp.description = args.msg_pattern_description.clone();
    }

    let mut options = LintOptions {
        message_pattern,
        body_policy: preset.body_policy,
        enforce_conventional_spec: enforce_spec,
        ..Default::default()
    };

    let mut body_policy = preset.body_policy;
    let mut forbid_emojis = false;
    let mut forbid_non_ascii = false;
    let mut title_prefix_pattern: Option<String> = None;
    let mut title_prefix_separator = DEFAULT_TITLE_PREFIX_SEPARATOR.to_string();
    let mut title_suffix_pattern: Option<String> = None;
    let mut title_suffix_separator = DEFAULT_TITLE_SUFFIX_SEPARATOR.to_string();

    if let Some((_, cfg)) = &loaded_config {
        let single_line_flag = cfg.rules.single_line.unwrap_or(false);
        let require_body_flag = cfg.rules.require_body.unwrap_or(false);
        forbid_emojis = cfg.rules.no_emojis.unwrap_or(false);
        forbid_non_ascii = cfg.rules.ascii_only.unwrap_or(false);

        if let Some(pattern) = &cfg.rules.title_prefix {
            title_prefix_pattern = Some(pattern.clone());
        }
        if let Some(separator) = &cfg.rules.title_prefix_separator {
            title_prefix_separator = separator.clone();
        }
        if let Some(pattern) = &cfg.rules.title_suffix {
            title_suffix_pattern = Some(pattern.clone());
        }
        if let Some(separator) = &cfg.rules.title_suffix_separator {
            title_suffix_separator = separator.clone();
        }

        if single_line_flag && require_body_flag {
            return Err(anyhow!(
                "configuration cannot enable both `single_line` and `require_body` rules"
            ));
        }

        if single_line_flag {
            body_policy = BodyPolicy::SingleLine;
        } else if require_body_flag {
            body_policy = BodyPolicy::RequireBody;
        } else {
            if matches!(cfg.rules.single_line, Some(false))
                && matches!(body_policy, BodyPolicy::SingleLine)
            {
                body_policy = BodyPolicy::Any;
            }
            if matches!(cfg.rules.require_body, Some(false))
                && matches!(body_policy, BodyPolicy::RequireBody)
            {
                body_policy = BodyPolicy::Any;
            }
        }

        for exclude in &cfg.rules.excludes {
            options.exclude_rules.push(build_exclude_rule(
                &exclude.pattern,
                exclude.message.clone(),
            )?);
        }

        for cleanup in &cfg.rules.cleanup {
            options.cleanup_rules.push(build_cleanup_rule(
                &cleanup.find,
                &cleanup.replace,
                cleanup.description.clone(),
            )?);
        }
    }

    for exclude in &args.exclude {
        let (pattern, message) = parse_exclude_arg(exclude)?;
        options
            .exclude_rules
            .push(build_exclude_rule(&pattern, message)?);
    }

    for cleanup in &args.cleanup {
        let (find, replace) = parse_cleanup_arg(cleanup)?;
        options
            .cleanup_rules
            .push(build_cleanup_rule(&find, &replace, None)?);
    }

    if let Some(pattern) = &args.cleanup_pattern {
        let replace = args.cleanup_replacement.clone().unwrap_or_default();
        options.cleanup_rules.push(build_cleanup_rule(
            pattern,
            &replace,
            args.cleanup_description.clone(),
        )?);
    }

    if args.single_line {
        body_policy = BodyPolicy::SingleLine;
    } else if args.require_body {
        body_policy = BodyPolicy::RequireBody;
    }

    if args.no_emojis {
        forbid_emojis = true;
    }
    if args.ascii_only {
        forbid_non_ascii = true;
    }
    if let Some(pattern) = &args.title_prefix {
        title_prefix_pattern = Some(pattern.clone());
        title_prefix_separator = args.title_prefix_separator.clone();
    }
    if let Some(pattern) = &args.title_suffix {
        title_suffix_pattern = Some(pattern.clone());
        title_suffix_separator = args.title_suffix_separator.clone();
    }

    let write_requested = if args.write {
        true
    } else if let Some((_, cfg)) = &loaded_config {
        cfg.write.unwrap_or(false)
    } else {
        false
    };

    options.autofix = write_requested;

    let exit_nonzero_on_rewrite = if args.exit_nonzero_on_rewrite {
        true
    } else if let Some((_, cfg)) = &loaded_config {
        cfg.rules.exit_nonzero_on_rewrite.unwrap_or(false)
    } else {
        false
    };

    options.body_policy = body_policy;
    options.forbid_emojis = forbid_emojis;
    options.forbid_non_ascii = forbid_non_ascii;

    if let Some(pattern) = title_prefix_pattern.as_ref() {
        options.title_prefix = Some(build_title_prefix_rule(pattern, &title_prefix_separator)?);
    }

    if let Some(pattern) = title_suffix_pattern.as_ref() {
        options.title_suffix = Some(build_title_suffix_rule(pattern, &title_suffix_separator)?);
    }

    for (pattern, message) in AI_EXCLUDE_RULES {
        options
            .exclude_rules
            .push(build_exclude_rule(pattern, Some((*message).to_string()))?);
    }

    for (find, replace, desc) in AI_CLEANUP_RULES {
        options.cleanup_rules.push(build_cleanup_rule(
            find,
            replace,
            Some((*desc).to_string()),
        )?);
    }

    let outcome = lint_message(&message_data.text, &options);

    if outcome.cleanup_summaries.is_empty() {
        // nothing to do
    } else if write_requested {
        for summary in &outcome.cleanup_summaries {
            reporter.info(format!("applied cleanup: {summary}"))?;
        }
    } else {
        for summary in &outcome.cleanup_summaries {
            reporter.info(format!("cleanup available: {summary}"))?;
        }
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

    let did_rewrite = write_requested && outcome.cleaned_message != message_data.text;

    if write_requested {
        apply_write(&message_data, &outcome.cleaned_message)?;
    } else if message_data.source == MessageSource::Literal && !active_violations.is_empty() {
        // no-op, keep behavior simple
    }

    if active_violations.is_empty() {
        if did_rewrite && exit_nonzero_on_rewrite {
            reporter
                .info("commit message was rewritten; please re-run the commit to review changes")?;
            Ok(1)
        } else {
            Ok(0)
        }
    } else {
        Ok(1)
    }
}

fn apply_write(message: &MessageData, cleaned: &str) -> Result<()> {
    match &message.source {
        MessageSource::File(path) => {
            if cleaned != message.text {
                fs::write(path, cleaned).with_context(|| {
                    format!(
                        "failed to write cleaned commit message to {}",
                        path.display()
                    )
                })?;
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

fn load_message(args: &LintArgs) -> Result<MessageData> {
    if args.from_file.is_none()
        && args.commit_file.is_none()
        && !args.stdin
        && args.message.is_none()
    {
        return Err(anyhow!(
            "no commit message source provided (pass COMMIT_FILE, --from-file, --stdin, or --message)"
        ));
    }

    let (text, source) = if let Some(path) = &args.from_file {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read commit message from {}", path.display()))?;
        (content, MessageSource::File(path.clone()))
    } else if let Some(path) = &args.commit_file {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read commit message from {}", path.display()))?;
        (content, MessageSource::File(path.clone()))
    } else if args.stdin {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .context("failed to read commit message from stdin")?;
        (buf, MessageSource::Stdin)
    } else if let Some(message) = &args.message {
        (message.clone(), MessageSource::Literal)
    } else {
        return Err(anyhow!(
            "no commit message source provided (pass COMMIT_FILE, --from-file, --stdin, or --message)"
        ));
    };

    Ok(MessageData { text, source })
}

fn parse_exclude_arg(raw: &str) -> Result<(String, Option<String>)> {
    if let Some((pattern, message)) = raw.split_once(':') {
        if message.is_empty() {
            Ok((pattern.to_string(), None))
        } else {
            Ok((pattern.to_string(), Some(message.to_string())))
        }
    } else {
        Ok((raw.to_string(), None))
    }
}

fn parse_cleanup_arg(raw: &str) -> Result<(String, String)> {
    if let Some((find, replace)) = raw.split_once("->") {
        Ok((find.to_string(), replace.to_string()))
    } else {
        Err(anyhow!(
            "cleanup argument must use `find->replace` format (got `{raw}`)"
        ))
    }
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

fn format_error(err: &anyhow::Error) -> String {
    let mut msg = err.to_string();
    for cause in err.chain().skip(1) {
        msg.push_str(&format!("\n  caused by: {}", cause));
    }
    msg
}

fn hook_label(kind: crate::hooks::HookKind) -> &'static str {
    match kind {
        crate::hooks::HookKind::CommitMsg => "commit-msg",
    }
}

struct Reporter {
    color: bool,
    stderr: io::Stderr,
}

impl Reporter {
    fn new(mode: ColorMode) -> Self {
        let is_tty = io::stderr().is_terminal();
        let color = match mode {
            ColorMode::Auto => is_tty,
            ColorMode::Always => true,
            ColorMode::Never => false,
        };

        Self {
            color,
            stderr: io::stderr(),
        }
    }

    fn error(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        self.write_line("error", msg.as_ref(), Some(Ansi::Red))
    }

    fn info(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        self.write_line("info", msg.as_ref(), Some(Ansi::Cyan))
    }

    fn warn(&mut self, msg: impl AsRef<str>) -> io::Result<()> {
        self.write_line("warn", msg.as_ref(), Some(Ansi::Yellow))
    }

    fn write_line(&mut self, level: &str, msg: &str, color: Option<Ansi>) -> io::Result<()> {
        let mut stderr = self.stderr.lock();
        for line in msg.split('\n') {
            if self.color {
                if let Some(color) = color {
                    writeln!(
                        stderr,
                        "gitfluff: {}{}{}: {}",
                        color.code(),
                        level,
                        Ansi::Reset.code(),
                        line
                    )?;
                } else {
                    writeln!(stderr, "gitfluff: {level}: {line}")?;
                }
            } else {
                writeln!(stderr, "gitfluff: {level}: {line}")?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Ansi {
    Red,
    Yellow,
    Cyan,
    Reset,
}

impl Ansi {
    fn code(self) -> &'static str {
        match self {
            Ansi::Red => "\x1b[31m",
            Ansi::Yellow => "\x1b[33m",
            Ansi::Cyan => "\x1b[36m",
            Ansi::Reset => "\x1b[0m",
        }
    }
}

fn is_merge_commit_in_progress(start_dir: &std::path::Path) -> bool {
    let mut current = start_dir;
    loop {
        let git_dir = current.join(".git");
        if git_dir.is_dir() {
            return git_dir.join("MERGE_HEAD").exists();
        }
        if git_dir.is_file() {
            if let Ok(resolved) = resolve_gitdir_file(&git_dir) {
                return resolved.join("MERGE_HEAD").exists();
            }
            return false;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return false,
        }
    }
}

fn resolve_gitdir_file(git_file: &std::path::Path) -> Result<std::path::PathBuf> {
    let content = fs::read_to_string(git_file)
        .with_context(|| format!("failed to read gitdir file {}", git_file.display()))?;
    let content = content.trim();

    let prefix = "gitdir:";
    if let Some(rest) = content.strip_prefix(prefix) {
        let raw = rest.trim();
        let path = std::path::Path::new(raw);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            git_file
                .parent()
                .context("gitdir file missing parent")?
                .join(path)
                .canonicalize()
                .with_context(|| format!("failed to resolve gitdir path {}", path.display()))?
        };
        Ok(resolved)
    } else {
        Err(anyhow!(
            "unexpected gitdir file format in {}",
            git_file.display()
        ))
    }
}
