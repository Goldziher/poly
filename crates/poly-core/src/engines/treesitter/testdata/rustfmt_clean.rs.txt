//! High-level, clap-free API for embedding gitfluff.
//!
//! [`resolve_lint_options`] turns a loaded [`FileConfig`] plus a set of overrides
//! ([`LintSettings`], the library equivalent of the CLI flags) into ready-to-run
//! [`LintOptions`]; [`lint_with_config`] does that and lints a message in one call.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::config::{CleanupRuleConfig, ExcludeRuleConfig, FileConfig};
use crate::lint::{
    BodyPolicy, LintOptions, LintOutcome, build_cleanup_rule, build_exclude_rule, build_message_pattern,
    build_title_prefix_rule, build_title_suffix_rule, lint_message,
};
use crate::presets::resolve_preset;

const AI_EXCLUDE_RULES: &[(&str, &str)] = &[
    (
        "(?mi)^Co-Authored-By:.*(?:Claude|Anthropic|ChatGPT|GPT|OpenAI).*$",
        "Remove AI co-author attribution lines",
    ),
    ("🤖 Generated with", "Remove AI generation notices from commit messages"),
];

/// Built-in cleanup rules for AI attribution.
///
/// Every rule is line-anchored with `(?m)` plus `^`/`$`: it may only ever match the specific
/// trailer or banner line(s) it targets. A rule must never span arbitrary intervening lines,
/// because doing so silently deletes the user's own body text.
const AI_CLEANUP_RULES: &[(&str, &str, &str)] = &[
    // The Claude Code banner wraps its markdown link across exactly two lines:
    //   🤖 Generated with [Claude
    //   Code](https://claude.com/claude-code)
    // Both lines are spelled out so nothing between them can be swallowed. The second line
    // tolerates trailing text after the URL: without that, this rule misses and the
    // single-line banner rule below strips line 1 only, orphaning `Code](...)` in the body.
    (
        "(?mi)^[ \\t]*(?:🤖[ \\t]*)?Generated with \\[Claude[ \\t\\r]*\\n[ \\t]*Code\\]\\([^)\\r\\n]*\\)[^\\n]*$\\n?",
        "",
        "Remove wrapped Claude Code attribution banner",
    ),
    (
        "(?m)^[^\\n]*🤖 Generated with[^\\n]*$\\n?",
        "",
        "Remove AI generation banner",
    ),
    (
        "(?mi)^[ \\t]*Generated with Claude[^\\n]*$\\n?",
        "",
        "Remove plain Claude generation banner",
    ),
    // The trailer is optionally followed by a line holding only the bare `<email>`. That
    // continuation must itself name an AI assistant: matching any `<...>` line swallowed
    // unrelated footers such as a bare `<https://issue.example.com/123>`.
    (
        "(?mi)^[ \\t]*Co-Authored-By:[^\\n]*(?:Claude|Anthropic|ChatGPT|GPT|OpenAI)[^\\n]*$\\n?\
         (?:[ \\t]*<[^>\\n]*(?:claude|anthropic|chatgpt|openai)[^>\\n]*>[ \\t\\r]*$\\n?)?",
        "",
        "Drop Co-Authored-By lines referencing AI assistants",
    ),
    // Only a bullet that is *nothing but* AI attribution, so a real body line such as
    // `- Claude Monet gallery importer` survives.
    (
        "(?mi)^[ \\t]*-[ \\t]*Claude(?:[ \\t]+(?:Code|AI|Sonnet|Opus|Haiku)(?:[ \\t]+[0-9][0-9.]*)?)?(?:[ \\t]*<[^>\\n]+>)?[ \\t\\r]*$\\n?",
        "",
        "Remove standalone Claude attribution bullets",
    ),
    ("\\A\\s*\\n+", "", "Trim leading blank lines introduced by cleanup"),
    ("\\n\\s*\\n\\z", "\n", "Trim trailing blank lines introduced by cleanup"),
    ("\\n{3,}", "\n\n", "Collapse excessive blank lines"),
];

const DEFAULT_TITLE_PREFIX_SEPARATOR: &str = " * ";
const DEFAULT_TITLE_SUFFIX_SEPARATOR: &str = " ";

/// Entries inside the git directory that mean git is composing the commit message itself
/// (merge, revert, cherry-pick). Those messages are not hand authored, so linting them only
/// blocks the operation.
///
/// Deliberately excludes `rebase-merge` and `rebase-apply`: those exist for the whole of an
/// interactive rebase, so keying on them skipped every hook run during one -- including a
/// `reword`, where the message *is* hand authored. The subjects git generates during a rebase
/// are already covered by [`GENERATED_SUBJECT_PREFIXES`], which is checked first.
const GIT_SEQUENCER_MARKERS: &[&str] = &["MERGE_HEAD", "REVERT_HEAD", "CHERRY_PICK_HEAD"];

/// Subject prefixes git generates itself, for `git revert` and `git commit --fixup/--squash`.
const GENERATED_SUBJECT_PREFIXES: &[&str] = &["Revert \"", "fixup! ", "squash! ", "amend! "];

/// Overrides applied on top of a [`FileConfig`], mirroring the `gitfluff lint` flags.
///
/// Boolean fields can only force a rule on (like the corresponding flags); `Option` fields
/// replace the config file value when set.
#[derive(Debug, Clone, Default)]
pub struct LintSettings {
    pub preset: Option<String>,
    pub msg_pattern: Option<String>,
    pub msg_pattern_description: Option<String>,
    pub excludes: Vec<ExcludeRuleConfig>,
    pub cleanups: Vec<CleanupRuleConfig>,
    pub no_emojis: bool,
    pub ascii_only: bool,
    pub title_prefix: Option<String>,
    pub title_prefix_separator: Option<String>,
    pub title_suffix: Option<String>,
    pub title_suffix_separator: Option<String>,
    pub no_ai_cleanup: bool,
    pub single_line: bool,
    pub require_body: bool,
    pub write: bool,
    pub exit_nonzero_on_rewrite: bool,
}

/// The result of resolving configuration and overrides.
#[derive(Debug)]
pub struct ResolvedLint {
    /// Options to pass to [`lint_message`]; `autofix` mirrors [`ResolvedLint::write`].
    pub options: LintOptions,
    /// Whether the cleaned message should be written back (flag or config `write`).
    pub write: bool,
    /// Whether a rewrite that leaves the message valid should still exit non-zero.
    pub exit_nonzero_on_rewrite: bool,
    /// Explanation to show when a lint fails under a custom `[rules.message]`.
    pub custom_pattern_notice: Option<String>,
}

/// Resolves preset, config file rules and overrides into [`LintOptions`].
///
/// `config` is the loaded config together with the path it came from (used in diagnostics).
///
/// # Errors
///
/// Returns an error for an unknown preset, an invalid regex, or a configuration enabling
/// both `single_line` and `require_body`.
pub fn resolve_lint_options(config: Option<(&Path, &FileConfig)>, settings: &LintSettings) -> Result<ResolvedLint> {
    let preset_name = settings
        .preset
        .clone()
        .or_else(|| config.and_then(|(_, cfg)| cfg.preset.clone()))
        .unwrap_or_else(|| "conventional".to_string());

    let preset = resolve_preset(&preset_name).ok_or_else(|| anyhow!("unknown preset `{}`", preset_name))?;

    let mut enforce_spec = preset.enforce_spec;
    let mut enforce_line_lengths = false;
    let mut enforce_subject_style = false;
    let mut custom_pattern_notice = None;
    let mut message_pattern = Some(build_message_pattern(
        preset.message_pattern,
        Some(preset.description.to_string()),
    )?);

    if let Some((path, cfg)) = config
        && let Some(rule) = &cfg.rules.message
    {
        message_pattern = Some(build_message_pattern(&rule.pattern, rule.description.clone())?);
        enforce_spec = false;
        enforce_line_lengths = rule.enforce_length.unwrap_or(false);
        enforce_subject_style = rule.enforce_case.unwrap_or(false);
        // Held back until the lint actually fails. Printing it on every run buried a clean
        // commit under a paragraph of text that only helps once something is rejected.
        custom_pattern_notice = Some(format!(
            "`[rules.message]` in {} replaces the Conventional Commits spec check: length checks \
             are {}, case checks are {} (toggle with `enforce_length` / `enforce_case` under \
             `[rules.message]`)",
            path.display(),
            on_off(enforce_line_lengths),
            on_off(enforce_subject_style)
        ));
    }

    if let Some(pattern) = &settings.msg_pattern {
        let desc = settings
            .msg_pattern_description
            .clone()
            .or_else(|| Some(format!("Commit message must match pattern `{pattern}`")));
        message_pattern = Some(build_message_pattern(pattern, desc)?);
        enforce_spec = false;
    } else if settings.msg_pattern_description.is_some()
        && let Some(mp) = message_pattern.as_mut()
    {
        mp.description = settings.msg_pattern_description.clone();
    }

    let mut options = LintOptions {
        message_pattern,
        body_policy: preset.body_policy,
        enforce_conventional_spec: enforce_spec,
        enforce_line_lengths,
        enforce_subject_style,
        ..Default::default()
    };

    let mut body_policy = preset.body_policy;
    let mut forbid_emojis = false;
    let mut forbid_non_ascii = false;
    let mut title_prefix_pattern: Option<String> = None;
    let mut title_prefix_separator = DEFAULT_TITLE_PREFIX_SEPARATOR.to_string();
    let mut title_suffix_pattern: Option<String> = None;
    let mut title_suffix_separator = DEFAULT_TITLE_SUFFIX_SEPARATOR.to_string();

    if let Some((_, cfg)) = config {
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
            if matches!(cfg.rules.single_line, Some(false)) && matches!(body_policy, BodyPolicy::SingleLine) {
                body_policy = BodyPolicy::Any;
            }
            if matches!(cfg.rules.require_body, Some(false)) && matches!(body_policy, BodyPolicy::RequireBody) {
                body_policy = BodyPolicy::Any;
            }
        }

        for exclude in &cfg.rules.excludes {
            options
                .exclude_rules
                .push(build_exclude_rule(&exclude.pattern, exclude.message.clone())?);
        }

        for cleanup in &cfg.rules.cleanup {
            options.cleanup_rules.push(build_cleanup_rule(
                &cleanup.find,
                &cleanup.replace,
                cleanup.description.clone(),
            )?);
        }
    }

    for exclude in &settings.excludes {
        options
            .exclude_rules
            .push(build_exclude_rule(&exclude.pattern, exclude.message.clone())?);
    }

    for cleanup in &settings.cleanups {
        options.cleanup_rules.push(build_cleanup_rule(
            &cleanup.find,
            &cleanup.replace,
            cleanup.description.clone(),
        )?);
    }

    if settings.single_line {
        body_policy = BodyPolicy::SingleLine;
    } else if settings.require_body {
        body_policy = BodyPolicy::RequireBody;
    }

    if settings.no_emojis {
        forbid_emojis = true;
    }
    if settings.ascii_only {
        forbid_non_ascii = true;
    }
    // Separators resolve independently of the patterns so that overriding only the pattern
    // keeps the separator configured in the config file.
    if let Some(pattern) = &settings.title_prefix {
        title_prefix_pattern = Some(pattern.clone());
    }
    if let Some(separator) = &settings.title_prefix_separator {
        title_prefix_separator = separator.clone();
    }
    if let Some(pattern) = &settings.title_suffix {
        title_suffix_pattern = Some(pattern.clone());
    }
    if let Some(separator) = &settings.title_suffix_separator {
        title_suffix_separator = separator.clone();
    }

    let write = settings.write || config.is_some_and(|(_, cfg)| cfg.write.unwrap_or(false));
    let exit_nonzero_on_rewrite = settings.exit_nonzero_on_rewrite
        || config.is_some_and(|(_, cfg)| cfg.rules.exit_nonzero_on_rewrite.unwrap_or(false));

    options.autofix = write;
    options.body_policy = body_policy;
    options.forbid_emojis = forbid_emojis;
    options.forbid_non_ascii = forbid_non_ascii;

    if let Some(pattern) = title_prefix_pattern.as_ref() {
        options.title_prefix = Some(build_title_prefix_rule(pattern, &title_prefix_separator)?);
    }

    if let Some(pattern) = title_suffix_pattern.as_ref() {
        options.title_suffix = Some(build_title_suffix_rule(pattern, &title_suffix_separator)?);
    }

    let ai_cleanup_enabled =
        !settings.no_ai_cleanup && config.and_then(|(_, cfg)| cfg.rules.ai_cleanup).unwrap_or(true);

    if ai_cleanup_enabled {
        push_builtin_ai_rules(&mut options)?;
    }

    Ok(ResolvedLint {
        options,
        write,
        exit_nonzero_on_rewrite,
        custom_pattern_notice,
    })
}

/// Resolves the configuration and lints `message` in one call.
///
/// # Errors
///
/// Returns an error under the same conditions as [`resolve_lint_options`].
pub fn lint_with_config(
    message: &str,
    config: Option<(&Path, &FileConfig)>,
    settings: &LintSettings,
) -> Result<LintOutcome> {
    let resolved = resolve_lint_options(config, settings)?;
    Ok(lint_message(message, &resolved.options))
}

/// Renders a toggle state for diagnostics.
fn on_off(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

/// Registers the built-in AI attribution detection and cleanup rules.
pub fn push_builtin_ai_rules(options: &mut LintOptions) -> Result<()> {
    for (pattern, message) in AI_EXCLUDE_RULES {
        options
            .exclude_rules
            .push(build_exclude_rule(pattern, Some((*message).to_string()))?);
    }

    for (find, replace, description) in AI_CLEANUP_RULES {
        options
            .cleanup_rules
            .push(build_cleanup_rule(find, replace, Some((*description).to_string()))?);
    }

    Ok(())
}

/// Returns `true` when the commit subject is one git generated itself, such as a revert or a
/// `fixup!`/`squash!` marker. Those subjects cannot satisfy a commit format and are rewritten
/// by git anyway once the rebase or revert completes.
pub fn has_generated_subject(message: &str) -> bool {
    let subject = message.lines().next().unwrap_or("");
    GENERATED_SUBJECT_PREFIXES
        .iter()
        .any(|prefix| subject.starts_with(prefix))
}

/// Returns `true` when git is mid-merge, mid-revert, mid-cherry-pick or mid-rebase in the
/// repository containing `start_dir`.
pub fn git_sequencer_in_progress(start_dir: &Path, message_path: &Path) -> bool {
    match find_git_dir(start_dir) {
        Some(git_dir) => {
            let pending_message = git_dir.join("COMMIT_EDITMSG").canonicalize();
            let input_message = message_path.canonicalize();
            matches!((pending_message, input_message), (Ok(pending), Ok(input)) if pending == input)
                && GIT_SEQUENCER_MARKERS.iter().any(|marker| git_dir.join(marker).exists())
        }
        None => false,
    }
}

/// Walks up from `start_dir` to the nearest git directory, resolving worktree `.git` files.
fn find_git_dir(start_dir: &Path) -> Option<PathBuf> {
    let mut current = start_dir;
    loop {
        let git_dir = current.join(".git");
        if git_dir.is_dir() {
            return Some(git_dir);
        }
        if git_dir.is_file() {
            return resolve_gitdir_file(&git_dir).ok();
        }
        current = current.parent()?;
    }
}

fn resolve_gitdir_file(git_file: &Path) -> Result<PathBuf> {
    let content =
        fs::read_to_string(git_file).with_context(|| format!("failed to read gitdir file {}", git_file.display()))?;
    let content = content.trim();

    let prefix = "gitdir:";
    if let Some(rest) = content.strip_prefix(prefix) {
        let raw = rest.trim();
        let path = Path::new(raw);
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
        Err(anyhow!("unexpected gitdir file format in {}", git_file.display()))
    }
}
