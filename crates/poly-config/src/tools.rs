//! `[tools]` — enabling and configuring vendored catalog tools (ADR 0013).
//!
//! poly ships a vendored catalog of standalone CLI tools (`poly-catalog`); the
//! `[tools]` table lets a user opt any of them in from `poly.toml` and tune how
//! it runs. Every entry is keyed by the tool's **catalog name** (e.g. `shfmt`,
//! `clang-format`):
//!
//! ```toml
//! [tools.shfmt]
//! enabled = true              # off by default — presence does not enable
//! command = "format"          # which catalog command to use ("" default)
//! args = ["-i", "2"]          # REPLACE the catalog command's argv entirely
//! stages = ["pre-commit"]     # git stages this tool runs in
//! files = "**/*.sh"           # include glob(s)
//! exclude = "**/vendor/**"    # exclude glob(s)
//! root = "packages/go"        # run from this subdirectory (relative to config root)
//!
//! [tools.shfmt.env]           # environment variables injected when running the tool
//! GOPATH = "/home/user/go"
//! ```
//!
//! Tools are **off by default**: a bare `[tools.<name>]` table with no
//! `enabled = true` is parsed but inert. Tool names are validated against
//! [`poly_catalog::Catalog`] via [`ToolsConfig::validate`] — an unknown name is
//! rejected (with a closest-match suggestion when one is near).

use std::collections::BTreeMap;

use poly_catalog::Catalog;
use serde::Deserialize;

use crate::hooks::{Patterns, Stage};

/// `[tools]` — a map from catalog tool name to its [`ToolConfig`].
///
/// Deserializes transparently from the `[tools.<name>]` tables. Empty by
/// default (no tools enabled).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct ToolsConfig(BTreeMap<String, ToolConfig>);

impl ToolsConfig {
    /// Look up a tool's configuration by its catalog name.
    pub fn get(&self, name: &str) -> Option<&ToolConfig> {
        self.0.get(name)
    }

    /// Iterate over the `(name, config)` pairs.
    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, String, ToolConfig> {
        self.0.iter()
    }

    /// Whether any tool is configured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of configured tools.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Validate every configured tool name against the vendored
    /// [`poly_catalog::Catalog`] (allowlist).
    ///
    /// Returns a human-readable error for the first name not present in the
    /// catalog, suggesting the closest known name when one is within a small
    /// edit distance.
    pub fn validate(&self) -> Result<(), String> {
        let catalog = Catalog::get();
        for name in self.0.keys() {
            if catalog.tool(name).is_none() {
                return Err(unknown_tool_message(name, catalog));
            }
        }
        Ok(())
    }
}

impl<'a> IntoIterator for &'a ToolsConfig {
    type Item = (&'a String, &'a ToolConfig);
    type IntoIter = std::collections::btree_map::Iter<'a, String, ToolConfig>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl From<BTreeMap<String, ToolConfig>> for ToolsConfig {
    fn from(map: BTreeMap<String, ToolConfig>) -> Self {
        ToolsConfig(map)
    }
}

/// Configuration for a single catalog tool under `[tools.<name>]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolConfig {
    /// Whether this tool is active. **Off by default** — declaring the table
    /// is not enough; `enabled = true` is required.
    pub enabled: bool,
    /// Which catalog command to invoke (`""` is the tool's default command,
    /// `"format"`, `"check"`, …). `None` lets poly pick by intent.
    pub command: Option<String>,
    /// When present, **replaces** the catalog command's argument vector
    /// entirely; when `None`, the catalog command's own argv is used.
    pub args: Option<Vec<String>>,
    /// Git stages this tool runs in; empty means it is not bound to a stage.
    pub stages: Vec<Stage>,
    /// File include glob(s); `None` matches every candidate file.
    pub files: Option<Patterns>,
    /// File exclude glob(s) filtered from the matched set before the tool runs.
    pub exclude: Option<Patterns>,
    /// Environment variables injected on top of the inherited environment when
    /// the tool runs. Applied on both the direct-engine path and the hooks-lowering
    /// path.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory the tool runs in, relative to the config/repo root.
    /// `None` means the repo root. Applied on both the direct-engine path and the
    /// hooks-lowering path.
    pub root: Option<String>,
}

/// Maximum edit distance at which a closest-match name is worth suggesting.
const SUGGESTION_MAX_DISTANCE: usize = 3;

/// Build the "unknown tool" error, appending a closest-match suggestion when a
/// catalog name is within [`SUGGESTION_MAX_DISTANCE`] of `name`.
fn unknown_tool_message(name: &str, catalog: &Catalog) -> String {
    let suggestion = catalog
        .tools()
        .iter()
        .map(|tool| (tool.name.as_str(), levenshtein(name, &tool.name)))
        .filter(|(_, distance)| *distance <= SUGGESTION_MAX_DISTANCE)
        .min_by_key(|(_, distance)| *distance)
        .map(|(candidate, _)| candidate);
    match suggestion {
        Some(candidate) => format!(
            "unknown tool `{name}` in [tools]; did you mean `{candidate}`? \
             (no such tool in the poly catalog)"
        ),
        None => format!("unknown tool `{name}` in [tools]; no such tool in the poly catalog"),
    }
}

/// Classic two-row Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0usize; b_chars.len() + 1];
    for (i, a_char) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, &b_char) in b_chars.iter().enumerate() {
            let cost = usize::from(a_char != b_char);
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> ToolsConfig {
        toml::from_str(toml).expect("parse [tools]")
    }

    #[test]
    fn should_parse_full_and_minimal_tool_entries() {
        let tools = parse(
            r#"
[shfmt]
enabled = true
command = "format"
args = ["-i", "2"]
stages = ["pre-commit"]
files = "**/*.sh"
exclude = "**/vendor/**"

[clang-format]
enabled = true
"#,
        );

        let shfmt = tools.get("shfmt").expect("shfmt entry present");
        assert!(shfmt.enabled);
        assert_eq!(shfmt.command.as_deref(), Some("format"));
        assert_eq!(shfmt.args.as_deref(), Some(&["-i".to_string(), "2".to_string()][..]));
        assert_eq!(shfmt.stages, vec![Stage::PreCommit]);
        assert_eq!(
            shfmt.files.as_ref().map(Patterns::as_slice),
            Some(&["**/*.sh".to_string()][..])
        );
        assert_eq!(
            shfmt.exclude.as_ref().map(Patterns::as_slice),
            Some(&["**/vendor/**".to_string()][..])
        );

        let clang = tools.get("clang-format").expect("clang-format entry present");
        assert!(clang.enabled);
        assert_eq!(clang.command, None);
        assert_eq!(clang.args, None);
        assert!(clang.stages.is_empty());
        assert!(clang.files.is_none());
        assert!(clang.exclude.is_none());

        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn should_default_enabled_to_false_for_bare_table() {
        let tools = parse("[shfmt]\n");
        let shfmt = tools.get("shfmt").expect("shfmt entry present");
        assert!(!shfmt.enabled, "tools are off until enabled = true");
    }

    #[test]
    fn should_capture_args_override_as_some_and_absent_as_none() {
        let tools = parse(
            r#"
[shfmt]
enabled = true
args = ["--flag", "value"]

[gofmt]
enabled = true
"#,
        );
        assert_eq!(
            tools.get("shfmt").unwrap().args.as_deref(),
            Some(&["--flag".to_string(), "value".to_string()][..])
        );
        assert_eq!(tools.get("gofmt").unwrap().args, None);
    }

    #[test]
    fn should_accept_known_catalog_tool_names() {
        let tools = parse(
            r#"
[shfmt]
enabled = true
[clang-format]
enabled = true
"#,
        );
        tools.validate().expect("known tools must validate");
    }

    #[test]
    fn should_reject_unknown_tool_name() {
        let tools = parse("[definitely-not-a-tool]\nenabled = true\n");
        let error = tools.validate().unwrap_err();
        assert!(
            error.contains("definitely-not-a-tool"),
            "names the offending tool: {error}"
        );
        assert!(error.contains("no such tool"), "explains the rejection: {error}");
    }

    #[test]
    fn should_suggest_closest_name_for_near_miss() {
        let tools = parse("[shfmtt]\nenabled = true\n");
        let error = tools.validate().unwrap_err();
        assert!(
            error.contains("did you mean `shfmt`"),
            "suggests the closest catalog name: {error}"
        );
    }

    #[test]
    fn should_reject_unknown_keys_in_tool_table() {
        let result: Result<ToolsConfig, _> = toml::from_str("[shfmt]\nbogus = true\n");
        assert!(result.is_err(), "deny_unknown_fields must reject `bogus`");
    }

    #[test]
    fn empty_tools_config_validates() {
        let tools = ToolsConfig::default();
        assert!(tools.is_empty());
        tools.validate().expect("empty config is valid");
    }

    #[test]
    fn should_parse_env_vars_and_root() {
        let tools = parse(
            r#"
[shfmt]
enabled = true
root = "packages/shell"

[shfmt.env]
MYVAR = "hello"
OTHER = "world"
"#,
        );
        let shfmt = tools.get("shfmt").expect("shfmt entry present");
        assert!(shfmt.enabled);
        assert_eq!(shfmt.root.as_deref(), Some("packages/shell"));
        assert_eq!(shfmt.env.get("MYVAR").map(String::as_str), Some("hello"));
        assert_eq!(shfmt.env.get("OTHER").map(String::as_str), Some("world"));
    }

    #[test]
    fn should_default_env_to_empty_and_root_to_none() {
        let tools = parse("[shfmt]\nenabled = true\n");
        let shfmt = tools.get("shfmt").expect("shfmt entry present");
        assert!(shfmt.env.is_empty(), "env defaults to empty BTreeMap");
        assert!(shfmt.root.is_none(), "root defaults to None");
    }
}
