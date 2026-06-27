use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HookKind {
    #[clap(name = "commit-msg")]
    CommitMsg,
}

pub fn install_hook(start_dir: &Path, kind: HookKind, write: bool, force: bool) -> Result<PathBuf> {
    let git_dir = locate_git_dir(start_dir).context("failed to locate .git directory")?;
    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).with_context(|| {
        format!(
            "failed to ensure hooks directory at {}",
            hooks_dir.display()
        )
    })?;

    let hook_name = hook_filename(kind);
    let hook_path = hooks_dir.join(hook_name);

    if hook_path.exists() && !force {
        bail!(
            "hook `{}` already exists at {} (use --force to overwrite)",
            hook_name,
            hook_path.display()
        );
    }

    let script = hook_script(kind, write)?;
    fs::write(&hook_path, script)
        .with_context(|| format!("failed to write hook to {}", hook_path.display()))?;
    apply_executable_permissions(&hook_path)?;

    Ok(hook_path)
}

fn locate_git_dir(start_dir: &Path) -> Result<PathBuf> {
    let mut current = start_dir;

    loop {
        let candidate = current.join(".git");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        if candidate.is_file() {
            return resolve_gitdir_file(&candidate);
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => bail!("no .git directory found from {}", start_dir.display()),
        }
    }
}

fn resolve_gitdir_file(git_file: &Path) -> Result<PathBuf> {
    let content = fs::read_to_string(git_file)
        .with_context(|| format!("failed to read gitdir file {}", git_file.display()))?;
    let content = content.trim();

    let prefix = "gitdir:";
    if let Some(rest) = content.strip_prefix(prefix) {
        let raw = rest.trim();
        let path = Path::new(raw);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            git_file
                .parent()
                .context("gitdir file missing parent")?
                .join(path)
                .canonicalize()
                .with_context(|| format!("failed to resolve gitdir path {}", path.display()))?
        };
        Ok(resolved)
    } else {
        bail!("unexpected gitdir file format in {}", git_file.display());
    }
}

fn hook_filename(kind: HookKind) -> &'static str {
    match kind {
        HookKind::CommitMsg => "commit-msg",
    }
}

fn hook_script(kind: HookKind, write: bool) -> Result<String> {
    let base = match kind {
        HookKind::CommitMsg => {
            if write {
                "exec gitfluff lint \"$1\" --write\n"
            } else {
                "exec gitfluff lint \"$1\"\n"
            }
        }
    };

    Ok(format!("#!/bin/sh\n{}\n", base.trim_end()))
}

fn apply_executable_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(path, perms).with_context(|| {
            format!("failed to set executable permissions on {}", path.display())
        })?;
    }

    #[cfg(not(unix))]
    {
        let mut perms = fs::metadata(path)
            .with_context(|| format!("failed to read permissions for {}", path.display()))?
            .permissions();
        perms.set_readonly(false);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to adjust permissions on {}", path.display()))?;
    }

    Ok(())
}
