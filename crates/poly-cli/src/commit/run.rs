//! The `poly commit` flow: map the command line onto gitfluff's library runner and exit code.

use std::io::{self, Write};

use anyhow::{Result, anyhow};
use gitfluff::LintSettings;
use gitfluff::config::CleanupRuleConfig;
use gitfluff::run::{ColorChoice as GitfluffColor, MessageInput, RunIo, RunOptions, run_lint_with};
use poly_config::BaseConfigResolver;

use super::args::CommitArgs;
use super::config;
use crate::global::ColorChoice;

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
    let explicit = args.config.as_deref();
    let options = run_options(&args)?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut diagnostics = Vec::new();
    let code = {
        let mut io = RunIo {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut diagnostics,
        };
        run_lint_with(&options, &mut io, |cwd| config::load(explicit, cwd, resolver))?
    };
    let mut stderr = io::stderr().lock();
    stderr.write_all(&diagnostics)?;
    Ok(code)
}

const NO_SOURCE: &str = "no commit message source provided (pass COMMIT_FILE, --from-file, --stdin, or --message)";

fn run_options(args: &CommitArgs) -> Result<RunOptions> {
    let input = if let Some(path) = args.from_file.as_ref().or(args.commit_file.as_ref()) {
        MessageInput::File(path.clone())
    } else if args.stdin {
        MessageInput::Stdin
    } else if let Some(message) = &args.message {
        MessageInput::Literal(message.clone())
    } else {
        return Err(anyhow!(NO_SOURCE));
    };

    let mut options = RunOptions::new(input);
    options.settings = settings_from_args(args);
    options.exclude_args.clone_from(&args.exclude);
    options.cleanup_args.clone_from(&args.cleanup);
    options.cleanup_pattern = args.cleanup_pattern.as_ref().map(|find| CleanupRuleConfig {
        find: find.clone(),
        replace: args.cleanup_replacement.clone().unwrap_or_default(),
        description: args.cleanup_description.clone(),
    });
    options.color = match args.color {
        ColorChoice::Auto => GitfluffColor::Auto,
        ColorChoice::Always => GitfluffColor::Always,
        ColorChoice::Never => GitfluffColor::Never,
    };
    Ok(options)
}

/// Translate the command-line overrides into gitfluff's [`LintSettings`].
fn settings_from_args(args: &CommitArgs) -> LintSettings {
    LintSettings {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_override_the_config_only_with_their_pattern() {
        let mut args = blank_args();
        let settings = settings_from_args(&args);
        assert_eq!(settings.title_prefix_separator, None);
        assert_eq!(settings.title_suffix_separator, None);

        args.title_prefix = Some("^\\w+".into());
        let settings = settings_from_args(&args);
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
