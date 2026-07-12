//! Provision local and pinned Git sources declared in `poly-hooks.toml`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use poly_config::{
    HookInstallChannel, HookMachinePreferences, HookSource, MissingToolchainPolicy, load_hook_source_config,
};
use serde::{Deserialize, Serialize};

const LOCK_FILE_NAME: &str = "poly-hooks.lock";
const SOURCE_MANIFEST_NAME: &str = "poly-hook.toml";

/// A provisioned source and its live or locked checkout directory.
#[derive(Debug, Clone)]
pub struct ResolvedSource {
    /// Stable identifier from the repository's `poly-hooks.toml`.
    pub id: String,
    /// Canonical local directory containing the source's `poly-hook.toml`.
    pub root: PathBuf,
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
    revision: Option<String>,
    path: String,
}

/// Provision configured hook sources and atomically refresh `poly-hooks.lock`.
/// Returns immediately when the repository has no `poly-hooks.toml`.
pub fn provision(root: &Path, update: bool) -> anyhow::Result<Vec<ResolvedSource>> {
    let Some((config, preferences)) = load_hook_source_config(root)? else {
        return Ok(Vec::new());
    };
    let cache_root = poly_cache::repo_cache_dir(root)?.join("hook-sources");
    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("creating hook source cache {}", cache_root.display()))?;

    let existing = read_lock(root)?;
    let mut locked = Vec::with_capacity(config.sources.len());
    let mut resolved = Vec::with_capacity(config.sources.len());
    for source in &config.sources {
        if should_skip_for_toolchain(source, &preferences)? {
            continue;
        }
        let (entry, source_root) = provision_source(root, &cache_root, source, existing.as_ref(), update)?;
        ensure_toolchain(source, &preferences)?;
        locked.push(entry);
        resolved.push(ResolvedSource {
            id: source.id.clone(),
            root: source_root,
        });
    }
    if update {
        write_lock(
            root,
            &HookSourceLock {
                version: 1,
                sources: locked,
            },
        )?;
    }
    Ok(resolved)
}

/// Load a source's stage manifest. Local sources are read on every invocation,
/// so edits are immediately visible without an update step.
pub fn load_manifest(source: &ResolvedSource) -> anyhow::Result<poly_config::HooksConfig> {
    let path = source.root.join(SOURCE_MANIFEST_NAME);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading hook manifest {}", path.display()))?;
    let hooks: poly_config::HooksConfig =
        toml::from_str(&text).with_context(|| format!("parsing hook manifest {}", path.display()))?;
    hooks.validate().map_err(anyhow::Error::msg)?;
    validate_manifest_paths(&hooks)?;
    Ok(hooks)
}

fn validate_manifest_paths(hooks: &poly_config::HooksConfig) -> anyhow::Result<()> {
    for (stage, config) in &hooks.stage_configs {
        for (label, job) in config.labeled_jobs() {
            for (field, value) in [("root", job.root.as_deref()), ("script", job.script.as_deref())] {
                let Some(value) = value else { continue };
                let path = Path::new(value);
                if path.is_absolute()
                    || path
                        .components()
                        .any(|component| matches!(component, std::path::Component::ParentDir))
                {
                    bail!("hook manifest stage {stage} job {label:?} {field} must stay inside its source");
                }
            }
        }
    }
    Ok(())
}

/// Lower source manifests into `spec`, prefixing identifiers and anchoring
/// scripts/working directories at each source checkout.
pub fn merge_stage(
    spec: &mut poly_hooks::StageSpec,
    sources: &[ResolvedSource],
    poly_bin: &Path,
    files: &[PathBuf],
    cache_mode: &poly_config::HookCacheMode,
) -> anyhow::Result<()> {
    for source in sources {
        let hooks = load_manifest(source)?;
        let mut source_spec = super::lower::lower_stage(
            &hooks,
            poly_bin,
            spec.stage,
            files,
            cache_mode,
            &source.root,
            &poly_config::ToolsConfig::default(),
        )?;
        for hook in &mut source_spec.hooks {
            hook.id = format!("{}:{}", source.id, hook.id);
            hook.cwd = Some(match hook.cwd.take() {
                Some(relative) => source.root.join(relative),
                None => source.root.clone(),
            });
            if let poly_hooks::HookCommand::Script { path, .. } = &mut hook.command {
                let candidate = Path::new(path);
                if candidate.is_relative() {
                    *path = source.root.join(candidate).to_string_lossy().into_owned();
                }
            }
        }
        if let Some(source_precondition) = source_spec.precondition.take() {
            spec.precondition = Some(match spec.precondition.take() {
                Some(existing) => format!("({existing}) && ({source_precondition})"),
                None => source_precondition,
            });
        }
        spec.before.extend(source_spec.before);
        spec.after.extend(source_spec.after);
        spec.hooks.extend(source_spec.hooks);
    }
    spec.hooks.sort_by_key(|hook| hook.priority);
    Ok(())
}

fn should_skip_for_toolchain(source: &HookSource, preferences: &HookMachinePreferences) -> anyhow::Result<bool> {
    if source.channel != HookInstallChannel::Managed || source.toolchain.is_some() {
        return Ok(false);
    }
    match preferences.missing_toolchain {
        MissingToolchainPolicy::Error => bail!(
            "managed hook source {:?} has no toolchain; set `toolchain` in poly-hooks.toml or choose missing_toolchain = \"warn\"/\"skip\" in poly.local.toml",
            source.id
        ),
        MissingToolchainPolicy::Warn => {
            eprintln!(
                "poly hooks: skipping source {:?}: managed toolchain is unset",
                source.id
            );
            Ok(true)
        }
        MissingToolchainPolicy::Skip => Ok(true),
    }
}

fn ensure_toolchain(source: &HookSource, preferences: &HookMachinePreferences) -> anyhow::Result<()> {
    let Some(toolchain) = source.toolchain.as_deref() else {
        return Ok(());
    };
    if which::which(toolchain).is_ok() {
        return Ok(());
    }
    if source.channel == HookInstallChannel::System {
        bail!(
            "hook source {:?} requires missing system toolchain {:?}",
            source.id,
            toolchain
        );
    }
    for channel in &preferences.channels {
        if let Some(argv) = source.installers.get(channel).filter(|argv| !argv.is_empty()) {
            eprintln!(
                "poly hooks: installing toolchain {:?} for source {:?} via channel {:?}",
                toolchain, source.id, channel
            );
            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]);
            return run_command(&mut command, "install hook toolchain");
        }
    }
    let version = preferences.toolchains.get(toolchain).ok_or_else(|| {
        anyhow::anyhow!("hook source {:?} requires missing toolchain {:?}; configure hook_preferences.toolchains.{} or an explicit install recipe", source.id, toolchain, toolchain)
    })?;
    if !source.installers.is_empty() {
        bail!(
            "hook source {:?} has no installer matching configured channels {:?}",
            source.id,
            preferences.channels
        );
    }
    let mut command = match toolchain {
        "python" => {
            let mut c = Command::new("uv");
            c.args(["python", "install", version]);
            c
        }
        "rust" => {
            let mut c = Command::new("rustup");
            c.args(["toolchain", "install", version]);
            c
        }
        "node" | "go" => {
            let mut c = Command::new("mise");
            c.args(["install", &format!("{toolchain}@{version}")]);
            c
        }
        _ => bail!(
            "no managed install recipe for toolchain {:?}; configure a channel installer",
            toolchain
        ),
    };
    run_command(&mut command, "install hook toolchain")
}

fn provision_source(
    root: &Path,
    cache_root: &Path,
    source: &HookSource,
    existing: Option<&HookSourceLock>,
    update: bool,
) -> anyhow::Result<(LockedSource, PathBuf)> {
    if let Some(relative) = &source.path {
        let canonical_root = root.canonicalize().context("canonicalizing repository root")?;
        let path = root.join(relative);
        let canonical = path
            .canonicalize()
            .with_context(|| format!("resolving local hook source {}", path.display()))?;
        if !canonical.starts_with(&canonical_root) {
            bail!("local hook source {:?} resolves outside the repository", source.id);
        }
        return Ok((
            LockedSource {
                id: source.id.clone(),
                source: "local".to_string(),
                revision: None,
                path: relative.to_string_lossy().into_owned(),
            },
            canonical,
        ));
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
        let locked_revision = locked
            .revision
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Git hook source {:?} lock entry has no resolved revision", source.id))?;
        ensure_checkout_origin(&checkout, url)?;
        if !git_object_exists(&checkout, locked_revision)? {
            run_git(&checkout, &["fetch", "--quiet", "origin", locked_revision])?;
        }
        run_git(&checkout, &["checkout", "--quiet", "--detach", locked_revision])?;
        return Ok((locked.clone(), checkout));
    }
    let revision = source.revision.as_deref().expect("validated Git revision");
    let target = if checkout.join(".git").exists() {
        let origin = git_output(&checkout, &["remote", "get-url", "origin"])?;
        if origin != url {
            bail!(
                "cached hook source {:?} has origin {:?}, expected {:?}; remove {} and retry",
                source.id,
                origin,
                url,
                checkout.display()
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
        LockedSource {
            id: source.id.clone(),
            source: url.to_string(),
            revision: Some(resolved),
            path: format!("cache://hook-sources/{}", source.id),
        },
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
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .output()
        .context("checking locked Git revision")?;
    Ok(output.status.success())
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
    let content = toml::to_string_pretty(lock).context("serializing hook source lock")?;
    std::fs::write(&temporary, content).with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("installing {}", path.display()))
}

fn read_lock(root: &Path) -> anyhow::Result<Option<HookSourceLock>> {
    let path = root.join(LOCK_FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let lock: HookSourceLock = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if lock.version != 1 {
        bail!("unsupported {} version {}; expected 1", LOCK_FILE_NAME, lock.version);
    }
    Ok(Some(lock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_safe_local_source_without_creating_lock() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("hooks")).unwrap();
        std::fs::write(
            root.path().join("poly-hooks.toml"),
            r#"
version = 1
[[sources]]
id = "local"
path = "hooks"
channel = "system"
"#,
        )
        .unwrap();

        provision(root.path(), false).unwrap();

        assert!(!root.path().join(LOCK_FILE_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_local_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("hooks")).unwrap();
        std::fs::write(
            root.path().join("poly-hooks.toml"),
            r#"
version = 1
[[sources]]
id = "escape"
path = "hooks"
channel = "system"
"#,
        )
        .unwrap();

        assert!(
            provision(root.path(), false)
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
    }

    #[test]
    fn rejects_managed_source_without_toolchain_by_default() {
        let source = HookSource {
            id: "remote".to_string(),
            path: None,
            git: Some("https://example.com/hooks.git".to_string()),
            revision: Some("0123456789abcdef".to_string()),
            channel: HookInstallChannel::Managed,
            toolchain: None,
            installers: std::collections::BTreeMap::new(),
        };

        let error = should_skip_for_toolchain(&source, &HookMachinePreferences::default()).unwrap_err();
        assert!(error.to_string().contains("has no toolchain"));
    }

    #[test]
    fn local_manifest_lowers_and_runs_from_source_directory() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("hooks");
        std::fs::create_dir(&source_root).unwrap();
        std::fs::write(
            root.path().join("poly-hooks.toml"),
            r#"
version = 1
[[sources]]
id = "local"
path = "hooks"
channel = "system"
"#,
        )
        .unwrap();
        std::fs::write(
            source_root.join(SOURCE_MANIFEST_NAME),
            r#"
[pre-commit.commands.marker]
run = "printf ran > marker.txt"
"#,
        )
        .unwrap();

        let sources = provision(root.path(), false).unwrap();
        let mut spec = poly_hooks::StageSpec {
            stage: poly_hooks::Stage::PreCommit,
            ..poly_hooks::StageSpec::default()
        };
        merge_stage(
            &mut spec,
            &sources,
            Path::new("poly"),
            &[],
            &poly_config::HookCacheMode::Off,
        )
        .unwrap();
        let outcome = poly_hooks::run(poly_hooks::HookRunRequest {
            root: root.path().to_path_buf(),
            stages: vec![spec],
            ..poly_hooks::HookRunRequest::default()
        })
        .unwrap();

        assert!(outcome.success());
        assert_eq!(std::fs::read_to_string(source_root.join("marker.txt")).unwrap(), "ran");
    }

    #[test]
    fn normal_run_keeps_locked_git_revision_and_update_refreshes_it() {
        fn git(directory: &Path, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(directory)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }
        let upstream = tempfile::tempdir().unwrap();
        git(upstream.path(), &["init", "-q", "-b", "main"]);
        git(upstream.path(), &["config", "user.email", "test@example.com"]);
        git(upstream.path(), &["config", "user.name", "Test"]);
        std::fs::write(
            upstream.path().join(SOURCE_MANIFEST_NAME),
            "[pre-commit.commands.one]\nrun = \"true\"\n",
        )
        .unwrap();
        git(upstream.path(), &["add", "."]);
        git(upstream.path(), &["commit", "-qm", "one"]);

        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("poly-hooks.toml"),
            format!(
                "version = 1\n[[sources]]\nid = \"remote\"\ngit = {:?}\nrevision = \"main\"\ntoolchain = \"sh\"\n",
                upstream.path().to_string_lossy()
            ),
        )
        .unwrap();
        let first = provision(root.path(), true).unwrap();
        let first_revision = git_output(&first[0].root, &["rev-parse", "HEAD"]).unwrap();
        let original_lock = std::fs::read(root.path().join(LOCK_FILE_NAME)).unwrap();

        std::fs::write(upstream.path().join("second"), "two").unwrap();
        git(upstream.path(), &["add", "."]);
        git(upstream.path(), &["commit", "-qm", "two"]);

        std::fs::remove_dir_all(&first[0].root).unwrap();
        let locked = provision(root.path(), false).unwrap();
        assert_eq!(
            git_output(&locked[0].root, &["rev-parse", "HEAD"]).unwrap(),
            first_revision
        );
        assert_eq!(std::fs::read(root.path().join(LOCK_FILE_NAME)).unwrap(), original_lock);
        let updated = provision(root.path(), true).unwrap();
        assert_ne!(
            git_output(&updated[0].root, &["rev-parse", "HEAD"]).unwrap(),
            first_revision
        );
    }

    #[test]
    fn normal_run_rejects_unlocked_git_source_without_writing_lock() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("poly-hooks.toml"),
            "version = 1\n[[sources]]\nid = \"remote\"\ngit = \"https://example.invalid/hooks\"\nrevision = \"main\"\ntoolchain = \"sh\"\n",
        )
        .unwrap();

        let error = provision(root.path(), false).unwrap_err();
        assert!(error.to_string().contains("poly hooks update"));
        assert!(!root.path().join(LOCK_FILE_NAME).exists());
    }
}
