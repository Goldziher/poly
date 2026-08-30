//! Discovery- and result-filtering helpers used by the runner, split by the
//! question each answers: which *diagnostics* survive, which *paths* are
//! skipped, which *file contents* opt out of formatting, whether a file is
//! text at all, and which findings an in-source `poly: allow[…]` directive
//! suppresses. Kept out of `runner.rs` so orchestration stays one concern per
//! file.

mod binary;
mod diagnostics;
mod generated;
mod paths;
mod suppress;

pub(crate) use binary::is_binary;
pub(crate) use diagnostics::{PerFileIgnores, SeverityRemap};
pub(crate) use generated::{is_format_ignored, is_generated_source, is_hash_stamped_source};
pub(crate) use paths::{is_generated_lockfile, match_bases, relative_for_match};
pub(crate) use suppress::Suppressions;
