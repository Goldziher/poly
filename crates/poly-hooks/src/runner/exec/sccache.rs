//! The tier-2 sccache environment a compiler hook opts into.
//!
//! Separate from the rest of `exec` because it owns process-global state — the
//! one-shot server start — and a lifetime that is not the command's: everything
//! else in `exec` builds or runs a single child, whereas this decides once per
//! `poly hooks` process whether a shared sccache server exists at all. Keeping
//! that `Once` behind its own module boundary is what stops the rest of command
//! construction from having to reason about it.

use std::process::Stdio;
use std::sync::Once;

use tracing::warn;

use crate::model::{Hook, SccacheSettings};
use crate::process::Cmd;

/// Module-global guard so the shared sccache server is started at most once per
/// `poly hooks` process, no matter how many compiler hooks or batches run.
static SCCACHE_SERVER_START: Once = Once::new();

/// Inject the tier-2 sccache environment into a compiler hook's command.
///
/// A no-op unless the hook opted in via [`Hook::compiler`] **and** the run
/// carries [`SccacheSettings`]. Starts the shared sccache server once per
/// process (best-effort — a start failure only warns, since sccache also
/// auto-starts on first client use), then sets `RUSTC_WRAPPER` plus the
/// optional `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE`.
///
/// Caveat: if an sccache server is already running with a different `SCCACHE_DIR`
/// / size, the client env is ignored by that server — this is accepted.
pub(super) fn inject_sccache_env(cmd: &mut Cmd, hook: &Hook, sccache: Option<&SccacheSettings>) {
    if !hook.compiler {
        return;
    }
    let Some(settings) = sccache else {
        return;
    };
    ensure_sccache_server(settings);
    cmd.env("RUSTC_WRAPPER", &settings.bin);
    if let Some(dir) = &settings.dir {
        cmd.env("SCCACHE_DIR", dir);
    }
    if let Some(max_size) = &settings.max_size {
        cmd.env("SCCACHE_CACHE_SIZE", max_size);
    }
}

/// Start the sccache server idempotently (once per process), with the resolved
/// `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE` in its own environment. Best-effort: a
/// launch failure is logged and ignored.
fn ensure_sccache_server(settings: &SccacheSettings) {
    SCCACHE_SERVER_START.call_once(|| {
        let mut cmd = Cmd::new(&settings.bin, format!("{} --start-server", settings.bin));
        cmd.arg("--start-server");
        if let Some(dir) = &settings.dir {
            cmd.env("SCCACHE_DIR", dir);
        }
        if let Some(max_size) = &settings.max_size {
            cmd.env("SCCACHE_CACHE_SIZE", max_size);
        }
        cmd.check(false).stdout(Stdio::null()).stderr(Stdio::null());
        if let Err(error) = cmd.status() {
            warn!("failed to start sccache server: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use crate::model::{Hook, SccacheSettings};
    use crate::runner::exec::build_command;

    /// Collect the explicit environment overrides a built `Cmd` carries.
    fn injected_env(hook: &Hook, sccache: Option<&SccacheSettings>) -> HashMap<String, String> {
        let cmd = build_command(hook, Path::new("."), &[], sccache, None);
        cmd.get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
            })
            .collect()
    }

    /// `bin = "true"` keeps the one-shot `--start-server` probe harmless: `true`
    /// ignores its arguments and exits 0, so the test never requires sccache.
    fn settings() -> SccacheSettings {
        SccacheSettings {
            bin: "true".to_string(),
            dir: Some(std::path::PathBuf::from("/tmp/sccache-test")),
            max_size: Some("2G".to_string()),
        }
    }

    #[test]
    fn compiler_hook_gets_sccache_env_injected() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let env = injected_env(&hook, Some(&settings()));
        assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some("true"));
        assert_eq!(env.get("SCCACHE_DIR").map(String::as_str), Some("/tmp/sccache-test"));
        assert_eq!(env.get("SCCACHE_CACHE_SIZE").map(String::as_str), Some("2G"));
    }

    #[test]
    fn non_compiler_hook_gets_no_sccache_env() {
        let hook = Hook::run("fmt", "cargo fmt --check");
        let env = injected_env(&hook, Some(&settings()));
        assert!(!env.contains_key("RUSTC_WRAPPER"), "env: {env:?}");
        assert!(!env.contains_key("SCCACHE_DIR"), "env: {env:?}");
    }

    #[test]
    fn compiler_hook_without_settings_gets_no_sccache_env() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let env = injected_env(&hook, None);
        assert!(!env.contains_key("RUSTC_WRAPPER"), "env: {env:?}");
    }

    #[test]
    fn sccache_settings_without_dir_omits_dir_env() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let bare = SccacheSettings {
            bin: "true".to_string(),
            dir: None,
            max_size: None,
        };
        let env = injected_env(&hook, Some(&bare));
        assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some("true"));
        assert!(!env.contains_key("SCCACHE_DIR"), "env: {env:?}");
        assert!(!env.contains_key("SCCACHE_CACHE_SIZE"), "env: {env:?}");
    }
}
