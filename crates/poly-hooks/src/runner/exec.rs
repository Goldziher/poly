//! Subprocess construction and execution for the hook runner.
//!
//! Everything that turns a [`Hook`] (or a bare `before`/`after`/`precondition`
//! command line) into a running process lives here: shell selection and
//! quoting, `CARGO_TARGET_DIR` / sccache environment injection, and the
//! capture-or-preview output plumbing.
//!
//! This module itself keeps only the assembly step — the argv, cwd and
//! environment of a hook's command — and delegates the three concerns that
//! each stand on their own: [`shell`] (platform shell, quoting, the `"$@"`
//! append), [`sccache`] (the process-global compiler cache environment) and
//! [`run`] (supervision, timeouts, status classification). The runner's view is
//! unchanged: everything it uses is re-exported below.

use std::path::Path;

use crate::model::{Hook, HookCommand, SccacheSettings};
use crate::process::Cmd;

mod run;
mod sccache;
mod shell;

use sccache::inject_sccache_env;
use shell::shell_command;

pub(super) use run::{Probe, await_cargo_package_cache, execute, run_precondition, run_step};

pub(super) fn build_command(
    hook: &Hook,
    root: &Path,
    files: &[&Path],
    sccache: Option<&SccacheSettings>,
    cargo_target_dir: Option<&Path>,
) -> Cmd {
    let mut cmd = match &hook.command {
        HookCommand::Run(line) => shell_command(line, &hook.id, &hook.args, files, hook.pass_filenames),
        HookCommand::Script { path, runner } => {
            let mut cmd = match runner {
                Some(runner) => {
                    let mut cmd = Cmd::new(runner, hook.id.clone());
                    cmd.arg(path);
                    cmd
                }
                None => Cmd::new(path, hook.id.clone()),
            };
            cmd.args(&hook.args);
            if hook.pass_filenames {
                cmd.args(files.iter().map(|p| p.as_os_str()));
            }
            cmd
        }
    };
    cmd.current_dir(hook_working_dir(hook, root));
    cmd.envs(hook.env.iter());
    if let Some(target) = cargo_target_dir {
        if !hook.env.contains_key("CARGO_TARGET_DIR") {
            cmd.env("CARGO_TARGET_DIR", target);
        }
    }
    inject_sccache_env(&mut cmd, hook, sccache);
    cmd
}

/// The directory a hook (and therefore its own `precondition`/`before`) runs
/// from: the execution root, plus the hook's `cwd` override when set.
pub(super) fn hook_working_dir(hook: &Hook, root: &Path) -> std::path::PathBuf {
    hook.cwd
        .as_deref()
        .map_or_else(|| root.to_path_buf(), |relative| root.join(relative))
}

/// Estimate the argv bytes consumed by everything except the matched files, so
/// `ARG_MAX` batching reserves the right headroom.
pub(super) fn base_arg_len(hook: &Hook) -> usize {
    const FIXED: usize = 256;
    let command_len = match &hook.command {
        HookCommand::Run(line) => line.len(),
        HookCommand::Script { path, runner } => path.len() + runner.as_ref().map_or(0, String::len),
    };
    let args_len: usize = hook.args.iter().map(|a| a.len() + 9).sum();
    FIXED + command_len + args_len
}
