//! Provision catalogs selected by `[[hooks.sources]]` in `poly.toml`.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use fs2::FileExt;
use poly_config::{HookMachinePreferences, HookSource, HooksConfig, Job, Stage, StageConfig, load_hook_preferences};
use serde::{Deserialize, Serialize};

const LOCK_FILE_NAME: &str = "poly-hooks.lock";
const PRODUCER_MANIFEST_NAME: &str = "poly-hooks.toml";

/// A selected producer hook and the execution path chosen for this machine.
#[derive(Debug, Clone)]
pub struct ResolvedHook {
    source_id: String,
    source_root: PathBuf,
    manifest: ManifestHook,
    command: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookPath {
    channel: String,
    check: String,
    run: String,
    #[serde(default)]
    install: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestHook {
    id: String,
    stages: Vec<Stage>,
    paths: Vec<HookPath>,
    #[serde(default)]
    pass_filenames: Option<bool>,
    #[serde(default)]
    always_run: Option<bool>,
    #[serde(flatten)]
    job: Job,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProducerManifest {
    version: u32,
    hooks: Vec<ManifestHook>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HookSourceLock {
    version: u32,
    sources: Vec<LockedSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockedSource {
    id: String,
    source: String,
    revision: String,
    path: String,
}

struct SourceLock(File);

impl Drop for SourceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Resolve selected sources and choose one eligible path for every selected hook.
pub fn provision(root: &Path, hooks: &HooksConfig, update: bool, install: bool) -> anyhow::Result<Vec<ResolvedHook>> {
    reject_legacy_consumer_file(root)?;
    if hooks.sources.is_empty() {
        return Ok(Vec::new());
    }
    let preferences = load_hook_preferences(root, true)?;
    let cache_root = poly_cache::hook_sources_dir()?;
    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("creating hook source cache {}", cache_root.display()))?;
    let existing = read_lock(root)?;
    let mut locked = Vec::new();
    let mut resolved = Vec::new();
    for source in &hooks.sources {
        let (entry, source_root) = provision_source(root, &cache_root, source, existing.as_ref(), update)?;
        if let Some(entry) = entry {
            locked.push(entry);
        }
        let manifest = load_manifest(&source_root)?;
        resolved.extend(select_hooks(source, &source_root, manifest, &preferences, install)?);
    }
    if update {
        if locked.is_empty() {
            remove_lock(root)?;
        } else {
            write_lock(
                root,
                &HookSourceLock {
                    version: 1,
                    sources: locked,
                },
            )?;
        }
    }
    Ok(resolved)
}

fn reject_legacy_consumer_file(root: &Path) -> anyhow::Result<()> {
    let legacy = root.join(PRODUCER_MANIFEST_NAME);
    if !legacy.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&legacy).with_context(|| format!("reading {}", legacy.display()))?;
    let document: toml::Value = toml::from_str(&text).with_context(|| format!("parsing {}", legacy.display()))?;
    if document.get("sources").is_some() {
        bail!(
            "{} is a producer catalog, not consumer configuration; move source declarations to [[hooks.sources]] in poly.toml",
            legacy.display()
        );
    }
    Ok(())
}

fn load_manifest(root: &Path) -> anyhow::Result<ProducerManifest> {
    let path = root.join(PRODUCER_MANIFEST_NAME);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading hook catalog {}", path.display()))?;
    let manifest: ProducerManifest =
        toml::from_str(&text).with_context(|| format!("parsing hook catalog {}", path.display()))?;
    if manifest.version != 1 {
        bail!(
            "hook catalog {} has unsupported version {}; expected 1",
            path.display(),
            manifest.version
        );
    }
    let mut ids = std::collections::BTreeSet::new();
    for hook in &manifest.hooks {
        if hook.id.is_empty() || !ids.insert(&hook.id) {
            bail!(
                "hook catalog {} contains an empty or duplicate hook id {:?}",
                path.display(),
                hook.id
            );
        }
        if hook.stages.is_empty() {
            bail!("catalog hook {:?} must declare at least one stage", hook.id);
        }
        if hook.stages.contains(&Stage::Always) {
            bail!("catalog hook {:?} cannot use the `always` pseudo-stage", hook.id);
        }
        if hook.job.run.is_some() || hook.job.script.is_some() || hook.job.runner.is_some() {
            bail!(
                "catalog hook {:?} must declare execution only through [[hooks.paths]]",
                hook.id
            );
        }
        if hook.paths.is_empty() {
            bail!("catalog hook {:?} must declare at least one execution path", hook.id);
        }
        let mut channels = std::collections::BTreeSet::new();
        for execution in &hook.paths {
            if execution.channel.is_empty() || execution.check.is_empty() || execution.run.is_empty() {
                bail!(
                    "catalog hook {:?} paths require nonempty channel, check, and run",
                    hook.id
                );
            }
            if execution.install.as_ref().is_some_and(String::is_empty) {
                bail!("catalog hook {:?} path install command cannot be empty", hook.id);
            }
            if !channels.insert(&execution.channel) {
                bail!(
                    "catalog hook {:?} has duplicate channel {:?}",
                    hook.id,
                    execution.channel
                );
            }
        }
    }
    Ok(manifest)
}

fn select_hooks(
    source: &HookSource,
    source_root: &Path,
    manifest: ProducerManifest,
    preferences: &HookMachinePreferences,
    install: bool,
) -> anyhow::Result<Vec<ResolvedHook>> {
    let by_id: std::collections::BTreeMap<_, _> =
        manifest.hooks.into_iter().map(|hook| (hook.id.clone(), hook)).collect();
    source
        .hooks
        .iter()
        .map(|id| {
            let manifest = by_id
                .get(id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("hook source {:?} selects unknown hook {:?}", source.id, id))?;
            let mut attempted = Vec::new();
            let mut selected_path = None;
            for channel in &preferences.channels {
                let Some(path) = manifest.paths.iter().find(|path| &path.channel == channel) else {
                    continue;
                };
                attempted.push(format!("{} ({})", channel, path.check));
                if check_path(path, source_root)? {
                    selected_path = Some(path.clone());
                    break;
                }
            }
            let path = selected_path.ok_or_else(|| {
                anyhow::anyhow!(
                    "hook source {:?} hook {:?} has no eligible execution path; attempted: {}",
                    source.id,
                    id,
                    if attempted.is_empty() {
                        "no configured channels matched".to_string()
                    } else {
                        attempted.join(", ")
                    }
                )
            })?;
            if install && let Some(command) = &path.install {
                run_command(
                    shell_command(command).current_dir(source_root),
                    "install hook execution path",
                )?;
            }
            Ok(ResolvedHook {
                source_id: source.id.clone(),
                source_root: source_root.to_path_buf(),
                manifest,
                command: path.run.clone(),
            })
        })
        .collect()
}

fn check_path(path: &HookPath, root: &Path) -> anyhow::Result<bool> {
    let output = shell_command(&path.check)
        .current_dir(root)
        .output()
        .with_context(|| format!("checking hook execution channel {:?}", path.channel))?;
    Ok(output.status.success())
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    let process = {
        let mut p = Command::new("cmd");
        p.args(["/C", command]);
        p
    };
    #[cfg(not(windows))]
    let process = {
        let mut p = Command::new("sh");
        p.args(["-c", command]);
        p
    };
    process
}

/// Merge selected producer hooks for `spec.stage` into the native runner model.
pub fn merge_stage(
    spec: &mut poly_hooks::StageSpec,
    hooks: &[ResolvedHook],
    poly_bin: &Path,
    files: &[PathBuf],
    cache_mode: &poly_config::HookCacheMode,
    consumer_root: &Path,
) -> anyhow::Result<()> {
    for selected in hooks {
        if !selected
            .manifest
            .stages
            .iter()
            .any(|stage| super::lower::to_hook_stage(*stage) == Some(spec.stage))
        {
            continue;
        }
        let mut job = selected.manifest.job.clone();
        job.name = Some(selected.manifest.id.clone());
        job.run = Some(selected.command.clone());
        job.script = None;
        job.env.insert(
            "POLY_HOOK_SOURCE_ROOT".to_string(),
            selected.source_root.to_string_lossy().into_owned(),
        );
        let mut stage = StageConfig::default();
        stage.commands.insert(selected.manifest.id.clone(), job);
        let mut config = HooksConfig {
            present: true,
            ..HooksConfig::default()
        };
        config
            .stage_configs
            .insert(super::lower::from_hook_stage(spec.stage), stage);
        let mut lowered = super::lower::lower_stage(
            &config,
            poly_bin,
            spec.stage,
            files,
            cache_mode,
            consumer_root,
            &poly_config::ToolsConfig::default(),
        )?;
        for hook in &mut lowered.hooks {
            hook.id = format!("{}:{}", selected.source_id, hook.id);
            if let Some(pass_filenames) = selected.manifest.pass_filenames {
                hook.pass_filenames = pass_filenames;
            }
            if let Some(always_run) = selected.manifest.always_run {
                hook.always_run = always_run;
            }
        }
        spec.hooks.extend(lowered.hooks);
    }
    spec.hooks.sort_by_key(|hook| hook.priority);
    Ok(())
}

fn provision_source(
    root: &Path,
    cache_root: &Path,
    source: &HookSource,
    existing: Option<&HookSourceLock>,
    update: bool,
) -> anyhow::Result<(Option<LockedSource>, PathBuf)> {
    if let Some(path) = &source.path {
        let candidate = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("resolving local hook source {}", candidate.display()))?;
        return Ok((None, canonical));
    }
    let url = source.git.as_deref().expect("validated Git source");
    let source_key = poly_cache::hook_source_key(url);
    let source_cache = cache_root.join(&source_key);
    let mirror = source_cache.join("mirror.git");
    let locked = existing.and_then(|lock| {
        lock.sources
            .iter()
            .find(|entry| entry.id == source.id && entry.source == url)
    });
    if !update {
        let locked = locked.ok_or_else(|| {
            anyhow::anyhow!(
                "Git hook source {:?} has no lock entry; run `poly hooks update` first",
                source.id
            )
        })?;
        validate_locked_revision(&locked.revision)?;
        let checkout = source_cache.join("checkouts").join(&locked.revision);
        let _guard = lock_source(&source_cache)?;
        if checkout_is_valid(&checkout, &locked.revision) {
            make_read_only(&checkout)?;
            return Ok((Some(locked.clone()), checkout));
        }
        ensure_mirror(&mirror, url)?;
        ensure_commit(&mirror, url, &locked.revision)?;
        materialize_checkout(&mirror, &checkout, &locked.revision)?;
        return Ok((Some(locked.clone()), checkout));
    }
    let revision = source.revision.as_deref().expect("validated Git revision");
    let _guard = lock_source(&source_cache)?;
    ensure_mirror(&mirror, url)?;
    run_git(&mirror, &["fetch", "--quiet", "--force", "origin", revision])?;
    let resolved = git_output(&mirror, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    validate_locked_revision(&resolved)?;
    let checkout = source_cache.join("checkouts").join(&resolved);
    materialize_checkout(&mirror, &checkout, &resolved)?;
    let cache_path = format!("cache://hook-sources/{source_key}/{resolved}");
    Ok((
        Some(LockedSource {
            id: source.id.clone(),
            source: url.to_string(),
            revision: resolved,
            path: cache_path,
        }),
        checkout,
    ))
}

fn lock_source(source_cache: &Path) -> anyhow::Result<SourceLock> {
    std::fs::create_dir_all(source_cache)
        .with_context(|| format!("creating hook source cache {}", source_cache.display()))?;
    let path = source_cache.join("source.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening hook source lock {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking hook source {}", path.display()))?;
    Ok(SourceLock(file))
}

fn ensure_mirror(mirror: &Path, url: &str) -> anyhow::Result<()> {
    if !mirror.is_dir() {
        let parent = mirror.parent().context("hook mirror has no parent")?;
        let temporary = tempfile::Builder::new()
            .prefix("mirror-")
            .tempdir_in(parent)
            .with_context(|| format!("creating temporary hook mirror in {}", parent.display()))?;
        let temporary_path = temporary.path().join("repository.git");
        run_command(
            Command::new("git")
                .args(["clone", "--quiet", "--mirror", "--", url])
                .arg(&temporary_path),
            "clone hook source mirror",
        )?;
        std::fs::rename(&temporary_path, mirror)
            .with_context(|| format!("installing hook source mirror {}", mirror.display()))?;
    }
    let origin = git_output(mirror, &["remote", "get-url", "origin"])?;
    if origin != url {
        bail!(
            "cached hook source mirror origin {:?} does not match configured {:?}",
            origin,
            url
        );
    }
    Ok(())
}

fn ensure_commit(mirror: &Path, url: &str, revision: &str) -> anyhow::Result<()> {
    if git_object_exists(mirror, revision)? {
        return Ok(());
    }
    run_git(mirror, &["fetch", "--quiet", "origin", revision])
        .with_context(|| format!("fetching locked hook source commit {revision} from {url}"))?;
    if !git_object_exists(mirror, revision)? {
        bail!("locked hook source commit {revision} is unavailable from {url}");
    }
    Ok(())
}

fn materialize_checkout(mirror: &Path, checkout: &Path, revision: &str) -> anyhow::Result<()> {
    if checkout.is_dir() {
        if checkout_is_valid(checkout, revision) {
            return make_read_only(checkout);
        }
        make_writable(checkout)?;
        std::fs::remove_dir_all(checkout)
            .with_context(|| format!("removing invalid hook checkout {}", checkout.display()))?;
    }
    let parent = checkout.parent().context("hook checkout has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating hook checkout directory {}", parent.display()))?;
    let temporary = tempfile::Builder::new()
        .prefix("checkout-")
        .tempdir_in(parent)
        .with_context(|| format!("creating temporary hook checkout in {}", parent.display()))?;
    let temporary_path = temporary.path().join("source");
    run_command(
        Command::new("git")
            .args(["clone", "--quiet", "--no-checkout", "--no-hardlinks"])
            .arg(mirror)
            .arg(&temporary_path),
        "clone hook source checkout",
    )?;
    run_git(&temporary_path, &["checkout", "--quiet", "--detach", revision])?;
    std::fs::rename(&temporary_path, checkout)
        .with_context(|| format!("installing hook source checkout {}", checkout.display()))?;
    make_read_only(checkout)
}

fn checkout_is_valid(checkout: &Path, revision: &str) -> bool {
    if !checkout.is_dir() {
        return false;
    }
    let head = git_output(checkout, &["rev-parse", "HEAD^{commit}"]);
    if !matches!(head.as_deref(), Ok(value) if value == revision) {
        return false;
    }
    matches!(
        git_output(checkout, &["status", "--porcelain=v1", "--untracked-files=all"]),
        Ok(status) if status.is_empty()
    )
}

fn validate_locked_revision(revision: &str) -> anyhow::Result<()> {
    let valid_length = revision.len() == 40 || revision.len() == 64;
    if !valid_length || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("locked hook source revision must be a full hexadecimal Git object ID: {revision:?}");
    }
    Ok(())
}

fn make_writable(root: &Path) -> anyhow::Result<()> {
    for entry in walkdir::WalkDir::new(root).contents_first(true) {
        let entry = entry.with_context(|| format!("walking hook checkout {}", root.display()))?;
        if entry.file_type().is_symlink() {
            continue;
        }
        let mut permissions = entry.metadata()?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | 0o700);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        std::fs::set_permissions(entry.path(), permissions)
            .with_context(|| format!("making hook checkout writable: {}", entry.path().display()))?;
    }
    Ok(())
}

fn make_read_only(root: &Path) -> anyhow::Result<()> {
    for entry in walkdir::WalkDir::new(root).contents_first(true) {
        let entry = entry.with_context(|| format!("walking hook checkout {}", root.display()))?;
        if entry.file_type().is_symlink() {
            continue;
        }
        let mut permissions = entry.metadata()?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() & !0o222);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(true);
        std::fs::set_permissions(entry.path(), permissions)
            .with_context(|| format!("making hook checkout read-only: {}", entry.path().display()))?;
    }
    Ok(())
}

fn git_object_exists(repository: &Path, revision: &str) -> anyhow::Result<bool> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .status()
        .context("checking locked Git revision")?
        .success())
}
fn run_git(directory: &Path, args: &[&str]) -> anyhow::Result<()> {
    run_command(Command::new("git").arg("-C").arg(directory).args(args), "run git")
}
fn git_output(directory: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .context("starting git")?;
    if !output.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
fn run_command(command: &mut Command, operation: &str) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("{operation}: starting command"))?;
    if !output.status.success() {
        bail!("{operation} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}
fn write_lock(root: &Path, lock: &HookSourceLock) -> anyhow::Result<()> {
    let path = root.join(LOCK_FILE_NAME);
    let temporary = root.join(format!("{LOCK_FILE_NAME}.tmp"));
    std::fs::write(
        &temporary,
        toml::to_string_pretty(lock).context("serializing hook source lock")?,
    )
    .with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("installing {}", path.display()))
}
fn remove_lock(root: &Path) -> anyhow::Result<()> {
    let path = root.join(LOCK_FILE_NAME);
    if path.is_file() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}
fn read_lock(root: &Path) -> anyhow::Result<Option<HookSourceLock>> {
    let path = root.join(LOCK_FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let lock: HookSourceLock =
        toml::from_str(&std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?)
            .with_context(|| format!("parsing {}", path.display()))?;
    if lock.version != 1 {
        bail!("unsupported {} version {}; expected 1", LOCK_FILE_NAME, lock.version);
    }
    Ok(Some(lock))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_source(repository: &Path) -> HookSource {
        HookSource {
            id: "rules".to_string(),
            path: None,
            git: Some(repository.to_string_lossy().into_owned()),
            revision: Some("HEAD".to_string()),
            hooks: vec!["validate".to_string()],
        }
    }

    fn create_git_source() -> tempfile::TempDir {
        let repository = tempfile::tempdir().unwrap();
        run_command(
            Command::new("git").arg("init").arg("--quiet").arg(repository.path()),
            "initialize test source",
        )
        .unwrap();
        std::fs::write(repository.path().join(PRODUCER_MANIFEST_NAME), "version=1\nhooks=[]\n").unwrap();
        run_git(repository.path(), &["add", PRODUCER_MANIFEST_NAME]).unwrap();
        run_command(
            Command::new("git").arg("-C").arg(repository.path()).args([
                "-c",
                "user.name=Poly Test",
                "-c",
                "user.email=poly@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "catalog",
            ]),
            "commit test source",
        )
        .unwrap();
        repository
    }

    fn make_writable(root: &Path) {
        for entry in walkdir::WalkDir::new(root)
            .contents_first(true)
            .into_iter()
            .filter_map(Result::ok)
        {
            let mut permissions = entry.metadata().unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            #[cfg(not(unix))]
            permissions.set_readonly(false);
            std::fs::set_permissions(entry.path(), permissions).unwrap();
        }
    }

    fn write_consumer(_root: &Path, source: &Path, hooks: &[&str]) -> HooksConfig {
        let selected = hooks.iter().map(|id| format!("{id:?}")).collect::<Vec<_>>().join(",");
        toml::from_str(&format!(
            "[[sources]]\nid='rules'\npath={:?}\nhooks=[{}]",
            source.to_string_lossy(),
            selected
        ))
        .unwrap()
    }
    fn write_catalog(root: &Path) {
        std::fs::write(
            root.join(PRODUCER_MANIFEST_NAME),
            r#"
version = 1
[[hooks]]
id = "validate"
stages = ["pre-commit"]
args = ["ok"]
workspace = true
[[hooks.paths]]
channel = "shell"
check = "command -v printf"
run = "printf"
[[hooks]]
id = "other"
stages = ["pre-push"]
[[hooks.paths]]
channel = "shell"
check = "false"
run = "false"
"#,
        )
        .unwrap();
    }
    fn preferences(root: &Path) {
        std::fs::write(root.join("poly.local.toml"), "[hook_preferences]\nchannels=['shell']\n").unwrap();
    }

    #[test]
    fn selects_explicit_hook_and_guarded_path() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        write_catalog(producer.path());
        preferences(consumer.path());
        let hooks = write_consumer(consumer.path(), producer.path(), &["validate"]);
        let selected = provision(consumer.path(), &hooks, false, true).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].command, "printf");
    }

    #[test]
    fn reports_unknown_hook() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        write_catalog(producer.path());
        preferences(consumer.path());
        let hooks = write_consumer(consumer.path(), producer.path(), &["missing"]);
        assert!(
            provision(consumer.path(), &hooks, false, true)
                .unwrap_err()
                .to_string()
                .contains("unknown hook")
        );
    }

    #[test]
    fn installs_selected_path_only_when_requested() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(
            producer.path().join(PRODUCER_MANIFEST_NAME),
            r#"
version = 1
[[hooks]]
id = "validate"
stages = ["pre-commit"]
[[hooks.paths]]
channel = "shell"
check = "command -v printf"
install = "printf installed > installed.txt"
run = "printf"
"#,
        )
        .unwrap();
        preferences(consumer.path());
        let hooks = write_consumer(consumer.path(), producer.path(), &["validate"]);
        provision(consumer.path(), &hooks, false, false).unwrap();
        assert!(!producer.path().join("installed.txt").exists());
        provision(consumer.path(), &hooks, false, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(producer.path().join("installed.txt")).unwrap(),
            "installed"
        );
    }

    #[test]
    fn lowers_catalog_args_and_filename_controls() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(
            producer.path().join(PRODUCER_MANIFEST_NAME),
            r#"
version = 1
[[hooks]]
id = "validate"
stages = ["pre-commit"]
args = ["generate", "--dry-run"]
workspace = true
pass_filenames = false
always_run = true
[[hooks.paths]]
channel = "shell"
check = "command -v printf"
run = "printf"
"#,
        )
        .unwrap();
        preferences(consumer.path());
        let config = write_consumer(consumer.path(), producer.path(), &["validate"]);
        let selected = provision(consumer.path(), &config, false, true).unwrap();
        let mut spec = poly_hooks::StageSpec {
            stage: poly_hooks::Stage::PreCommit,
            ..poly_hooks::StageSpec::default()
        };
        merge_stage(
            &mut spec,
            &selected,
            Path::new("poly"),
            &[],
            &poly_config::HookCacheMode::Off,
            consumer.path(),
        )
        .unwrap();
        assert_eq!(spec.hooks.len(), 1);
        assert_eq!(spec.hooks[0].args, ["generate", "--dry-run"]);
        assert!(!spec.hooks[0].pass_filenames);
        assert!(spec.hooks[0].always_run);
        assert!(spec.hooks[0].workspace);
        assert!(spec.hooks[0].cwd.is_none());
        assert_eq!(
            spec.hooks[0].env.get("POLY_HOOK_SOURCE_ROOT"),
            Some(&producer.path().canonicalize().unwrap().to_string_lossy().into_owned())
        );
    }

    #[test]
    fn rejects_legacy_consumer_catalog() {
        let root = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(PRODUCER_MANIFEST_NAME), "version=1\nsources=[]").unwrap();
        preferences(root.path());
        let hooks = write_consumer(root.path(), producer.path(), &["validate"]);
        assert!(
            provision(root.path(), &hooks, false, true)
                .unwrap_err()
                .to_string()
                .contains("producer catalog")
        );
    }

    #[test]
    fn rejects_catalog_execution_outside_guarded_paths() {
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(
            producer.path().join(PRODUCER_MANIFEST_NAME),
            r#"
version = 1
[[hooks]]
id = "unsafe"
stages = ["pre-commit"]
run = "false"
[[hooks.paths]]
channel = "shell"
check = "true"
run = "true"
"#,
        )
        .unwrap();
        assert!(
            load_manifest(producer.path())
                .unwrap_err()
                .to_string()
                .contains("only through [[hooks.paths]]")
        );
    }

    #[test]
    fn concurrent_consumers_share_one_mirror_and_checkout() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let cache_root = cache.path().to_path_buf();
            let source = source.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                provision_source(Path::new("."), &cache_root, &source, None, true).unwrap()
            }));
        }
        let first = workers.remove(0).join().unwrap();
        let second = workers.remove(0).join().unwrap();
        assert_eq!(first.1, second.1);
        let source_cache = cache
            .path()
            .join(poly_cache::hook_source_key(source.git.as_deref().unwrap()));
        assert!(source_cache.join("mirror.git").is_dir());
        assert_eq!(std::fs::read_dir(source_cache.join("checkouts")).unwrap().count(), 1);
        assert!(
            std::fs::metadata(first.1.join(PRODUCER_MANIFEST_NAME))
                .unwrap()
                .permissions()
                .readonly()
        );
    }

    #[test]
    fn normal_run_reconstructs_exact_locked_checkout() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = provision_source(Path::new("."), cache.path(), &source, None, true).unwrap();
        make_writable(&checkout);
        std::fs::remove_dir_all(&checkout).unwrap();
        let lock = HookSourceLock {
            version: 1,
            sources: vec![locked.unwrap()],
        };
        let (_, reconstructed) = provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap();
        assert_eq!(reconstructed, checkout);
        assert!(checkout.join(PRODUCER_MANIFEST_NAME).is_file());
        let head = git_output(&checkout, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(head, lock.sources[0].revision);
        assert!(!checkout.join(".git/objects/info/alternates").exists());
    }

    #[test]
    fn normal_run_replaces_checkout_with_wrong_head() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = provision_source(Path::new("."), cache.path(), &source, None, true).unwrap();
        let lock = HookSourceLock {
            version: 1,
            sources: vec![locked.unwrap()],
        };
        make_writable(&checkout);
        std::fs::write(checkout.join(".git/HEAD"), "0000000000000000000000000000000000000000\n").unwrap();

        let (_, reconstructed) = provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap();

        assert_eq!(reconstructed, checkout);
        assert_eq!(
            git_output(&checkout, &["rev-parse", "HEAD"]).unwrap(),
            lock.sources[0].revision
        );
    }

    #[test]
    fn normal_run_replaces_tampered_checkout() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = provision_source(Path::new("."), cache.path(), &source, None, true).unwrap();
        let lock = HookSourceLock {
            version: 1,
            sources: vec![locked.unwrap()],
        };
        make_writable(&checkout);
        std::fs::write(
            checkout.join(PRODUCER_MANIFEST_NAME),
            "version=1\nhooks=[]\n# tampered\n",
        )
        .unwrap();

        provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap();

        assert!(
            !std::fs::read_to_string(checkout.join(PRODUCER_MANIFEST_NAME))
                .unwrap()
                .contains("tampered")
        );
    }

    #[test]
    fn normal_run_reuses_valid_checkout_without_mirror() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = provision_source(Path::new("."), cache.path(), &source, None, true).unwrap();
        let lock = HookSourceLock {
            version: 1,
            sources: vec![locked.unwrap()],
        };
        let source_cache = cache
            .path()
            .join(poly_cache::hook_source_key(source.git.as_deref().unwrap()));
        std::fs::remove_dir_all(source_cache.join("mirror.git")).unwrap();

        let (_, reused) = provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap();

        assert_eq!(reused, checkout);
        assert!(!source_cache.join("mirror.git").exists());
    }

    #[test]
    fn normal_run_rejects_non_oid_lock_revision() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let lock = HookSourceLock {
            version: 1,
            sources: vec![LockedSource {
                id: source.id.clone(),
                source: source.git.clone().unwrap(),
                revision: "../../outside".to_string(),
                path: "cache://invalid".to_string(),
            }],
        };

        let error = provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap_err();

        assert!(error.to_string().contains("full hexadecimal Git object ID"));
    }
}
