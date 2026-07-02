//! Git hook stage enumeration and its mapping from `HookType`.
//!
//! Ported from `polyhooks/src/config.rs`. The `Stage` enum covers every git
//! hook stage; `HookType` is the clap-visible alias used on the CLI
//! (`hook-impl --hook-type=pre-commit`). Both map 1-to-1 via `From<HookType>`.

use serde::{Deserialize, Serialize};

/// A git hook stage, used both as a config key and a runtime discriminant.
///
/// Serialised as `kebab-case`; common aliases (`commit`, `push`, `merge-commit`)
/// are accepted for backward compatibility with pre-commit tooling.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Hash,
    Deserialize,
    Serialize,
    clap::ValueEnum,
    strum::AsRefStr,
    strum::Display,
    strum::EnumCount,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
#[repr(u8)]
#[non_exhaustive]
pub enum Stage {
    /// Run manually via `poly hooks run --hook-type=manual`.
    Manual,
    /// `commit-msg` git hook.
    CommitMsg,
    /// `post-checkout` git hook.
    PostCheckout,
    /// `post-commit` git hook.
    PostCommit,
    /// `post-merge` git hook.
    PostMerge,
    /// `post-rewrite` git hook.
    PostRewrite,
    /// `pre-commit` git hook (the default).
    #[default]
    #[serde(alias = "commit")]
    PreCommit,
    /// `pre-merge-commit` git hook.
    #[serde(alias = "merge-commit")]
    PreMergeCommit,
    /// `pre-push` git hook.
    #[serde(alias = "push")]
    PrePush,
    /// `pre-rebase` git hook.
    PreRebase,
    /// `prepare-commit-msg` git hook.
    PrepareCommitMsg,
}

impl Stage {
    /// Total number of `Stage` variants — must match `StageCount::COUNT`.
    pub const STAGE_COUNT: usize = 11;

    /// All stages in their canonical order.
    const ORDER: [Self; Self::STAGE_COUNT] = [
        Self::Manual,
        Self::CommitMsg,
        Self::PostCheckout,
        Self::PostCommit,
        Self::PostMerge,
        Self::PostRewrite,
        Self::PreCommit,
        Self::PreMergeCommit,
        Self::PrePush,
        Self::PreRebase,
        Self::PrepareCommitMsg,
    ];

    /// Return the bitmask for this stage (used by B1 stage-set tracking).
    #[allow(dead_code)] // used in B1 hook-runner phase
    pub(crate) const fn bit(self) -> u16 {
        1u16 << (self as u8)
    }

    /// Look up a stage by its index in `ORDER`.
    #[allow(dead_code)] // used in B1 hook-runner phase
    fn from_index(index: u32) -> Self {
        Self::ORDER[index as usize]
    }
}

/// The hook-type argument accepted by `poly hooks hook-impl --hook-type=<value>`.
///
/// Every variant maps 1-to-1 to a [`Stage`] (except `Manual`, which is not
/// triggered by a git hook and therefore has no `HookType` counterpart).
#[derive(Debug, Clone, Copy, Default, Deserialize, clap::ValueEnum, strum::AsRefStr, strum::Display)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum HookType {
    /// `commit-msg` git hook.
    CommitMsg,
    /// `post-checkout` git hook.
    PostCheckout,
    /// `post-commit` git hook.
    PostCommit,
    /// `post-merge` git hook.
    PostMerge,
    /// `post-rewrite` git hook.
    PostRewrite,
    /// `pre-commit` git hook (default).
    #[default]
    PreCommit,
    /// `pre-merge-commit` git hook.
    PreMergeCommit,
    /// `pre-push` git hook.
    PrePush,
    /// `pre-rebase` git hook.
    PreRebase,
    /// `prepare-commit-msg` git hook.
    PrepareCommitMsg,
}

impl From<HookType> for Stage {
    fn from(value: HookType) -> Self {
        match value {
            HookType::CommitMsg => Self::CommitMsg,
            HookType::PostCheckout => Self::PostCheckout,
            HookType::PostCommit => Self::PostCommit,
            HookType::PostMerge => Self::PostMerge,
            HookType::PostRewrite => Self::PostRewrite,
            HookType::PreCommit => Self::PreCommit,
            HookType::PreMergeCommit => Self::PreMergeCommit,
            HookType::PrePush => Self::PrePush,
            HookType::PreRebase => Self::PreRebase,
            HookType::PrepareCommitMsg => Self::PrepareCommitMsg,
        }
    }
}

/// Whether a stage receives filenames, a message-file path, or no files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunInputMode {
    /// Hooks receive a list of matched files (e.g. `pre-commit`).
    #[default]
    Files,
    /// Hooks receive the path to the commit-message file (e.g. `commit-msg`).
    MessageFile,
    /// Hooks receive no files (e.g. `post-commit`).
    NoFiles,
}

impl From<Stage> for RunInputMode {
    fn from(stage: Stage) -> Self {
        match stage {
            Stage::CommitMsg | Stage::PrepareCommitMsg => Self::MessageFile,
            Stage::Manual | Stage::PreCommit | Stage::PreMergeCommit | Stage::PrePush => Self::Files,
            Stage::PostCheckout | Stage::PostCommit | Stage::PostMerge | Stage::PostRewrite | Stage::PreRebase => {
                Self::NoFiles
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_type_maps_to_matching_stage() {
        assert_eq!(Stage::from(HookType::PreCommit), Stage::PreCommit);
        assert_eq!(Stage::from(HookType::CommitMsg), Stage::CommitMsg);
        assert_eq!(Stage::from(HookType::PrePush), Stage::PrePush);
        assert_eq!(Stage::from(HookType::PostCheckout), Stage::PostCheckout);
    }

    #[test]
    fn stage_run_input_mode() {
        assert_eq!(RunInputMode::from(Stage::PreCommit), RunInputMode::Files);
        assert_eq!(RunInputMode::from(Stage::CommitMsg), RunInputMode::MessageFile);
        assert_eq!(RunInputMode::from(Stage::PostCommit), RunInputMode::NoFiles);
    }

    #[test]
    fn stage_order_length_matches_count() {
        use strum::EnumCount as _;
        assert_eq!(Stage::ORDER.len(), Stage::COUNT);
        assert_eq!(Stage::COUNT, Stage::STAGE_COUNT);
    }
}
