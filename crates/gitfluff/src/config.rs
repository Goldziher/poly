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

pub fn load_config(
    explicit_path: Option<&Path>,
    start_dir: &Path,
) -> Result<Option<(PathBuf, FileConfig)>> {
    let path = match explicit_path {
        Some(p) => p.to_path_buf(),
        None => match find_config(start_dir) {
            Some(p) => p,
            None => return Ok(None),
        },
    };

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    let config: FileConfig = toml::from_str(&content)
        .with_context(|| format!("invalid config at {}", path.display()))?;
    Ok(Some((path, config)))
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
