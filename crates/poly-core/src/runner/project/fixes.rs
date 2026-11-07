//! `--fix` for project findings: the edits an engine attached are committed to the file they
//! belong to, the same way the per-file tier commits its own.
//!
//! A project check ran over the contents the host loaded, so its edits are byte offsets into
//! exactly those bytes. They are applied only while the file on disk still equals what was loaded;
//! a file that changed underneath (an editor, a formatter) is left alone and its findings stay.

use std::path::Path;

use super::super::edits::apply_edits_indexed;
use super::super::write::write_atomic;
use super::FixMode;
use crate::engine::Diagnostic;
use crate::filter::is_hash_stamped_source;

/// What `--fix` did to one file's project findings.
#[derive(Debug, Default)]
pub(super) struct Applied {
    /// Findings resolved by a committed edit group (and so removed from the report).
    pub(super) fixed: usize,
    /// `--fix` was requested but withheld because the file is hash-stamped as generated.
    pub(super) withheld_generated: bool,
}

/// Commit the fixes of `diagnostics` to `path` when `mode` asks for it, and drop the findings they
/// resolved. Without `--fix` the edits are cleared, so a report never advertises a fix that was not
/// offered; under `--fix` an edit that could not be applied is cleared too, for the same reason.
pub(super) fn resolve(path: &Path, loaded: Option<&str>, diagnostics: &mut Vec<Diagnostic>, mode: FixMode) -> Applied {
    let mut applied = Applied::default();
    if mode.apply
        && let Some(loaded) = loaded
        && diagnostics.iter().any(|d| !d.fix.is_empty())
    {
        if !mode.generated && is_hash_stamped_source(loaded) {
            tracing::debug!(path = %path.display(), "skipping --fix on generated file");
            applied.withheld_generated = true;
        } else {
            applied.fixed = commit(path, loaded, diagnostics);
        }
    }
    for diagnostic in diagnostics.iter_mut() {
        diagnostic.fix.clear();
    }
    applied
}

/// Apply every fix group to `path` in one write and remove the findings whose group was committed.
fn commit(path: &Path, loaded: &str, diagnostics: &mut Vec<Diagnostic>) -> usize {
    let fixable: Vec<usize> = (0..diagnostics.len())
        .filter(|&i| !diagnostics[i].fix.is_empty())
        .collect();
    let groups: Vec<&[crate::engine::Edit]> = fixable.iter().map(|&i| diagnostics[i].fix.as_slice()).collect();
    match std::fs::read_to_string(path) {
        Ok(now) if now == loaded => {}
        _ => {
            tracing::warn!(path = %path.display(), "project fix skipped: the file changed since it was checked");
            return 0;
        }
    }
    let Some((rewritten, taken)) = apply_edits_indexed(loaded, &groups) else {
        return 0;
    };
    if let Err(error) = write_atomic(path, &rewritten) {
        tracing::warn!(path = %path.display(), %error, "project fix not written");
        return 0;
    }
    let resolved: Vec<usize> = taken.into_iter().map(|group| fixable[group]).collect();
    let mut index = 0usize;
    diagnostics.retain(|_| {
        let keep = !resolved.contains(&index);
        index += 1;
        keep
    });
    resolved.len()
}
