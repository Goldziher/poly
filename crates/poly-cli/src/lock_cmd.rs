//! `poly lock` — resolve remote `extends` bases, Git hook sources and
//! downloadable tool releases into the single `poly.lock` (ADR 0038, ADR 0046),
//! or verify it is current (`--check`).

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Args;
use poly_config::extends::ExtendsSource;
use poly_config::{PolyConfig, ToolsConfig};

use crate::config_sources::{self, RemoteExtendsResolver};
use crate::hooks::sources;
use crate::polylock::{self, LockedTool, PolyLock};

/// `poly lock` argument surface.
#[derive(Args)]
pub struct LockArgs {
    /// Re-resolve every symbolic ref to its latest object ID, and every `latest`
    /// tool to its newest release, instead of keeping what `poly.lock` records.
    #[arg(long)]
    upgrade: bool,

    /// Write nothing; exit 1 when `poly.lock` would change.
    ///
    /// Without `--upgrade` this is an offline comparison of what the config
    /// declares against what `poly.lock` records. With it, refs are re-resolved
    /// against their remotes and compared.
    #[arg(long)]
    check: bool,
}

/// What a `poly lock` run found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    /// `poly.lock` already matches (nothing written).
    InSync(PolyLock),
    /// `--check` only: `poly.lock` would change (nothing written).
    Drift,
    /// `poly.lock` was written (or removed, when the result is empty).
    Written(PolyLock),
}

/// Run `poly lock`, mapping any error to exit code 2.
pub fn run_lock(args: LockArgs) -> ExitCode {
    match lock(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly lock: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn lock(args: LockArgs) -> Result<ExitCode> {
    let root = config_sources::repo_root()?;
    let config_path = crate::global()
        .config
        .clone()
        .unwrap_or_else(|| config_sources::root_config_path(&root));
    if !config_path.is_file() {
        bail!("config file not found: {}", config_path.display());
    }
    match sync(&root, &config_path, args.upgrade, args.check)? {
        LockOutcome::Drift => {
            eprintln!(
                "poly lock: {} is out of date; run `poly lock`",
                polylock::LOCK_FILE_NAME
            );
            Ok(ExitCode::from(1))
        }
        LockOutcome::InSync(lock) => {
            println!("{} is up to date. {}", polylock::LOCK_FILE_NAME, summary(&lock));
            Ok(ExitCode::SUCCESS)
        }
        LockOutcome::Written(lock) => {
            println!("Wrote {}. {}", polylock::LOCK_FILE_NAME, summary(&lock));
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn summary(lock: &PolyLock) -> String {
    let (bases, sources, tools) = (lock.extends.len(), lock.hook_sources.len(), lock.tools.len());
    format!(
        "Locked {bases} remote config base{}, {sources} hook source{} and {tools} tool{}.",
        if bases == 1 { "" } else { "s" },
        if sources == 1 { "" } else { "s" },
        if tools == 1 { "" } else { "s" }
    )
}

/// Bring `<root>/poly.lock` in line with what `config_path` declares.
///
/// With `check`, nothing is written and drift is reported as
/// [`LockOutcome::Drift`]. Without `upgrade`, entries already in `poly.lock` keep
/// their pinned revision; entries for sources no longer declared are dropped.
pub fn sync(root: &Path, config_path: &Path, upgrade: bool, check: bool) -> Result<LockOutcome> {
    let existing = polylock::read(root)?.unwrap_or_default();
    let declared = config_sources::declared_extends(config_path)?;
    if check && !upgrade {
        return check_declared(root, config_path, &declared, existing);
    }

    let mut next = PolyLock {
        extends: config_sources::resolve_extends(&declared, &existing, upgrade)?,
        ..PolyLock::default()
    };
    // Hook sources live in the merged config, which may itself come from a base
    // that was only just locked, so load it against the in-memory result.
    let resolver = RemoteExtendsResolver::from_lock(root, next.clone());
    let config = PolyConfig::load_file_with(config_path, &resolver)
        .with_context(|| format!("loading {} to find hook sources", config_path.display()))?;
    next.hook_sources = sources::lock_sources(&config.hooks, &existing, upgrade)?;
    next.tools = lock_tools(&config.tools, &existing, upgrade)?;
    next.normalize();

    if matches_disk(root, &next)? {
        return Ok(LockOutcome::InSync(next));
    }
    if check {
        return Ok(LockOutcome::Drift);
    }
    polylock::write(root, &next)?;
    if !next.extends.is_empty() {
        config_sources::review_merged(config_path, &RemoteExtendsResolver::from_lock(root, next.clone()));
    }
    Ok(LockOutcome::Written(next))
}

/// Offline `--check`: compare the declared lockable sources with the recorded ones.
fn check_declared(
    root: &Path,
    config_path: &Path,
    declared: &[ExtendsSource],
    existing: PolyLock,
) -> Result<LockOutcome> {
    let recorded_extends = sorted(
        existing
            .extends
            .iter()
            .map(|entry| (entry.git.clone(), entry.file.clone(), entry.revision.clone())),
    );
    if config_sources::lockable_keys(declared) != recorded_extends {
        return Ok(LockOutcome::Drift);
    }
    let resolver = RemoteExtendsResolver::from_lock(root, existing.clone());
    let config = PolyConfig::load_file_with(config_path, &resolver)
        .with_context(|| format!("loading {} to find hook sources", config_path.display()))?;
    let recorded_hooks = sorted(
        existing
            .hook_sources
            .iter()
            .map(|entry| (entry.id.clone(), entry.source.clone())),
    );
    if sources::declared_git_keys(&config.hooks) != recorded_hooks {
        return Ok(LockOutcome::Drift);
    }
    if !tools_in_sync(&config.tools, &existing) {
        return Ok(LockOutcome::Drift);
    }
    if matches_disk(root, &existing)? {
        Ok(LockOutcome::InSync(existing))
    } else {
        Ok(LockOutcome::Drift)
    }
}

/// The lock entry of every downloadable tool the config enables. An entry that
/// still satisfies the config is kept as recorded unless `upgrade`.
fn lock_tools(tools: &ToolsConfig, existing: &PolyLock, upgrade: bool) -> Result<Vec<LockedTool>> {
    let mut locked = Vec::new();
    for name in poly_core::engines::catalog_tool::lockable_tools(tools) {
        let config = tools.get(&name).expect("lockable tools come from the config");
        let entry = poly_core::engines::catalog_tool::lock_tool(&name, config, upgrade, existing.tool(&name))
            .map_err(|message| anyhow::anyhow!(message))?;
        locked.push(entry);
    }
    Ok(locked)
}

/// Offline `--check` for tools: the recorded names are the lockable ones, and
/// each entry still satisfies its config (an exact `version` matches).
fn tools_in_sync(tools: &ToolsConfig, existing: &PolyLock) -> bool {
    let declared = poly_core::engines::catalog_tool::lockable_tools(tools);
    let recorded = sorted(existing.tools.iter().map(|entry| entry.name.clone()));
    declared == recorded
        && existing.tools.iter().all(|entry| {
            tools
                .get(&entry.name)
                .is_some_and(|config| poly_core::engines::catalog_tool::lock_satisfies(config, entry))
        })
}

fn sorted<T: Ord>(items: impl Iterator<Item = T>) -> Vec<T> {
    let mut items: Vec<T> = items.collect();
    items.sort();
    items.dedup();
    items
}

/// Whether the file on disk is byte-identical to what [`polylock::write`] would
/// produce for `lock` (absent, for an empty lock).
fn matches_disk(root: &Path, lock: &PolyLock) -> Result<bool> {
    let path = root.join(polylock::LOCK_FILE_NAME);
    let current = if path.is_file() {
        Some(std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?)
    } else {
        None
    };
    let desired = if lock.is_empty() { None } else { Some(lock.to_toml()?) };
    Ok(current == desired)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn write_config(root: &Path, text: &str) -> PathBuf {
        let path = root.join("poly.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn local_sources_need_no_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("base.toml"), "[defaults]\nline_length = 66\n").unwrap();
        let config = write_config(dir.path(), "extends = [{ path = \"./base.toml\" }]\n");
        assert!(matches!(
            sync(dir.path(), &config, false, true).unwrap(),
            LockOutcome::InSync(_)
        ));
        assert!(matches!(
            sync(dir.path(), &config, false, false).unwrap(),
            LockOutcome::InSync(_)
        ));
        assert!(!dir.path().join(polylock::LOCK_FILE_NAME).exists());
        assert!(matches!(
            sync(dir.path(), &config, true, false).unwrap(),
            LockOutcome::InSync(_)
        ));
    }

    #[test]
    fn stale_lock_is_drift_and_is_removed_on_lock() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_config(dir.path(), "[defaults]\nline_length = 100\n");
        std::fs::write(
            dir.path().join(polylock::LOCK_FILE_NAME),
            "version = 1\n\n[[hook_sources]]\nid = \"gone\"\nsource = \"https://example.invalid/g.git\"\n\
             revision = \"cccccccccccccccccccccccccccccccccccccccc\"\npath = \"cache://x\"\n",
        )
        .unwrap();
        assert_eq!(sync(dir.path(), &config, false, true).unwrap(), LockOutcome::Drift);
        assert!(
            dir.path().join(polylock::LOCK_FILE_NAME).is_file(),
            "--check writes nothing"
        );
        assert!(matches!(
            sync(dir.path(), &config, false, false).unwrap(),
            LockOutcome::Written(lock) if lock.is_empty()
        ));
        assert!(!dir.path().join(polylock::LOCK_FILE_NAME).exists());
        assert!(matches!(
            sync(dir.path(), &config, false, true).unwrap(),
            LockOutcome::InSync(_)
        ));
    }

    #[test]
    fn local_hook_sources_are_not_locked() {
        let dir = tempfile::tempdir().unwrap();
        let producer = dir.path().join("producer");
        std::fs::create_dir(&producer).unwrap();
        let config = write_config(
            dir.path(),
            "[[hooks.sources]]\nid = \"local\"\npath = \"producer\"\nhooks = [\"validate\"]\n",
        );
        assert!(matches!(
            sync(dir.path(), &config, false, true).unwrap(),
            LockOutcome::InSync(_)
        ));
    }
}
