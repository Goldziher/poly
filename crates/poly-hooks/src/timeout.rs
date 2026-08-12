//! Per-hook time budgets.
//!
//! A hook that never returns used to hang the commit forever with no output at
//! all: no way to tell whether it was running, wedged, or skipped. Every hook
//! therefore runs under a [`Budget`] — a limit past which poly kills it and
//! reports [`crate::model::HookStatus::TimedOut`], plus the cadence at which a
//! still-running hook announces itself so a hang names its culprit long before
//! it is killed.
//!
//! # Choosing the defaults
//!
//! Too low a default turns a working setup into a broken one, so the defaults
//! are hang detectors, not performance budgets, and they differ by hook shape:
//!
//! - A **per-file** hook is a formatter or linter over a file batch; those
//!   finish in milliseconds to seconds. [`DEFAULT_HOOK_TIMEOUT`] (10 minutes)
//!   is orders of magnitude above anything legitimate while still bounding a
//!   wedged one.
//! - A **whole-project** hook ([`Hook::workspace`]) compiles or analyses the
//!   entire tree — `cargo clippy` on a cold `target/`, `tsc` on a large
//!   monorepo — and can legitimately run for many minutes.
//!   [`DEFAULT_WORKSPACE_HOOK_TIMEOUT`] (30 minutes) is deliberately far
//!   longer, because killing a real cold build would be a worse defect than the
//!   hang it was meant to catch.
//!
//! Both are overridable: [`Hook::timeout`] per hook, or the
//! [`HOOK_TIMEOUT_ENV`] / [`WORKSPACE_HOOK_TIMEOUT_ENV`] environment variables
//! run-wide (whole seconds; `0`, `off` or `none` disables the limit entirely and
//! restores the old unbounded behaviour).

use std::time::Duration;

use tracing::warn;

use crate::consts::env_vars::EnvVars;
use crate::model::Hook;

/// Default budget for a per-file hook.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_mins(10);

/// Default budget for a whole-project ([`Hook::workspace`]) hook.
pub const DEFAULT_WORKSPACE_HOOK_TIMEOUT: Duration = Duration::from_mins(30);

/// How long a hook must run before it announces that it is still running.
///
/// Short enough that a hang names its culprit while the author is still
/// watching, long enough that an ordinary run stays silent.
pub const STILL_RUNNING_AFTER: Duration = Duration::from_secs(15);

/// How often a still-running hook repeats its announcement after the first one.
pub const STILL_RUNNING_EVERY: Duration = Duration::from_mins(1);

/// Environment variable overriding [`DEFAULT_HOOK_TIMEOUT`] (whole seconds).
pub const HOOK_TIMEOUT_ENV: &str = "POLY_HOOK_TIMEOUT";

/// Environment variable overriding [`DEFAULT_WORKSPACE_HOOK_TIMEOUT`] (whole
/// seconds).
pub const WORKSPACE_HOOK_TIMEOUT_ENV: &str = "POLY_HOOK_WORKSPACE_TIMEOUT";

/// The time budget one hook process runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Kill the process once it has run this long; `None` never kills.
    pub limit: Option<Duration>,
    /// Announce that the hook is still running after this long; `None` never
    /// announces.
    pub announce_after: Option<Duration>,
    /// Repeat the announcement at this interval.
    pub announce_every: Duration,
}

impl Budget {
    /// A budget that neither kills nor announces — the pre-timeout behaviour,
    /// used for stage `before`/`after` steps and preconditions.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            limit: None,
            announce_after: None,
            announce_every: STILL_RUNNING_EVERY,
        }
    }

    /// Whether this budget needs the supervised execution path at all.
    ///
    /// When it does not, the runner keeps the plain capture path, so disabling
    /// timeouts restores the previous behaviour exactly.
    #[must_use]
    pub const fn is_supervised(&self) -> bool {
        self.limit.is_some() || self.announce_after.is_some()
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// The budget `hook` runs under: its own [`Hook::timeout`] when it declares
/// one, else the shape-derived default (possibly overridden by the environment).
#[must_use]
pub fn budget_for(hook: &Hook) -> Budget {
    let limit = match hook.timeout {
        Some(explicit) => Some(explicit),
        None => default_limit(hook.workspace),
    };
    Budget {
        limit,
        announce_after: Some(STILL_RUNNING_AFTER),
        announce_every: STILL_RUNNING_EVERY,
    }
}

/// The default budget for a hook of this shape, after the environment override.
fn default_limit(workspace: bool) -> Option<Duration> {
    let (name, fallback) = if workspace {
        (WORKSPACE_HOOK_TIMEOUT_ENV, DEFAULT_WORKSPACE_HOOK_TIMEOUT)
    } else {
        (HOOK_TIMEOUT_ENV, DEFAULT_HOOK_TIMEOUT)
    };
    let Ok(raw) = EnvVars::var(name) else {
        return Some(fallback);
    };
    if let Some(limit) = parse_limit(&raw) {
        return limit;
    }
    warn!("ignoring {name}={raw}: expected whole seconds, or `0`/`off`/`none` to disable");
    Some(fallback)
}

/// Parse a configured budget: `Some(None)` disables the limit, `Some(Some(d))`
/// sets it, `None` means the value was not understood.
#[must_use]
pub fn parse_limit(raw: &str) -> Option<Option<Duration>> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("off") || raw.eq_ignore_ascii_case("none") {
        return Some(None);
    }
    let seconds: u64 = raw.parse().ok()?;
    if seconds == 0 {
        return Some(None);
    }
    Some(Some(Duration::from_secs(seconds)))
}

#[cfg(test)]
mod tests {
    use super::{
        Budget, DEFAULT_HOOK_TIMEOUT, DEFAULT_WORKSPACE_HOOK_TIMEOUT, STILL_RUNNING_AFTER, budget_for, parse_limit,
    };
    use crate::model::Hook;
    use std::time::Duration;

    #[test]
    fn per_file_hook_gets_the_per_file_default() {
        let budget = budget_for(&Hook::run("fmt", "cargo fmt --check"));
        assert_eq!(budget.limit, Some(DEFAULT_HOOK_TIMEOUT));
        assert_eq!(budget.announce_after, Some(STILL_RUNNING_AFTER));
    }

    #[test]
    fn whole_project_hook_gets_the_longer_default() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.workspace = true;
        assert_eq!(budget_for(&hook).limit, Some(DEFAULT_WORKSPACE_HOOK_TIMEOUT));
        assert!(DEFAULT_WORKSPACE_HOOK_TIMEOUT > DEFAULT_HOOK_TIMEOUT);
    }

    #[test]
    fn explicit_hook_timeout_wins_over_the_shape_default() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.workspace = true;
        hook.timeout = Some(Duration::from_secs(7));
        assert_eq!(budget_for(&hook).limit, Some(Duration::from_secs(7)));
    }

    #[test]
    fn parse_limit_reads_seconds_and_disable_words() {
        assert_eq!(parse_limit("90"), Some(Some(Duration::from_secs(90))));
        assert_eq!(parse_limit(" 90 "), Some(Some(Duration::from_secs(90))));
        assert_eq!(parse_limit("0"), Some(None));
        assert_eq!(parse_limit("off"), Some(None));
        assert_eq!(parse_limit("NONE"), Some(None));
    }

    #[test]
    fn parse_limit_rejects_values_it_cannot_understand() {
        assert_eq!(parse_limit("soon"), None);
        assert_eq!(parse_limit("-5"), None);
        assert_eq!(parse_limit("1.5"), None);
        assert_eq!(parse_limit(""), None);
    }

    #[test]
    fn an_unlimited_budget_skips_supervision() {
        assert!(!Budget::unlimited().is_supervised());
        assert!(budget_for(&Hook::run("fmt", "true")).is_supervised());
    }
}
