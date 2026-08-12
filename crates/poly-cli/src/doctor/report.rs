//! The `poly doctor` findings: what is running, what `PATH` resolves to, which
//! config is in effect, and where the cache lives.
//!
//! Assembly is separated from rendering so the same findings drive the human
//! report, the JSON report, and the exit code.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::probe::{Probe, probe_version};
use super::shadow::{PathEntry, Resolution};

/// How bad a finding is. Only [`Severity::Error`] fails the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Actively wrong: acting on this poly's output can mislead.
    Error,
    /// Worth knowing, but the installation is usable.
    Warning,
    /// Context a bug report should carry.
    Note,
}

/// One diagnosed finding, with the command that fixes it where one exists.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// How bad it is.
    pub severity: Severity,
    /// What is wrong, in one line.
    pub summary: String,
    /// The concrete remedy, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

impl Finding {
    /// An error-severity finding with a remedy.
    fn error(summary: impl Into<String>, remedy: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Error,
            summary: summary.into(),
            remedy: Some(remedy.into()),
        }
    }

    /// A note carrying context but no action.
    fn note(summary: impl Into<String>) -> Self {
        Finding {
            severity: Severity::Note,
            summary: summary.into(),
            remedy: None,
        }
    }
}

/// The running executable's own identity.
#[derive(Debug, Clone, Serialize)]
pub struct RunningBinary {
    /// Resolved path of the running executable, symlinks followed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<PathBuf>,
    /// Workspace version (e.g. `0.19.7`).
    pub version: &'static str,
    /// Build identifier — `git describe` at compile time.
    pub build_id: &'static str,
    /// `release`, `dev`, or `unknown`.
    pub channel: &'static str,
    /// Cargo profile the binary was built with.
    pub profile: &'static str,
    /// Commit it was built from, when built inside a checkout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<&'static str>,
}

impl RunningBinary {
    /// Read the identity compiled into this binary.
    fn detect(executable: Option<PathBuf>) -> Self {
        RunningBinary {
            executable,
            version: poly_buildinfo::VERSION,
            build_id: poly_buildinfo::build_id(),
            channel: poly_buildinfo::channel().as_str(),
            profile: poly_buildinfo::profile(),
            commit: poly_buildinfo::commit(),
        }
    }
}

/// One `poly` on `PATH`, with what it said when asked its version.
#[derive(Debug, Clone, Serialize)]
pub struct PathInstall {
    /// Position on `PATH`, 1-based, as a reader would count it.
    pub order: usize,
    /// Path as `PATH` spells it.
    pub path: PathBuf,
    /// Resolved path, when it differs from `path` (a symlinked install).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<PathBuf>,
    /// Whether this entry is the running executable.
    pub running: bool,
    /// What `--version` reported, or why it could not. Flattened, so JSON reads
    /// `{"outcome": "reported", "version": "…"}` alongside the path rather than
    /// burying the answer a level down.
    #[serde(flatten)]
    pub probe: Probe,
}

/// Every `poly` on `PATH` and how they relate to the running one.
#[derive(Debug, Clone, Serialize)]
pub struct PathReport {
    /// Installs in `PATH` order.
    pub installs: Vec<PathInstall>,
    /// Whether the running executable is itself reachable through `PATH`.
    pub running_on_path: bool,
    /// Number of installs ahead of the running one on `PATH`.
    pub shadowing: usize,
    /// Number of installs the running one hides.
    pub shadowed: usize,
}

/// Config files in effect for the current working directory.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigReport {
    /// The config file that applies, if any was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// A sibling `poly.local.toml`, which layers on top.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_override: Option<PathBuf>,
    /// The `poly-config.lock` pinning remote `extends` bases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<PathBuf>,
    /// Whether the config loaded cleanly.
    pub loaded: bool,
    /// Why it did not load, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where cached results live.
#[derive(Debug, Clone, Serialize)]
pub struct CacheReport {
    /// Resolved cache root, when it could be resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// Why it could not be resolved, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The complete `poly doctor` report.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// The binary producing this report.
    pub running: RunningBinary,
    /// Every `poly` on `PATH`.
    pub path: PathReport,
    /// Config in effect.
    pub config: ConfigReport,
    /// Cache location.
    pub cache: CacheReport,
    /// Everything diagnosed, worst first.
    pub findings: Vec<Finding>,
}

impl DoctorReport {
    /// Whether any finding is error-severity — the exit-code signal.
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }
}

/// Collect everything `poly doctor` reports, probing each `poly` on `PATH`.
///
/// `explicit_config` mirrors the `--config` flag: when given, that file is the
/// config in effect instead of the discovered one.
pub fn collect(explicit_config: Option<&Path>) -> DoctorReport {
    let resolution = Resolution::detect();
    let running = RunningBinary::detect(resolution.running.clone());
    let path = build_path_report(&resolution);
    let config = build_config_report(explicit_config);
    let cache = build_cache_report();
    let findings = diagnose(&running, &resolution, &path, &config, &cache);
    DoctorReport {
        running,
        path,
        config,
        cache,
        findings,
    }
}

/// Probe every `poly` on `PATH` and record its position relative to the running
/// executable.
fn build_path_report(resolution: &Resolution) -> PathReport {
    let installs = resolution
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| PathInstall {
            order: index + 1,
            path: entry.path.clone(),
            resolved: (entry.canonical != entry.path).then(|| entry.canonical.clone()),
            running: resolution.running_index == Some(index),
            probe: probe_version(&entry.path),
        })
        .collect();
    PathReport {
        installs,
        running_on_path: resolution.resolved_from_path(),
        shadowing: resolution.shadowing().len(),
        shadowed: resolution.shadowed().len(),
    }
}

/// Locate the config in effect and confirm it actually parses.
fn build_config_report(explicit: Option<&Path>) -> ConfigReport {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let path = match explicit {
        Some(path) => Some(path.to_path_buf()),
        None => poly_config::find_config(&cwd),
    };
    let local_override = path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|dir| dir.join(poly_config::LOCAL_OVERRIDE_NAME))
        .filter(|p| p.is_file());
    let lock = crate::config_sources::repo_root()
        .ok()
        .map(|root| root.join("poly-config.lock"))
        .filter(|p| p.is_file());

    // Load through the same resolver the real commands use, so a broken remote
    // `extends` base surfaces here rather than on the next lint.
    let loaded = load_config(explicit, &cwd);
    let (loaded_ok, error) = match loaded {
        Ok(()) => (true, None),
        Err(error) => (false, Some(format!("{error:#}"))),
    };
    ConfigReport {
        path,
        local_override,
        lock,
        loaded: loaded_ok,
        error,
    }
}

/// Load the config exactly as `poly lint` does, discarding the value — we only
/// care whether it loads.
fn load_config(explicit: Option<&Path>, cwd: &Path) -> anyhow::Result<()> {
    let resolver = crate::config_sources::resolver()?;
    match explicit {
        Some(path) => poly_config::PolyConfig::load_file_with(path, &resolver)?,
        None => poly_config::PolyConfig::load_with(cwd, &resolver)?,
    };
    Ok(())
}

/// Resolve the cache root the way `poly cache` does.
fn build_cache_report() -> CacheReport {
    match crate::cache_cmd::resolve_root(None) {
        Ok(directory) => CacheReport {
            directory: Some(directory),
            error: None,
        },
        Err(error) => CacheReport {
            directory: None,
            error: Some(format!("{error:#}")),
        },
    }
}

/// Turn the collected facts into findings, worst first.
fn diagnose(
    running: &RunningBinary,
    resolution: &Resolution,
    path: &PathReport,
    config: &ConfigReport,
    cache: &CacheReport,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    diagnose_running(running, &mut findings);
    diagnose_path(resolution, path, &mut findings);
    diagnose_config(config, &mut findings);
    if let Some(error) = &cache.error {
        findings.push(Finding {
            severity: Severity::Warning,
            summary: format!("the cache directory could not be resolved: {error}"),
            remedy: Some("pin one with `[cache] dir` in poly.toml or POLY_CACHE_HOME".to_string()),
        });
    }
    findings.sort_by_key(|finding| match finding.severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Note => 2,
    });
    findings
}

/// Findings about the running executable's own identity.
fn diagnose_running(running: &RunningBinary, findings: &mut Vec<Finding>) {
    if running.executable.is_none() {
        findings.push(Finding::error(
            "the running executable's own path could not be determined",
            "report this with your OS and how poly was invoked",
        ));
    }
    match running.channel {
        "release" => {}
        "dev" => findings.push(Finding::note(format!(
            "this is a development build ({}), not the released {} — quote the build id, not the version, in bug reports",
            running.build_id, running.version
        ))),
        _ => findings.push(Finding::note(format!(
            "this binary's provenance is unknown (built outside a git checkout); it reports version {} but cannot prove which build it is",
            running.version
        ))),
    }
}

/// Findings about competing installs on `PATH`.
fn diagnose_path(resolution: &Resolution, path: &PathReport, findings: &mut Vec<Finding>) {
    if path.installs.is_empty() {
        findings.push(Finding {
            severity: Severity::Warning,
            summary: "no `poly` was found on PATH".to_string(),
            remedy: Some("add the install directory to PATH so `poly` resolves by name".to_string()),
        });
    }
    for install in &path.installs {
        if install.probe.is_failure() {
            findings.push(Finding::error(
                format!("{} {}", install.path.display(), install.probe.display()),
                format!("reinstall or remove it: {}", install.path.display()),
            ));
        }
    }
    for entry in resolution.shadowing() {
        findings.push(Finding::error(
            format!(
                "{} comes earlier on PATH than the running executable — a bare `poly` does not run this binary",
                entry.path.display()
            ),
            entry.removal_hint(),
        ));
    }
    for entry in resolution.shadowed() {
        findings.push(Finding::error(
            format!(
                "the running executable hides {} — upgrading that install changes nothing a bare `poly` observes",
                entry.path.display()
            ),
            shadowed_remedy(resolution, entry),
        ));
    }
    if !path.running_on_path && !path.installs.is_empty() {
        let first = resolution
            .first()
            .map(|entry| entry.path.display().to_string())
            .unwrap_or_default();
        findings.push(Finding::note(format!(
            "the running executable is not on PATH; a bare `poly` would run {first}"
        )));
    }
}

/// The remedy for a hidden install: remove whichever of the two you do not want.
fn shadowed_remedy(resolution: &Resolution, hidden: &PathEntry) -> String {
    match resolution.running_index.and_then(|index| resolution.entries.get(index)) {
        Some(running) => format!(
            "keep one install — to use {}, remove the one in front of it: {}",
            hidden.path.display(),
            running.removal_hint()
        ),
        None => hidden.removal_hint(),
    }
}

/// Findings about the config in effect.
fn diagnose_config(config: &ConfigReport, findings: &mut Vec<Finding>) {
    if let Some(error) = &config.error {
        findings.push(Finding::error(
            format!("the config in effect could not be loaded: {error}"),
            "fix the reported file, or pass --config to point at a different one",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(channel: &'static str) -> RunningBinary {
        RunningBinary {
            executable: Some(PathBuf::from("/usr/local/bin/poly")),
            version: "0.19.7",
            build_id: "v0.19.7",
            channel,
            profile: "release",
            commit: None,
        }
    }

    #[test]
    fn a_release_build_with_one_install_reports_no_errors() {
        let mut findings = Vec::new();
        diagnose_running(&running("release"), &mut findings);
        assert!(findings.is_empty(), "a clean release build is silent: {findings:?}");
    }

    #[test]
    fn a_dev_build_is_noted_but_does_not_fail() {
        let mut findings = Vec::new();
        diagnose_running(&running("dev"), &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Note, "a dev build is not a defect");
        assert!(findings[0].summary.contains("development build"));
    }

    #[test]
    fn an_unloadable_config_is_an_error() {
        let config = ConfigReport {
            path: Some(PathBuf::from("/repo/poly.toml")),
            local_override: None,
            lock: None,
            loaded: false,
            error: Some("expected `=`".to_string()),
        };
        let mut findings = Vec::new();
        diagnose_config(&config, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].remedy.is_some(), "an unloadable config has a remedy");
    }

    #[test]
    fn a_readable_config_produces_no_finding() {
        let config = ConfigReport {
            path: Some(PathBuf::from("/repo/poly.toml")),
            local_override: None,
            lock: None,
            loaded: true,
            error: None,
        };
        let mut findings = Vec::new();
        diagnose_config(&config, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn has_errors_tracks_only_error_severity() {
        let report = |severity| DoctorReport {
            running: running("release"),
            path: PathReport {
                installs: Vec::new(),
                running_on_path: true,
                shadowing: 0,
                shadowed: 0,
            },
            config: ConfigReport {
                path: None,
                local_override: None,
                lock: None,
                loaded: true,
                error: None,
            },
            cache: CacheReport {
                directory: None,
                error: None,
            },
            findings: vec![Finding {
                severity,
                summary: "x".to_string(),
                remedy: None,
            }],
        };
        assert!(report(Severity::Error).has_errors());
        assert!(!report(Severity::Warning).has_errors());
        assert!(!report(Severity::Note).has_errors());
    }
}
