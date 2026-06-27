//! `[commit]` — configuration for `poly commit` (the gitfluff-backed
//! Conventional-Commit linter + cleaner).
//!
//! The shape mirrors gitfluff's own `FileConfig` so a `[commit]` table in
//! `poly.toml` maps one-to-one onto gitfluff's in-process linter, letting the
//! standalone `.gitfluff.toml` and the unified `poly.toml` share one schema.

use serde::Deserialize;

/// `[commit]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CommitConfig {
    /// Named preset to seed the rules from (e.g. `"conventional"`).
    pub preset: Option<String>,
    /// Whether the cleaner rewrites the commit message in place.
    pub write: Option<bool>,
    /// Individual lint/cleanup rules.
    pub rules: CommitRules,
}

/// `[commit.rules]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CommitRules {
    /// Require the subject to match a pattern.
    pub message: Option<MessageRule>,
    /// Subjects matching any of these patterns skip linting (e.g. `^WIP`).
    pub excludes: Vec<ExcludeRule>,
    /// Find/replace cleanups applied to the message body.
    pub cleanup: Vec<CleanupRule>,
    /// Reject multi-line subjects.
    pub single_line: Option<bool>,
    /// Require a non-empty body.
    pub require_body: Option<bool>,
    /// Exit non-zero when the cleaner rewrote the message.
    pub exit_nonzero_on_rewrite: Option<bool>,
    /// Reject emojis in the message.
    pub no_emojis: Option<bool>,
    /// Require ASCII-only content.
    pub ascii_only: Option<bool>,
    /// Required subject prefix.
    pub title_prefix: Option<String>,
    /// Separator between the title prefix and the subject.
    pub title_prefix_separator: Option<String>,
    /// Required subject suffix.
    pub title_suffix: Option<String>,
    /// Separator between the subject and the title suffix.
    pub title_suffix_separator: Option<String>,
}

/// A subject-pattern requirement (`[commit.rules.message]`).
#[derive(Debug, Clone, Deserialize)]
pub struct MessageRule {
    /// Regular expression the subject must match.
    pub pattern: String,
    /// Human-readable description shown when the rule fails.
    pub description: Option<String>,
}

/// A subject-pattern exclusion (`[[commit.rules.excludes]]`).
#[derive(Debug, Clone, Deserialize)]
pub struct ExcludeRule {
    /// Regular expression; matching subjects skip linting.
    pub pattern: String,
    /// Optional note explaining the exclusion.
    pub message: Option<String>,
}

/// A find/replace cleanup (`[[commit.rules.cleanup]]`).
#[derive(Debug, Clone, Deserialize)]
pub struct CleanupRule {
    /// Regular expression to find.
    pub find: String,
    /// Replacement text.
    pub replace: String,
    /// Human-readable description of the cleanup.
    pub description: Option<String>,
}
