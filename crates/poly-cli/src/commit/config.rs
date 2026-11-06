//! Loading the commit-lint configuration: gitfluff's own files first, then the `[commit]`
//! table of the nearest `poly.toml`.

use std::path::Path;

use anyhow::Context;
use gitfluff::ConfigSource;
use gitfluff::config::{CleanupRuleConfig, ExcludeRuleConfig, FileConfig, MessageRuleConfig, RulesConfig};
use poly_config::BaseConfigResolver;

/// Load the commit config governing `start_dir`.
///
/// An explicit path or a gitfluff-native file (`.gitfluff.toml` / `.fluff.toml`) takes
/// precedence; otherwise the `[commit]` table of the nearest `poly.toml` is mapped onto
/// gitfluff's [`FileConfig`]. Returns `None` when neither exists.
///
/// `resolver` resolves the `extends` bases a `poly.toml` declares, so a repo whose shared base
/// lives in a pinned remote (not a local sibling path) still loads its `[commit]` rules.
pub(super) fn load(
    explicit_path: Option<&Path>,
    start_dir: &Path,
    resolver: &dyn BaseConfigResolver,
) -> anyhow::Result<Option<(ConfigSource, FileConfig)>> {
    if let Some((path, config)) = gitfluff::load_config(explicit_path, start_dir)? {
        return Ok(Some((ConfigSource::new(path), config)));
    }
    load_poly_commit_config(start_dir, resolver)
}

fn load_poly_commit_config(
    start_dir: &Path,
    resolver: &dyn BaseConfigResolver,
) -> anyhow::Result<Option<(ConfigSource, FileConfig)>> {
    match poly_config::find_config(start_dir) {
        Some(poly_path) => {
            let poly = poly_config::PolyConfig::load_file_with(&poly_path, resolver)
                .with_context(|| format!("invalid config at {}", poly_path.display()))?;
            let config = file_config(poly.commit);
            Ok(Some((
                ConfigSource::new(poly_path).with_rules_table("commit.rules"),
                config,
            )))
        }
        None => Ok(None),
    }
}

/// `poly.toml` has no key for the AI-trailer toggle or the message-rule length/case switches,
/// so those stay at gitfluff's defaults (AI cleanup on, spec length/case checks off under a
/// custom `message` pattern).
fn file_config(commit: poly_config::CommitConfig) -> FileConfig {
    FileConfig {
        preset: commit.preset,
        write: commit.write,
        rules: rules_config(commit.rules),
    }
}

fn rules_config(rules: poly_config::CommitRules) -> RulesConfig {
    RulesConfig {
        message: rules.message.map(|rule| MessageRuleConfig {
            pattern: rule.pattern,
            description: rule.description,
            enforce_length: None,
            enforce_case: None,
        }),
        excludes: rules
            .excludes
            .into_iter()
            .map(|rule| ExcludeRuleConfig {
                pattern: rule.pattern,
                message: rule.message,
            })
            .collect(),
        cleanup: rules
            .cleanup
            .into_iter()
            .map(|rule| CleanupRuleConfig {
                find: rule.find,
                replace: rule.replace,
                description: rule.description,
            })
            .collect(),
        single_line: rules.single_line,
        require_body: rules.require_body,
        exit_nonzero_on_rewrite: rules.exit_nonzero_on_rewrite,
        no_emojis: rules.no_emojis,
        ascii_only: rules.ascii_only,
        title_prefix: rules.title_prefix,
        title_prefix_separator: rules.title_prefix_separator,
        title_suffix: rules.title_suffix,
        title_suffix_separator: rules.title_suffix_separator,
        ai_cleanup: None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use poly_config::LocalPathResolver;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn loads_commit_section_from_poly_toml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("poly.toml"),
            r#"
[lint.python.ruff]
select = ["E"]

[commit]
preset = "conventional"
[commit.rules]
require_body = true
no_emojis = true
[[commit.rules.excludes]]
pattern = "^WIP"
"#,
        )
        .unwrap();

        let (path, config) = load(None, dir.path(), &LocalPathResolver)
            .expect("load")
            .expect("config found via poly.toml");
        assert!(path.path.ends_with("poly.toml"));
        assert_eq!(path.rules_table, "commit.rules");
        assert_eq!(config.preset.as_deref(), Some("conventional"));
        assert_eq!(config.rules.require_body, Some(true));
        assert_eq!(config.rules.no_emojis, Some(true));
        assert_eq!(config.rules.excludes.len(), 1);
        assert_eq!(config.rules.excludes[0].pattern, "^WIP");
    }

    #[test]
    fn gitfluff_native_config_wins_over_poly_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("poly.toml"), "[commit]\npreset = \"conventional\"\n").unwrap();
        fs::write(dir.path().join(".gitfluff.toml"), "preset = \"angular\"\n").unwrap();

        let (path, config) = load(None, dir.path(), &LocalPathResolver)
            .expect("load")
            .expect("config found");
        assert!(
            path.path.ends_with(".gitfluff.toml"),
            "gitfluff-native config should take precedence"
        );
        assert_eq!(config.preset.as_deref(), Some("angular"));
    }

    #[test]
    fn no_config_returns_none() {
        let dir = tempdir().unwrap();
        assert!(load(None, dir.path(), &LocalPathResolver).expect("load").is_none());
    }

    #[test]
    fn poly_toml_message_rule_keeps_spec_length_and_case_checks_off() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("poly.toml"),
            "[commit.rules.message]\npattern = \"^X\"\n[commit.rules]\nsingle_line = true\n",
        )
        .unwrap();

        let (_, config) = load(None, dir.path(), &LocalPathResolver).unwrap().unwrap();
        let message = config.rules.message.expect("message rule");
        assert_eq!(message.pattern, "^X");
        assert_eq!((message.enforce_length, message.enforce_case), (None, None));
        assert_eq!(config.rules.ai_cleanup, None);
        assert_eq!(config.rules.single_line, Some(true));
    }
}
