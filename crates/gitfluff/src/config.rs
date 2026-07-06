use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct FileConfig {
    pub preset: Option<String>,
    pub write: Option<bool>,
    pub rules: RulesConfig,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct RulesConfig {
    pub message: Option<MessageRuleConfig>,
    pub excludes: Vec<ExcludeRuleConfig>,
    pub cleanup: Vec<CleanupRuleConfig>,
    pub single_line: Option<bool>,
    pub require_body: Option<bool>,
    pub exit_nonzero_on_rewrite: Option<bool>,
    pub no_emojis: Option<bool>,
    pub ascii_only: Option<bool>,
    pub title_prefix: Option<String>,
    pub title_prefix_separator: Option<String>,
    pub title_suffix: Option<String>,
    pub title_suffix_separator: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MessageRuleConfig {
    pub pattern: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExcludeRuleConfig {
    pub pattern: String,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CleanupRuleConfig {
    pub find: String,
    pub replace: String,
    pub description: Option<String>,
}

pub fn load_config(explicit_path: Option<&Path>, start_dir: &Path) -> Result<Option<(PathBuf, FileConfig)>> {
    let path = match explicit_path {
        Some(p) => p.to_path_buf(),
        None => match find_config(start_dir) {
            Some(p) => p,
            // No gitfluff-native config: fall back to the unified `poly.toml`
            // `[commit]` table so a repo using the poly umbrella needs only one
            // config file.
            None => return load_poly_commit_config(start_dir),
        },
    };

    let content = fs::read_to_string(&path).with_context(|| format!("failed to read config at {}", path.display()))?;
    let config: FileConfig =
        toml::from_str(&content).with_context(|| format!("invalid config at {}", path.display()))?;
    Ok(Some((path, config)))
}

/// Load the `[commit]` table from the nearest `poly.toml`,
/// mapped onto gitfluff's [`FileConfig`]. Returns `None` when no such file
/// exists. Gitfluff-native files (`.gitfluff.toml` / `.fluff.toml`) take
/// precedence over this path.
fn load_poly_commit_config(start_dir: &Path) -> Result<Option<(PathBuf, FileConfig)>> {
    match poly_config::find_config(start_dir) {
        Some(poly_path) => {
            let poly = poly_config::PolyConfig::load_file(&poly_path)
                .with_context(|| format!("invalid config at {}", poly_path.display()))?;
            Ok(Some((poly_path, FileConfig::from(poly.commit))))
        }
        None => Ok(None),
    }
}

impl From<poly_config::CommitConfig> for FileConfig {
    fn from(commit: poly_config::CommitConfig) -> Self {
        FileConfig {
            preset: commit.preset,
            write: commit.write,
            rules: commit.rules.into(),
        }
    }
}

impl From<poly_config::CommitRules> for RulesConfig {
    fn from(rules: poly_config::CommitRules) -> Self {
        RulesConfig {
            message: rules.message.map(Into::into),
            excludes: rules.excludes.into_iter().map(Into::into).collect(),
            cleanup: rules.cleanup.into_iter().map(Into::into).collect(),
            single_line: rules.single_line,
            require_body: rules.require_body,
            exit_nonzero_on_rewrite: rules.exit_nonzero_on_rewrite,
            no_emojis: rules.no_emojis,
            ascii_only: rules.ascii_only,
            title_prefix: rules.title_prefix,
            title_prefix_separator: rules.title_prefix_separator,
            title_suffix: rules.title_suffix,
            title_suffix_separator: rules.title_suffix_separator,
        }
    }
}

impl From<poly_config::MessageRule> for MessageRuleConfig {
    fn from(rule: poly_config::MessageRule) -> Self {
        MessageRuleConfig {
            pattern: rule.pattern,
            description: rule.description,
        }
    }
}

impl From<poly_config::ExcludeRule> for ExcludeRuleConfig {
    fn from(rule: poly_config::ExcludeRule) -> Self {
        ExcludeRuleConfig {
            pattern: rule.pattern,
            message: rule.message,
        }
    }
}

impl From<poly_config::CleanupRule> for CleanupRuleConfig {
    fn from(rule: poly_config::CleanupRule) -> Self {
        CleanupRuleConfig {
            find: rule.find,
            replace: rule.replace,
            description: rule.description,
        }
    }
}

fn find_config(start_dir: &Path) -> Option<PathBuf> {
    let mut current = start_dir;
    loop {
        for name in [".gitfluff.toml", ".fluff.toml"] {
            let candidate = current.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

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

        let (path, config) = load_config(None, dir.path())
            .expect("load")
            .expect("config found via poly.toml");
        assert!(path.ends_with("poly.toml"));
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

        let (path, config) = load_config(None, dir.path()).expect("load").expect("config found");
        assert!(
            path.ends_with(".gitfluff.toml"),
            "gitfluff-native config should take precedence"
        );
        assert_eq!(config.preset.as_deref(), Some("angular"));
    }

    #[test]
    fn no_config_returns_none() {
        let dir = tempdir().unwrap();
        assert!(load_config(None, dir.path()).expect("load").is_none());
    }
}
