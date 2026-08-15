//! Discovery- and result-filtering helpers used by the runner, split by the
//! question each answers: which *diagnostics* survive, which *paths* are
//! skipped, and which *file contents* opt out of formatting. Kept out of
//! `runner.rs` so orchestration stays one concern per file.

mod diagnostics;
mod generated;
mod paths;

pub(crate) use diagnostics::{PerFileIgnores, SeverityRemap};
pub(crate) use generated::{is_format_ignored, is_generated_source, is_hash_stamped_source};
pub(crate) use paths::{is_generated_lockfile, match_bases, relative_for_match};
