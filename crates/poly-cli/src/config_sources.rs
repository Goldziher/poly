//! Remote `extends` config bases: the CLI-side [`BaseConfigResolver`] and the
//! `[[extends]]` half of the `poly.lock` flow.
//!
//! `poly-config` is deliberately network-free: it merges only local files that a
//! [`BaseConfigResolver`] hands back. This module implements the resolver the CLI
//! injects, materializing pinned remote git bases (via [`crate::remote`]) and
//! pinning symbolic refs through a repo-root `poly.lock`. It is the config
//! analogue of the `[[hooks.sources]]` lock flow in [`crate::hooks::sources`].

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use fs2::FileExt;
use poly_config::extends::{self, ExtendsSource};
use poly_config::{BaseConfigResolver, LocalPathResolver, PolyConfig};

use crate::polylock::{self, LockedExtends, PolyLock};
use crate::remote::{self, validate_locked_revision};

/// The default config file re-parsed for a repo's top-level `extends` list.
const ROOT_CONFIG_NAME: &str = "poly.toml";

/// The CLI's [`BaseConfigResolver`]: resolves local `path` bases exactly like
/// [`LocalPathResolver`] and materializes pinned remote `git` bases from the
/// per-user source cache, pinning symbolic refs through `poly.lock`.
///
/// A full-object-ID `revision` is self-pinning and needs no lock entry; any other
/// (symbolic) revision must have been locked by `poly lock` first.
#[derive(Debug)]
pub struct RemoteExtendsResolver {
    lock: PolyLock,
    root: PathBuf,
}

impl RemoteExtendsResolver {
    /// Build a resolver for the repository rooted at `root`, loading its
    /// `poly.lock` if present (an empty lock otherwise).
    pub fn new(root: &Path) -> anyhow::Result<Self> {
        let lock = polylock::read(root)?.unwrap_or_default();
        // The tool provisioner reads pinned releases from here, so every command
        // that builds a resolver sees the same `poly.lock`.
        poly_config::tool_lock::install(lock.tools.clone());
        Ok(Self::from_lock(root, lock))
    }

    /// Build a resolver over an in-memory `lock`, used by `poly lock` itself to
    /// load the config against the extends it has just resolved.
    pub fn from_lock(root: &Path, lock: PolyLock) -> Self {
        RemoteExtendsResolver {
            lock,
            root: root.to_path_buf(),
        }
    }

    /// The object ID a git `source` resolves to for display: the revision itself
    /// when it is already a full OID, else its locked OID (if any).
    pub fn resolved_oid(&self, source: &ExtendsSource) -> Option<String> {
        let git = source.git.as_deref()?;
        let revision = source.revision.as_deref().unwrap_or_default();
        if validate_locked_revision(revision).is_ok() {
            return Some(revision.to_string());
        }
        self.lock
            .locked_oid(git, source.file_or_default(), revision)
            .map(str::to_string)
    }
}

impl BaseConfigResolver for RemoteExtendsResolver {
    fn resolve(&self, source: &ExtendsSource, relative_to: &Path) -> anyhow::Result<PathBuf> {
        // Local `path` bases behave identically to the network-free default.
        let Some(git) = source.git.as_deref() else {
            return LocalPathResolver.resolve(source, relative_to);
        };
        let revision = source.revision.as_deref().unwrap_or_default();
        let file = source.file_or_default();

        // A full object ID is self-pinning; a symbolic ref must be locked first.
        let oid = if validate_locked_revision(revision).is_ok() {
            revision.to_string()
        } else {
            match self.lock.locked_oid(git, file, revision) {
                Some(oid) => oid.to_string(),
                None => bail!(
                    "remote config base {} pinned to a symbolic ref has no lock entry; \
                     run `poly lock` first{}",
                    source.display_id(),
                    polylock::legacy_hint(&self.root)
                ),
            }
        };

        let cache_root = poly_cache::remote_sources_dir()?;
        let checkout = materialize_locked(git, &oid, &cache_root, false)
            .with_context(|| format!("materializing remote config base {}", source.display_id()))?;
        contained_base_file(&checkout, file, source)
    }
}

/// Resolve `<checkout>/<file>` and confirm it is a regular file that stays inside
/// `checkout` after symlink and `..` resolution.
///
/// `file` is already validated to be relative and `..`-free
/// ([`ExtendsSource`] validation); this is the defense-in-depth check that also
/// catches a symlink committed inside the base repository that points outside the
/// checkout (which would otherwise be followed by `read_to_string`).
fn contained_base_file(checkout: &Path, file: &str, source: &ExtendsSource) -> anyhow::Result<PathBuf> {
    let canonical_checkout = checkout
        .canonicalize()
        .with_context(|| format!("canonicalizing checkout for {}", source.display_id()))?;
    let canonical = checkout
        .join(file)
        .canonicalize()
        .with_context(|| format!("remote config base {} does not contain {}", source.display_id(), file))?;
    if !canonical.starts_with(&canonical_checkout) {
        bail!(
            "remote config base {} file {:?} resolves outside its checkout (symlink or path traversal)",
            source.display_id(),
            file
        );
    }
    if !canonical.is_file() {
        bail!(
            "remote config base {} does not contain a regular file {:?}",
            source.display_id(),
            file
        );
    }
    Ok(canonical)
}

/// Materialize a remote base while holding an exclusive advisory lock on the
/// per-URL source cache, so concurrent `poly` runs don't race on the shared
/// mirror/checkout ([`remote::materialize`] itself does not lock).
fn materialize_locked(url: &str, revision: &str, cache_root: &Path, update: bool) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(cache_root)
        .with_context(|| format!("creating remote source cache {}", cache_root.display()))?;
    let lock_path = cache_root.join(format!("{}.lock", poly_cache::remote_source_key(url)));
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening source lock {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("locking {}", lock_path.display()))?;
    // The lock is released when `lock_file` drops at the end of this function.
    remote::materialize(url, revision, cache_root, update)
}

/// Build a [`RemoteExtendsResolver`] rooted at the repository, for the CLI
/// surfaces that load config outside `poly lint`/`poly fmt` (`poly cache`,
/// `poly rules`, `poly migrate`) so they honor remote `extends` bases too.
pub fn resolver() -> anyhow::Result<RemoteExtendsResolver> {
    RemoteExtendsResolver::new(&repo_root()?)
}

/// Resolve the repository root — the nearest ancestor containing `.git` — by a
/// pure-Rust upward walk, falling back to the working directory outside a repo.
///
/// This deliberately does **not** shell out to `git` (unlike
/// [`poly_hooks::git::get_root`]): it runs on every `poly lint`/`poly fmt`
/// invocation to locate `poly.lock`, and the pure-Rust walk keeps those
/// commands subprocess-free when no remote `extends` base is in play.
pub fn repo_root() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    let mut dir: &Path = &cwd;
    loop {
        if dir.join(".git").exists() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return Ok(cwd.clone()),
        }
    }
}

/// The `(git, file, revision)` key of every symbolic-ref git base in `declared`.
///
/// Full-object-ID bases are self-pinning and `path` bases are local, so neither
/// carries a lock entry.
pub fn lockable_keys(declared: &[ExtendsSource]) -> Vec<(String, String, String)> {
    let mut keys: Vec<_> = declared
        .iter()
        .filter_map(|source| {
            let git = source.git.as_deref()?;
            let revision = source.revision.as_deref().unwrap_or_default();
            validate_locked_revision(revision).is_err().then(|| {
                (
                    git.to_string(),
                    source.file_or_default().to_string(),
                    revision.to_string(),
                )
            })
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Resolve every symbolic-ref remote `extends` git base in `declared` to a full
/// object ID, returning the entries for `poly.lock`.
///
/// Without `upgrade`, a base already recorded in `existing` keeps its pinned
/// object ID and is not fetched; with it, every base is re-resolved against the
/// remote. Only the top-level config's **direct** git bases are locked —
/// transitive `extends` of a base are out of scope for v1.
///
/// Prints each resolved `url revision -> oid` and, per base, whether it introduces
/// a `[hooks]` or `[tools]` section (a safety-review aid before adopting it).
pub fn resolve_extends(
    declared: &[ExtendsSource],
    existing: &PolyLock,
    upgrade: bool,
) -> anyhow::Result<Vec<LockedExtends>> {
    let mut locked = Vec::new();
    let mut cache_root = None;
    for (git, file, revision) in lockable_keys(declared) {
        if !upgrade && let Some(oid) = existing.locked_oid(&git, &file, &revision) {
            locked.push(LockedExtends {
                git,
                file,
                revision,
                locked: oid.to_string(),
            });
            continue;
        }
        let display = format!("{git}@{revision}");
        if cache_root.is_none() {
            let dir = poly_cache::remote_sources_dir()?;
            std::fs::create_dir_all(&dir).with_context(|| format!("creating remote source cache {}", dir.display()))?;
            cache_root = Some(dir);
        }
        let cache_root = cache_root.as_deref().expect("cache root set above");
        let checkout = materialize_locked(&git, &revision, cache_root, true)
            .with_context(|| format!("resolving remote config base {display}"))?;
        let oid = checkout
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .with_context(|| format!("reading resolved object id for {display}"))?;
        validate_locked_revision(&oid).with_context(|| format!("validating resolved object id for {display}"))?;

        let base_file = checkout.join(&file);
        if !base_file.is_file() {
            bail!("remote config base {display} does not contain {file}");
        }
        let (has_hooks, has_tools) = base_sections(&base_file)?;
        println!("{git} {revision} -> {oid}");
        println!("    {file}: introduces [hooks]={has_hooks} [tools]={has_tools}");
        locked.push(LockedExtends {
            git,
            file,
            revision,
            locked: oid,
        });
    }
    Ok(locked)
}

/// Print what the merged effective config (transitive bases included) introduces.
///
/// The per-base lines from [`resolve_extends`] only reflect each *direct* base
/// file, while the loader merges the FULL chain — a base's own (full-OID,
/// lock-free) bases can introduce `[hooks]`/`[tools]` the summary never shows.
pub fn review_merged(config_path: &Path, resolver: &RemoteExtendsResolver) {
    match PolyConfig::load_file_with(config_path, resolver) {
        Ok(merged) => println!(
            "merged effective config introduces [hooks]={} [tools]={} (transitive bases included)",
            merged.hooks.present,
            !merged.tools.is_empty()
        ),
        Err(error) => {
            println!("warning: could not fully resolve the merged config for review: {error:#}");
        }
    }
    println!(
        "note: remote bases are trusted transitively — a pinned base may itself `extends` \
         further bases whose [hooks]/[tools] would run on your machine."
    );
}

/// Parse `config_path`'s top-level `extends` list without resolving it — used by
/// `poly config show` to list the declared bases alongside their resolved OIDs.
pub fn declared_extends(config_path: &Path) -> anyhow::Result<Vec<ExtendsSource>> {
    let mut table = parse_table(config_path)?;
    extends::take_extends(&mut table).with_context(|| format!("parsing `extends` in {}", config_path.display()))
}

/// The default top-level config file for `root` (`<root>/poly.toml`).
pub fn root_config_path(root: &Path) -> PathBuf {
    root.join(ROOT_CONFIG_NAME)
}

fn parse_table(path: &Path) -> anyhow::Result<toml::Table> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Whether a base config file declares a `[hooks]` and/or `[tools]` section.
fn base_sections(path: &Path) -> anyhow::Result<(bool, bool)> {
    let table = parse_table(path)?;
    Ok((table.contains_key("hooks"), table.contains_key("tools")))
}
