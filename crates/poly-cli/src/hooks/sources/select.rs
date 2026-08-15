//! Choosing one eligible execution path per selected hook: channel preference
//! order, the guarded existence check, and the optional install command.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use poly_config::{HookMachinePreferences, HookSource};

use super::manifest::{HookPath, ManifestHook, ProducerManifest};
use crate::remote::run_command;

/// A selected producer hook and the execution path chosen for this machine.
#[derive(Debug, Clone)]
pub struct ResolvedHook {
    pub(super) source_id: String,
    pub(super) source_root: PathBuf,
    pub(super) manifest: ManifestHook,
    pub(super) command: String,
}

pub(super) fn select_hooks(
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
    if let Some(binary) = command_v_binary(&path.check) {
        return Ok(which::which(binary).is_ok());
    }
    let output = shell_command(&path.check)
        .current_dir(root)
        .output()
        .with_context(|| format!("checking hook execution channel {:?}", path.channel))?;
    Ok(output.status.success())
}

fn command_v_binary(check: &str) -> Option<&str> {
    let mut words = check.split_whitespace();
    if words.next()? != "command" || words.next()? != "-v" {
        return None;
    }
    let binary = words.next()?;
    words.next().is_none().then_some(binary)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_portable_command_existence_check() {
        assert_eq!(command_v_binary("command -v npx"), Some("npx"));
        assert_eq!(command_v_binary("command -v"), None);
        assert_eq!(command_v_binary("command -v npx && false"), None);
        assert_eq!(command_v_binary("npx --version"), None);
    }
}
