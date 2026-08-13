//! Time budgets for everything a hook run spawns.
//!
//! A hook that never returns used to hang the commit forever with no output at
//! all: no way to tell whether it was running, wedged, or skipped. Every
//! process a run spawns — hook bodies, stage and hook `before`/`after` steps,
//! and `precondition` probes — therefore runs under a [`Budget`]: a limit past
//! which poly kills it and reports [`crate::model::HookStatus::TimedOut`], plus
//! the cadence at which a still-running process announces itself so a hang
//! names its culprit long before it is killed.
//!
//! # Choosing the defaults
//!
//! Too low a default turns a working setup into a broken one, so the defaults
//! are hang detectors, not performance budgets, and they differ by what is
//! being run:
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
//! - A **setup step** (`before` / `after`, stage-level or per-hook) installs or
//!   prepares something — `npm ci`, `./gradlew --version`, a cache warm. That
//!   is bounded by dependency installation, not by compiling the workspace, so
//!   [`DEFAULT_STEP_TIMEOUT`] reuses the per-file hook's 10 minutes rather than
//!   the whole-project 30.
//! - A **precondition** is an applicability probe — `test -f gradlew`,
//!   `command -v cargo`. It answers a question about the environment, and a
//!   probe that needs minutes is not a probe. [`DEFAULT_PRECONDITION_TIMEOUT`]
//!   (60 seconds) still leaves ~60× headroom over any local probe and covers a
//!   network-touching one (`gh auth status`, `docker info`) on a slow link,
//!   while bounding a wedged one inside a minute — which matters more here than
//!   anywhere else, because a stage `precondition` gates *every* hook in the
//!   stage.
//!
//! # Precedence
//!
//! This is the one place the resolution order is defined; every other mention
//! (the `poly.toml` `timeout` key, the ADR, the README) refers back to it.
//!
//! ```text
//! environment override  →  explicit per-hook budget  →  shape default
//! ```
//!
//! The **environment wins** over an explicitly configured budget. It is the
//! escape hatch of whoever is actually running the hooks, and they are the only
//! party who knows how fast that machine is; a config author cannot, and the
//! operator frequently cannot edit the config at all (a CI checkout, a fork, a
//! base config pulled in via `extends`). Decisively: the disable form has to be
//! *total* — `POLY_HOOK_TIMEOUT=0` must restore unbounded behaviour for every
//! hook, and it would fail exactly on the hook being killed if that hook's own
//! budget outranked it. The cost is that a blanket override also flattens a
//! budget somebody deliberately widened; the report always names the effective
//! limit, so the reader can see which one applied.
//!
//! Accepted values are the same everywhere ([`ACCEPTED_TIMEOUT_FORMS`]): whole
//! seconds, a suffixed duration (`500ms`, `30s`, `10m`, `1h`), or `0` / `off` /
//! `none` to disable. Disabling is not "an enormous limit": it restores the
//! previous, un-supervised execution path exactly — no deadline, no liveness
//! notice, and no separate process group (so `Ctrl-C` reaches children again).

use std::time::Duration;

use tracing::warn;

use crate::consts::env_vars::EnvVars;
use crate::model::Hook;

/// Default budget for a per-file hook.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_mins(10);

/// Default budget for a whole-project ([`Hook::workspace`]) hook.
pub const DEFAULT_WORKSPACE_HOOK_TIMEOUT: Duration = Duration::from_mins(30);

/// Default budget for a `before` / `after` setup step, stage-level or per-hook.
pub const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_mins(10);

/// Default budget for a `precondition` probe (one minute).
pub const DEFAULT_PRECONDITION_TIMEOUT: Duration = Duration::from_mins(1);

/// How long a process must run before it announces that it is still running.
///
/// Short enough that a hang names its culprit while the author is still
/// watching, long enough that an ordinary run stays silent.
pub const STILL_RUNNING_AFTER: Duration = Duration::from_secs(15);

/// How often a still-running process repeats its announcement after the first.
pub const STILL_RUNNING_EVERY: Duration = Duration::from_mins(1);

/// The fraction of a hook's own budget poly will spend waiting, before spawning
/// it, for a lock held outside the run (see [`crate::cargo_lock`]).
///
/// Two, so the wait can never take more than half of what the hook was allowed:
/// a hook that waits out the whole bound and then overruns is still killed
/// inside 1.5× its configured limit, which keeps the promise that a run is
/// bounded while leaving the majority of the tolerance for the hook's own work.
pub const LOCK_WAIT_BUDGET_DIVISOR: u32 = 2;

/// How long a pre-spawn lock wait runs before it announces itself.
///
/// Much shorter than [`STILL_RUNNING_AFTER`], and deliberately so: a hook that
/// is *running* is the normal case and should stay quiet, whereas a hook poly is
/// deliberately holding back is not normal at all. Anything above a second or
/// two of poly doing nothing must say why, or it is indistinguishable from the
/// silent hang the timeouts exist to prevent.
pub const LOCK_WAIT_ANNOUNCE_AFTER: Duration = Duration::from_secs(2);

/// Environment variable overriding the budget of a per-file hook.
pub const HOOK_TIMEOUT_ENV: &str = "POLY_HOOK_TIMEOUT";

/// Environment variable overriding the budget of a whole-project hook.
pub const WORKSPACE_HOOK_TIMEOUT_ENV: &str = "POLY_HOOK_WORKSPACE_TIMEOUT";

/// Environment variable overriding the budget of a `before` / `after` step.
pub const STEP_TIMEOUT_ENV: &str = "POLY_HOOK_STEP_TIMEOUT";

/// Environment variable overriding the budget of a `precondition` probe.
pub const PRECONDITION_TIMEOUT_ENV: &str = "POLY_HOOK_PRECONDITION_TIMEOUT";

/// The value forms every timeout surface accepts, phrased for an error message.
///
/// Shared by the environment-variable warning and the `poly.toml` lowering
/// error so the two can never describe different grammars.
pub const ACCEPTED_TIMEOUT_FORMS: &str =
    "whole seconds (`90`), a duration (`500ms`, `30s`, `10m`, `1h`), or `0`/`off`/`none` to disable";

/// A configured time budget: unset, explicitly unbounded, or an explicit limit.
///
/// The three states are distinct because "not configured" and "configured to be
/// unbounded" resolve differently — the first falls through to the shape
/// default, the second is the escape hatch and must survive it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HookTimeout {
    /// No explicit budget; use the default for this hook's shape.
    #[default]
    Default,
    /// Explicitly unbounded (`0` / `off` / `none`).
    Disabled,
    /// An explicit limit.
    Limit(Duration),
}

impl HookTimeout {
    /// Resolve to a kill deadline, `fallback` supplying the shape default.
    #[must_use]
    pub fn limit(self, fallback: Duration) -> Option<Duration> {
        match self {
            Self::Default => Some(fallback),
            Self::Disabled => None,
            Self::Limit(limit) => Some(limit),
        }
    }
}

/// The time budget one spawned process runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Kill the process once it has run this long; `None` never kills.
    pub limit: Option<Duration>,
    /// Announce that the process is still running after this long; `None` never
    /// announces.
    pub announce_after: Option<Duration>,
    /// Repeat the announcement at this interval.
    pub announce_every: Duration,
}

impl Budget {
    /// A budget that neither kills nor announces — the pre-timeout behaviour,
    /// which is what a disabled budget resolves to.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            limit: None,
            announce_after: None,
            announce_every: STILL_RUNNING_EVERY,
        }
    }

    /// A budget that kills at `limit` and announces on the standard cadence.
    #[must_use]
    pub const fn bounded(limit: Duration) -> Self {
        Self {
            limit: Some(limit),
            announce_after: Some(STILL_RUNNING_AFTER),
            announce_every: STILL_RUNNING_EVERY,
        }
    }

    /// The budget for a resolved limit: bounded when there is one, fully
    /// unlimited when there is not.
    ///
    /// Disabling drops the liveness notice as well as the deadline on purpose:
    /// the promise is that the escape hatch restores the previous execution
    /// path exactly, and [`Self::is_supervised`] is what routes back to it.
    #[must_use]
    fn from_limit(limit: Option<Duration>) -> Self {
        limit.map_or_else(Self::unlimited, Self::bounded)
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

/// The budget `hook` runs under.
///
/// Resolution order is the module-level one: environment override, then the
/// hook's own [`Hook::timeout`], then the shape default.
#[must_use]
pub fn budget_for(hook: &Hook) -> Budget {
    let (env, fallback) = if hook.workspace {
        (WORKSPACE_HOOK_TIMEOUT_ENV, DEFAULT_WORKSPACE_HOOK_TIMEOUT)
    } else {
        (HOOK_TIMEOUT_ENV, DEFAULT_HOOK_TIMEOUT)
    };
    Budget::from_limit(resolve(env, hook.timeout, fallback))
}

/// The budget a `before` / `after` step runs under.
///
/// Steps carry no per-step configuration: they are setup, not checks, and a
/// per-step key would be schema surface for a knob nobody has needed. The
/// environment override is the escape hatch.
#[must_use]
pub fn step_budget() -> Budget {
    Budget::from_limit(resolve(STEP_TIMEOUT_ENV, HookTimeout::Default, DEFAULT_STEP_TIMEOUT))
}

/// The budget a `precondition` probe runs under.
#[must_use]
pub fn precondition_budget() -> Budget {
    Budget::from_limit(resolve(
        PRECONDITION_TIMEOUT_ENV,
        HookTimeout::Default,
        DEFAULT_PRECONDITION_TIMEOUT,
    ))
}

/// Apply the precedence chain: `env` (when set and understood) wins over
/// `configured`, which wins over `fallback`.
fn resolve(env: &str, configured: HookTimeout, fallback: Duration) -> Option<Duration> {
    env_timeout(env).unwrap_or(configured).limit(fallback)
}

/// The timeout an environment variable asks for, or `None` when it is unset or
/// unreadable.
///
/// A value that cannot be parsed is **ignored with a warning**, never read as
/// "disabled": failing open on a typo would silently reinstate the hang this
/// whole mechanism exists to bound.
fn env_timeout(name: &str) -> Option<HookTimeout> {
    let raw = EnvVars::var(name).ok()?;
    let parsed = parse_timeout(&raw);
    if parsed.is_none() {
        warn!("ignoring {name}={raw}: expected {ACCEPTED_TIMEOUT_FORMS}");
    }
    parsed
}

/// Parse a configured budget; `None` means the value was not understood.
///
/// Accepts a bare integer (seconds), an integer with a `ms` / `s` / `m` / `h`
/// suffix, and `0` / `off` / `none` for [`HookTimeout::Disabled`]. Fractions are
/// deliberately rejected: `1.5m` invites a rounding question no reader should
/// have to ask, and `90s` says the same thing exactly.
#[must_use]
pub fn parse_timeout(raw: &str) -> Option<HookTimeout> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("off") || raw.eq_ignore_ascii_case("none") {
        return Some(HookTimeout::Disabled);
    }
    let digits = raw.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let unit = raw[digits.len()..].to_ascii_lowercase();
    let value: u64 = digits.parse().ok()?;
    if value == 0 {
        return Some(HookTimeout::Disabled);
    }
    let duration = match unit.as_str() {
        "ms" => Duration::from_millis(value),
        "" | "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value.checked_mul(60)?),
        "h" => Duration::from_secs(value.checked_mul(3600)?),
        _ => return None,
    };
    Some(HookTimeout::Limit(duration))
}

#[cfg(test)]
mod tests {
    use super::{
        Budget, DEFAULT_HOOK_TIMEOUT, DEFAULT_PRECONDITION_TIMEOUT, DEFAULT_STEP_TIMEOUT,
        DEFAULT_WORKSPACE_HOOK_TIMEOUT, HookTimeout, STILL_RUNNING_AFTER, budget_for, parse_timeout,
        precondition_budget, step_budget,
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
        hook.timeout = HookTimeout::Limit(Duration::from_secs(7));
        assert_eq!(budget_for(&hook).limit, Some(Duration::from_secs(7)));
    }

    #[test]
    fn a_disabled_hook_timeout_is_unbounded_and_unsupervised() {
        let mut hook = Hook::run("slow", "sleep 30");
        hook.timeout = HookTimeout::Disabled;
        let budget = budget_for(&hook);
        assert_eq!(budget.limit, None);
        assert!(!budget.is_supervised(), "disabling restores the un-supervised path");
    }

    #[test]
    fn setup_steps_and_probes_have_their_own_defaults() {
        assert_eq!(step_budget().limit, Some(DEFAULT_STEP_TIMEOUT));
        assert_eq!(precondition_budget().limit, Some(DEFAULT_PRECONDITION_TIMEOUT));
        assert!(
            DEFAULT_PRECONDITION_TIMEOUT < DEFAULT_STEP_TIMEOUT,
            "a probe that needs minutes is not a probe"
        );
    }

    #[test]
    fn parse_timeout_reads_seconds_suffixes_and_disable_words() {
        assert_eq!(parse_timeout("90"), Some(HookTimeout::Limit(Duration::from_secs(90))));
        assert_eq!(parse_timeout(" 90 "), Some(HookTimeout::Limit(Duration::from_secs(90))));
        assert_eq!(
            parse_timeout("500ms"),
            Some(HookTimeout::Limit(Duration::from_millis(500)))
        );
        assert_eq!(parse_timeout("30S"), Some(HookTimeout::Limit(Duration::from_secs(30))));
        assert_eq!(parse_timeout("10m"), Some(HookTimeout::Limit(Duration::from_mins(10))));
        assert_eq!(parse_timeout("1h"), Some(HookTimeout::Limit(Duration::from_hours(1))));
        assert_eq!(parse_timeout("0"), Some(HookTimeout::Disabled));
        assert_eq!(parse_timeout("0s"), Some(HookTimeout::Disabled));
        assert_eq!(parse_timeout("off"), Some(HookTimeout::Disabled));
        assert_eq!(parse_timeout("NONE"), Some(HookTimeout::Disabled));
    }

    #[test]
    fn parse_timeout_rejects_values_it_cannot_understand() {
        assert_eq!(parse_timeout("soon"), None);
        assert_eq!(parse_timeout("-5"), None);
        assert_eq!(parse_timeout("1.5"), None);
        assert_eq!(parse_timeout("1.5m"), None);
        assert_eq!(parse_timeout("30 s"), None);
        assert_eq!(parse_timeout("30x"), None);
        assert_eq!(parse_timeout(""), None);
    }

    #[test]
    fn an_unlimited_budget_skips_supervision() {
        assert!(!Budget::unlimited().is_supervised());
        assert!(budget_for(&Hook::run("fmt", "true")).is_supervised());
    }
}
