//! Provision catalogs selected by `[[hooks.sources]]` in `poly.toml`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
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

/// Resolve selected sources and choose one eligible path for every selected hook.
pub fn provision(root: &Path, hooks: &HooksConfig, update: bool, install: bool) -> anyhow::Result<Vec<ResolvedHook>> {
    reject_legacy_consumer_file(root)?;
    if hooks.sources.is_empty() {
        return Ok(Vec::new());
    }
    let preferences = load_hook_preferences(root, true)?;
    let cache_root = poly_cache::repo_cache_dir(root)?.join("hook-sources");
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
            if !hook.workspace {
                hook.cwd = Some(selected.source_root.clone());
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
    let checkout = cache_root.join(&source.id);
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
        ensure_checkout_origin(&checkout, url)?;
        if !git_object_exists(&checkout, &locked.revision)? {
            run_git(&checkout, &["fetch", "--quiet", "origin", &locked.revision])?;
        }
        run_git(&checkout, &["checkout", "--quiet", "--detach", &locked.revision])?;
        return Ok((Some(locked.clone()), checkout));
    }
    let revision = source.revision.as_deref().expect("validated Git revision");
    let target = if checkout.join(".git").exists() {
        let origin = git_output(&checkout, &["remote", "get-url", "origin"])?;
        if origin != url {
            bail!(
                "cached hook source {:?} has origin {:?}, expected {:?}",
                source.id,
                origin,
                url
            );
        }
        run_git(&checkout, &["fetch", "--quiet", "--force", "origin", revision])?;
        "FETCH_HEAD"
    } else {
        run_command(
            Command::new("git")
                .args(["clone", "--quiet", "--no-checkout", "--", url])
                .arg(&checkout),
            "clone hook source",
        )?;
        revision
    };
    run_git(&checkout, &["checkout", "--quiet", "--detach", target])?;
    let resolved = git_output(&checkout, &["rev-parse", "HEAD"])?;
    Ok((
        Some(LockedSource {
            id: source.id.clone(),
            source: url.to_string(),
            revision: resolved,
            path: format!("cache://hook-sources/{}", source.id),
        }),
        checkout,
    ))
}

fn ensure_checkout_origin(checkout: &Path, url: &str) -> anyhow::Result<()> {
    if !checkout.join(".git").exists() {
        std::fs::create_dir_all(checkout)
            .with_context(|| format!("creating locked hook checkout {}", checkout.display()))?;
        run_git(checkout, &["init", "--quiet"])?;
        return run_git(checkout, &["remote", "add", "origin", url]);
    }
    let origin = git_output(checkout, &["remote", "get-url", "origin"])?;
    if origin != url {
        bail!(
            "cached hook source origin {:?} does not match configured {:?}",
            origin,
            url
        );
    }
    Ok(())
}

fn git_object_exists(checkout: &Path, revision: &str) -> anyhow::Result<bool> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(checkout)
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
}
