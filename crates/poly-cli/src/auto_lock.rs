//! Lock what an installing command needs before it reads `poly.lock`.
//!
//! `poly hooks install` provisions Git hook sources, and loading its config can
//! need a remote `extends` base: both are read from `poly.lock` only. Rather than
//! refusing until `poly lock` has been run, the command records the entries the
//! lock is missing first, the way `cargo build` writes `Cargo.lock` — through
//! [`lock_cmd::sync`] without `upgrade`, so an entry already recorded keeps its
//! pin. `--locked` (or [`LOCKED_ENV`]) turns this off and keeps the strict
//! "no lock entry" failure, for CI that must never change the lock.

use std::path::Path;

use anyhow::Result;

use crate::config_sources;
use crate::lock_cmd::{self, LockOutcome};
use crate::polylock::{self, PolyLock};

/// Environment variable that, like `--locked`, forbids adding missing
/// `poly.lock` entries.
pub const LOCKED_ENV: &str = "POLY_LOCKED";

/// Whether `--locked` (`flag`) or a truthy [`LOCKED_ENV`] forbids writing `poly.lock`.
#[must_use]
pub fn locked(flag: bool) -> bool {
    flag || std::env::var(LOCKED_ENV).is_ok_and(|value| poly_config::network::is_truthy(&value))
}

/// Record the `poly.lock` entries the repository's config declares but the lock
/// lacks, printing one line per change on stderr prefixed with `command`.
///
/// Does nothing when the lock is already complete (an offline comparison, so no
/// network), when there is no top-level config, or offline — the command then
/// fails on the missing entry exactly as without this step.
///
/// # Errors
///
/// When the config or `poly.lock` cannot be read, or resolving a missing entry fails.
pub fn lock_missing(command: &str) -> Result<()> {
    if crate::offline() {
        return Ok(());
    }
    let root = config_sources::repo_root()?;
    let config_path = crate::global()
        .config
        .clone()
        .unwrap_or_else(|| config_sources::root_config_path(&root));
    if !config_path.is_file() {
        return Ok(());
    }
    for change in lock_missing_at(&root, &config_path)? {
        eprintln!("{command}: {change}");
    }
    Ok(())
}

/// [`lock_missing`] for an explicit repository root and config: returns the
/// changes made to `<root>/poly.lock`, empty when it was already complete.
///
/// # Errors
///
/// As [`lock_missing`].
pub fn lock_missing_at(root: &Path, config_path: &Path) -> Result<Vec<String>> {
    if matches!(lock_cmd::sync(root, config_path, false, true)?, LockOutcome::InSync(_)) {
        return Ok(Vec::new());
    }
    let before = polylock::read(root)?.unwrap_or_default();
    let after = match lock_cmd::sync(root, config_path, false, false)? {
        LockOutcome::Written(lock) | LockOutcome::InSync(lock) => lock,
        LockOutcome::Drift => unreachable!("sync reports drift only when asked to check"),
    };
    Ok(changes(&before, &after))
}

/// One line per entry `after` adds to or drops from `before`.
fn changes(before: &PolyLock, after: &PolyLock) -> Vec<String> {
    let lock = polylock::LOCK_FILE_NAME;
    let mut lines = Vec::new();
    for entry in &after.extends {
        if before.locked_oid(&entry.git, &entry.file, &entry.revision) != Some(entry.locked.as_str()) {
            lines.push(format!(
                "locked remote config base {} ({}) {} at {} in {lock}",
                entry.git, entry.file, entry.revision, entry.locked
            ));
        }
    }
    for entry in &before.extends {
        if after.locked_oid(&entry.git, &entry.file, &entry.revision).is_none() {
            lines.push(format!(
                "dropped remote config base {} ({}) {} from {lock}: no longer declared",
                entry.git, entry.file, entry.revision
            ));
        }
    }
    for entry in &after.hook_sources {
        if before.hook_source(&entry.id, &entry.source) != Some(entry) {
            lines.push(format!(
                "locked hook source `{}` ({}) at {} in {lock}",
                entry.id, entry.source, entry.revision
            ));
        }
    }
    for entry in &before.hook_sources {
        if after.hook_source(&entry.id, &entry.source).is_none() {
            lines.push(format!(
                "dropped hook source `{}` ({}) from {lock}: no longer declared",
                entry.id, entry.source
            ));
        }
    }
    for entry in &after.tools {
        if before.tool(&entry.name) != Some(entry) {
            lines.push(format!("locked tool `{}` {} in {lock}", entry.name, entry.version));
        }
    }
    for entry in &before.tools {
        if after.tool(&entry.name).is_none() {
            lines.push(format!("dropped tool `{}` from {lock}: no longer declared", entry.name));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polylock::LockedHookSource;

    fn hook(id: &str, revision: char) -> LockedHookSource {
        LockedHookSource {
            id: id.to_string(),
            source: format!("https://example.invalid/{id}.git"),
            revision: revision.to_string().repeat(40),
            path: format!("cache://hook-sources/key/{id}"),
        }
    }

    #[test]
    fn reports_only_added_and_dropped_entries() {
        let before = PolyLock {
            hook_sources: vec![hook("kept", 'a'), hook("gone", 'b')],
            ..PolyLock::default()
        };
        let after = PolyLock {
            hook_sources: vec![hook("kept", 'a'), hook("new", 'c')],
            ..PolyLock::default()
        };
        assert_eq!(
            changes(&before, &after),
            vec![
                format!(
                    "locked hook source `new` (https://example.invalid/new.git) at {} in poly.lock",
                    "c".repeat(40)
                ),
                "dropped hook source `gone` (https://example.invalid/gone.git) from poly.lock: no longer declared"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn complete_lock_reports_nothing() {
        let lock = PolyLock {
            hook_sources: vec![hook("kept", 'a')],
            ..PolyLock::default()
        };
        assert!(changes(&lock, &lock).is_empty());
    }
}
