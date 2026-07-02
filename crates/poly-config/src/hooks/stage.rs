//! The canonical git hook [`Stage`] enum used across `poly hooks`.
//!
//! Every `[hooks.<stage>]` key in `poly.toml` must name a valid stage. The
//! variants cover the full git lifecycle plus the `manual` and `always`
//! pseudo-stages that pre-commit and lefthook expose. Parsing accepts the
//! legacy aliases pre-commit understands (`commit` → `pre-commit`,
//! `push` → `pre-push`, `merge-commit` → `pre-merge-commit`).
//
// parity with poly-hooks::Stage asserted in poly-cli (WS-B3)

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A git hook stage (lifecycle point at which hooks run).
///
/// Serializes to its canonical kebab-case name and parses from that name or a
/// recognized alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// `pre-commit` (alias `commit`).
    PreCommit,
    /// `pre-merge-commit` (alias `merge-commit`).
    PreMergeCommit,
    /// `prepare-commit-msg`.
    PrepareCommitMsg,
    /// `commit-msg`.
    CommitMsg,
    /// `post-commit`.
    PostCommit,
    /// `pre-rebase`.
    PreRebase,
    /// `post-checkout`.
    PostCheckout,
    /// `post-merge`.
    PostMerge,
    /// `pre-push` (alias `push`).
    PrePush,
    /// `post-rewrite`.
    PostRewrite,
    /// `manual` — run only when explicitly requested, never by a git hook.
    Manual,
    /// `always` — run at every stage.
    Always,
}

impl Stage {
    /// Every stage variant, in canonical lifecycle order.
    pub const ALL: &'static [Stage] = &[
        Stage::PreCommit,
        Stage::PreMergeCommit,
        Stage::PrepareCommitMsg,
        Stage::CommitMsg,
        Stage::PostCommit,
        Stage::PreRebase,
        Stage::PostCheckout,
        Stage::PostMerge,
        Stage::PrePush,
        Stage::PostRewrite,
        Stage::Manual,
        Stage::Always,
    ];

    /// The canonical kebab-case name for this stage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::PreCommit => "pre-commit",
            Stage::PreMergeCommit => "pre-merge-commit",
            Stage::PrepareCommitMsg => "prepare-commit-msg",
            Stage::CommitMsg => "commit-msg",
            Stage::PostCommit => "post-commit",
            Stage::PreRebase => "pre-rebase",
            Stage::PostCheckout => "post-checkout",
            Stage::PostMerge => "post-merge",
            Stage::PrePush => "pre-push",
            Stage::PostRewrite => "post-rewrite",
            Stage::Manual => "manual",
            Stage::Always => "always",
        }
    }

    /// A comma-separated list of all canonical stage names, for error messages.
    pub fn all_names() -> String {
        Stage::ALL
            .iter()
            .map(|stage| stage.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string does not name a known git hook stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseStageError(String);

impl fmt::Display for ParseStageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown git hook stage `{}`; expected one of: {}",
            self.0,
            Stage::all_names()
        )
    }
}

impl std::error::Error for ParseStageError {}

impl FromStr for Stage {
    type Err = ParseStageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let stage = match value {
            "pre-commit" | "commit" => Stage::PreCommit,
            "pre-merge-commit" | "merge-commit" => Stage::PreMergeCommit,
            "prepare-commit-msg" => Stage::PrepareCommitMsg,
            "commit-msg" => Stage::CommitMsg,
            "post-commit" => Stage::PostCommit,
            "pre-rebase" => Stage::PreRebase,
            "post-checkout" => Stage::PostCheckout,
            "post-merge" => Stage::PostMerge,
            "pre-push" | "push" => Stage::PrePush,
            "post-rewrite" => Stage::PostRewrite,
            "manual" => Stage::Manual,
            "always" => Stage::Always,
            other => return Err(ParseStageError(other.to_string())),
        };
        Ok(stage)
    }
}

impl Serialize for Stage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Stage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_canonical_name() {
        for stage in Stage::ALL {
            let name = stage.as_str();
            let parsed: Stage = name.parse().expect("canonical name parses");
            assert_eq!(parsed, *stage, "round-trip failed for {name}");
            assert_eq!(parsed.to_string(), name, "Display must equal as_str");
        }
    }

    #[test]
    fn accepts_legacy_aliases() {
        assert_eq!("commit".parse::<Stage>().unwrap(), Stage::PreCommit);
        assert_eq!("push".parse::<Stage>().unwrap(), Stage::PrePush);
        assert_eq!("merge-commit".parse::<Stage>().unwrap(), Stage::PreMergeCommit);
    }

    #[test]
    fn rejects_unknown_stage_with_listing() {
        let error = "bogus".parse::<Stage>().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("bogus"), "names the offending key");
        assert!(message.contains("pre-commit"), "lists valid stages");
    }

    #[test]
    fn all_contains_ten_git_stages_plus_manual_and_always() {
        assert_eq!(Stage::ALL.len(), 12);
    }
}
