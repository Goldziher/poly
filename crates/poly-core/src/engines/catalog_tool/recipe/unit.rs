//! What differs from one project root to the next for a recipe tool: the values
//! files a chart is checked with, the charts the tool skips, and the prepare
//! step (`helm dependency build`) that makes a root checkable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use globset::{Glob, GlobMatcher};
use poly_catalog::{Recipe, Tool};
use poly_config::ToolConfig;

use super::argv::Template;
use super::paths;

/// The per-root options of one recipe tool, resolved from its recipe and config.
pub(super) struct UnitOptions {
    values_flag: Option<String>,
    /// Absolute paths passed for every root.
    values_files: Vec<PathBuf>,
    /// Chart-directory glob and the absolute values files it adds.
    chart_values: Vec<(String, GlobMatcher, Vec<PathBuf>)>,
    skip: Vec<(String, GlobMatcher)>,
    /// The prepare command's argv, ready to run in the root.
    prepare: Option<Vec<String>>,
    retry: Retry,
    /// Whether a skipped chart is still run, to catch a skip that no longer holds.
    verify_skips: bool,
}

const DEFAULT_ATTEMPTS: u32 = 5;
const DEFAULT_BACKOFF_SECS: u64 = 4;
const DEFAULT_JITTER_SECS: u64 = 4;

/// How a failing prepare step is retried: the wait after the n-th failure is
/// `backoff_secs * 2^(n-1)` plus up to `jitter_secs` at random.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Retry {
    attempts: u32,
    backoff_secs: u64,
    jitter_secs: u64,
}

impl Retry {
    fn from_config(config: &ToolConfig) -> Self {
        Retry {
            attempts: config.prepare_attempts.unwrap_or(DEFAULT_ATTEMPTS).max(1),
            backoff_secs: config.prepare_backoff_secs.unwrap_or(DEFAULT_BACKOFF_SECS),
            jitter_secs: config.prepare_jitter_secs.unwrap_or(DEFAULT_JITTER_SECS),
        }
    }

    /// The wait after the `failed`-th consecutive failure; `entropy` picks the jitter.
    fn delay(&self, failed: u32, entropy: u64) -> Duration {
        let doubled = self
            .backoff_secs
            .saturating_mul(1_u64 << failed.saturating_sub(1).min(32));
        let jitter = entropy % self.jitter_secs.saturating_add(1);
        Duration::from_secs(doubled.saturating_add(jitter))
    }

    /// Run `step` until it succeeds or the attempts are spent, calling `wait`
    /// between attempts. Returns the last failure.
    pub(super) fn run(
        &self,
        mut step: impl FnMut() -> Result<(), String>,
        mut wait: impl FnMut(Duration),
    ) -> Result<(), String> {
        let mut failed = 0;
        loop {
            match step() {
                Ok(()) => return Ok(()),
                Err(reason) => {
                    failed += 1;
                    if failed >= self.attempts {
                        return Err(reason);
                    }
                    wait(self.delay(failed, clock_entropy()));
                }
            }
        }
    }
}

fn clock_entropy() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()))
}

fn matcher(pattern: &str) -> Result<GlobMatcher, String> {
    Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|error| format!("invalid chart glob `{pattern}`: {error}"))
}

fn absolute_files(cwd: &Path, files: &[String]) -> Vec<PathBuf> {
    files.iter().map(|file| paths::absolute(cwd, Path::new(file))).collect()
}

impl UnitOptions {
    /// Resolve the options.
    ///
    /// # Errors
    ///
    /// `Err` carries a message for a config key the tool's recipe cannot honour
    /// (values files for a tool with no values flag), an invalid glob, or a
    /// prepare command the catalog does not define.
    pub(super) fn new(tool: &Tool, recipe: &Recipe, config: &ToolConfig, cwd: &Path) -> Result<Self, String> {
        let wants_values = !config.values_files.is_empty() || !config.chart_values.is_empty();
        if wants_values && recipe.values_flag.is_none() {
            return Err(format!(
                "`values_files` and `chart_values` are not supported by tool `{}`",
                tool.name
            ));
        }
        let mut chart_values = Vec::new();
        for (pattern, files) in &config.chart_values {
            chart_values.push((pattern.clone(), matcher(pattern)?, absolute_files(cwd, files)));
        }
        let mut skip = Vec::new();
        for pattern in &config.skip_charts {
            skip.push((pattern.clone(), matcher(pattern)?));
        }
        let prepare = match (&recipe.prepare, config.prepare) {
            (Some(name), Some(true) | None) => {
                let command = tool
                    .command(name)
                    .ok_or_else(|| format!("tool `{}` names the missing prepare command `{name}`", tool.name))?;
                Some(Template::new(&command.arguments, &[]).render(&[]))
            }
            _ => None,
        };
        Ok(UnitOptions {
            values_flag: recipe.values_flag.clone(),
            values_files: absolute_files(cwd, &config.values_files),
            chart_values,
            skip,
            prepare,
            retry: Retry::from_config(config),
            verify_skips: config.verify_skip_charts.unwrap_or(true),
        })
    }

    /// Whether the tool does not run on the root `dir` (relative to the working
    /// directory, slashed).
    pub(super) fn skips(&self, dir: &str) -> bool {
        self.skip.iter().any(|(_, matcher)| matcher.is_match(dir))
    }

    /// Whether a skipped chart is still run, and fails the run when the tool
    /// now succeeds on it.
    pub(super) fn verifies_skips(&self) -> bool {
        self.verify_skips
    }

    /// How the prepare step is retried.
    pub(super) fn retry(&self) -> Retry {
        self.retry
    }

    /// The arguments that follow the rendered argv for the root `dir`.
    pub(super) fn extra_arguments(&self, dir: &str) -> Vec<String> {
        let Some(flag) = &self.values_flag else {
            return Vec::new();
        };
        let matched = self
            .chart_values
            .iter()
            .filter(|(_, matcher, _)| matcher.is_match(dir))
            .flat_map(|(_, _, files)| files);
        self.values_files
            .iter()
            .chain(matched)
            .flat_map(|file| [flag.clone(), file.to_string_lossy().into_owned()])
            .collect()
    }

    /// Every values file, for the config fingerprint: editing one changes the
    /// verdict of the charts that use it.
    pub(super) fn files(&self) -> Vec<PathBuf> {
        self.values_files
            .iter()
            .chain(self.chart_values.iter().flat_map(|(_, _, files)| files))
            .cloned()
            .collect()
    }

    /// The options as a string for the engine's cache key.
    pub(super) fn fingerprint(&self) -> String {
        format!(
            "values_flag={:?}:values={:?}:chart_values={:?}:skip={:?}:prepare={:?}:verify_skips={}",
            self.values_flag,
            self.values_files,
            self.chart_values
                .iter()
                .map(|(pattern, _, files)| (pattern, files))
                .collect::<Vec<_>>(),
            self.skip.iter().map(|(pattern, _)| pattern).collect::<Vec<_>>(),
            self.prepare,
            self.verify_skips,
        )
    }

    /// The prepare command's argv, when the tool has one.
    pub(super) fn prepare_argv(&self) -> Option<&[String]> {
        self.prepare.as_deref()
    }
}

/// Whether the process was asked not to touch the network.
pub(super) fn offline() -> bool {
    offline_from(std::env::var("POLY_OFFLINE").ok().as_deref())
}

fn offline_from(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false"))
}

/// A dependency a chart declares: its name and, when locked, the version.
#[derive(Debug, PartialEq, Eq)]
struct Dependency {
    name: String,
    version: Option<String>,
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches(['"', '\'']).to_owned()
}

/// The entries of the top-level `dependencies:` list of a `Chart.yaml`,
/// `requirements.yaml` or `Chart.lock`. A line scan, not a YAML parse: the
/// `maintainers:` list also has `- name:` entries, so only the block under
/// `dependencies:` counts.
fn dependencies_of(text: &str) -> Vec<Dependency> {
    let mut found: Vec<Dependency> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = line.starts_with([' ', '\t']);
        if !indented && !trimmed.starts_with('-') {
            inside = trimmed.starts_with("dependencies:");
            continue;
        }
        if !inside {
            continue;
        }
        let entry = trimmed.strip_prefix('-').map_or(trimmed, str::trim_start);
        if let Some(name) = entry.strip_prefix("name:") {
            found.push(Dependency {
                name: unquote(name),
                version: None,
            });
        } else if let Some(version) = entry.strip_prefix("version:")
            && let Some(last) = found.last_mut()
            && last.version.is_none()
        {
            last.version = Some(unquote(version));
        }
    }
    found
}

fn declared(root: &Path) -> Vec<Dependency> {
    ["Chart.yaml", "requirements.yaml"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(root.join(name)).ok())
        .flat_map(|text| dependencies_of(&text))
        .collect()
}

fn vendored(charts: &Path, dependency: &Dependency) -> bool {
    if charts.join(&dependency.name).is_dir() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(charts) else {
        return false;
    };
    let exact = dependency
        .version
        .as_ref()
        .map(|version| format!("{}-{version}.tgz", dependency.name));
    let prefix = format!("{}-", dependency.name);
    entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        match &exact {
            Some(exact) => name == *exact,
            None => name.starts_with(&prefix) && name.ends_with(".tgz"),
        }
    })
}

/// Whether the chart at `root` has nothing to build: it declares no
/// dependencies, or `charts/` already holds every one (at the locked version
/// when a `Chart.lock` pins it).
pub(super) fn dependencies_in_place(root: &Path) -> bool {
    let wanted = declared(root);
    if wanted.is_empty() {
        return true;
    }
    let locked = std::fs::read_to_string(root.join("Chart.lock"))
        .map(|text| dependencies_of(&text))
        .unwrap_or_default();
    let charts = root.join("charts");
    if locked.is_empty() {
        return wanted.iter().all(|dependency| vendored(&charts, dependency));
    }
    wanted.len() <= locked.len() && locked.iter().all(|dependency| vendored(&charts, dependency))
}

/// Outcome of one root's prepare step, shared by everything that asks.
type Prepared = Arc<OnceLock<Result<(), String>>>;

/// Run `step` once per `(root, argv)` for the process, however many tools or
/// files ask.
pub(super) fn once_per_root(root: &Path, argv: &[String], step: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    static MEMO: OnceLock<Mutex<HashMap<String, Prepared>>> = OnceLock::new();
    let key = format!("{}\u{1e}{}", root.display(), argv.join("\u{1f}"));
    let cell = {
        let mut memo = MEMO
            .get_or_init(Default::default)
            .lock()
            .expect("prepare memo poisoned");
        Arc::clone(memo.entry(key).or_default())
    };
    cell.get_or_init(step).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHART: &str = "apiVersion: v2\nname: web\nmaintainers:\n  - name: Someone\ndependencies:\n  - name: common\n    version: 1.2.0\n    repository: file://../common\n  - name: redis\n    version: \"19.0.1\"\n    repository: oci://x\nicon: y\n";

    #[test]
    fn only_the_dependencies_block_counts_not_the_maintainers() {
        let names: Vec<String> = dependencies_of(CHART).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["common", "redis"]);
    }

    #[test]
    fn a_lock_file_carries_the_versions() {
        let lock = "dependencies:\n- name: common\n  repository: file://../common\n  version: 1.2.0\ndigest: sha256:abc\ngenerated: \"2024\"\n";
        assert_eq!(
            dependencies_of(lock),
            [Dependency {
                name: "common".to_owned(),
                version: Some("1.2.0".to_owned())
            }]
        );
    }

    #[test]
    fn a_chart_without_dependencies_has_nothing_to_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Chart.yaml"), "name: web\n").expect("chart");
        assert!(dependencies_in_place(dir.path()));
    }

    #[test]
    fn dependencies_are_in_place_only_when_charts_holds_the_locked_versions() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Chart.yaml"), CHART).expect("chart");
        std::fs::write(
            dir.path().join("Chart.lock"),
            "dependencies:\n- name: common\n  version: 1.2.0\n- name: redis\n  version: 19.0.1\n",
        )
        .expect("lock");
        assert!(!dependencies_in_place(dir.path()), "no charts/ yet");
        std::fs::create_dir(dir.path().join("charts")).expect("charts");
        std::fs::write(dir.path().join("charts/common-1.2.0.tgz"), "").expect("common");
        assert!(!dependencies_in_place(dir.path()), "redis is missing");
        std::fs::write(dir.path().join("charts/redis-19.0.0.tgz"), "").expect("stale redis");
        assert!(!dependencies_in_place(dir.path()), "a stale version does not satisfy the lock");
        std::fs::write(dir.path().join("charts/redis-19.0.1.tgz"), "").expect("redis");
        assert!(dependencies_in_place(dir.path()));
    }

    fn retry(attempts: u32) -> Retry {
        Retry {
            attempts,
            backoff_secs: 4,
            jitter_secs: 4,
        }
    }

    #[test]
    fn the_wait_doubles_from_four_seconds_with_up_to_four_of_jitter() {
        let policy = retry(5);
        let waits: Vec<u64> = (1..=4).map(|failed| policy.delay(failed, 0).as_secs()).collect();
        assert_eq!(waits, [4, 8, 16, 32]);
        assert_eq!(policy.delay(1, 4).as_secs(), 8, "jitter 4 on a 4 second base");
        assert_eq!(policy.delay(1, 5).as_secs(), 4, "jitter wraps at jitter_secs + 1");
    }

    #[test]
    fn defaults_match_the_charts_script() {
        let retry = Retry::from_config(&ToolConfig::default());
        assert_eq!((retry.attempts, retry.backoff_secs, retry.jitter_secs), (5, 4, 4));
    }

    #[test]
    fn a_failing_step_is_attempted_five_times_with_four_waits() {
        let mut runs = 0;
        let mut waits = Vec::new();
        let outcome = retry(5).run(
            || {
                runs += 1;
                Err(format!("throttled {runs}"))
            },
            |wait| waits.push(wait.as_secs()),
        );
        assert_eq!(outcome, Err("throttled 5".to_owned()), "the last failure is reported");
        assert_eq!(runs, 5);
        assert_eq!(waits.len(), 4);
        assert!(waits.iter().zip([4, 8, 16, 32]).all(|(wait, base)| (base..=base + 4).contains(wait)));
    }

    #[test]
    fn a_step_that_recovers_stops_retrying() {
        let mut runs = 0;
        let mut waited = 0;
        let outcome = retry(5).run(
            || {
                runs += 1;
                if runs < 3 { Err("throttled".to_owned()) } else { Ok(()) }
            },
            |_| waited += 1,
        );
        assert_eq!(outcome, Ok(()));
        assert_eq!((runs, waited), (3, 2));
    }

    #[test]
    fn one_attempt_never_waits_and_zero_counts_as_one() {
        let config = ToolConfig {
            prepare_attempts: Some(0),
            ..ToolConfig::default()
        };
        let policy = Retry::from_config(&config);
        let mut runs = 0;
        let outcome = policy.run(
            || {
                runs += 1;
                Err("down".to_owned())
            },
            |_| panic!("no wait after the only attempt"),
        );
        assert_eq!(outcome, Err("down".to_owned()));
        assert_eq!(runs, 1);
    }

    #[test]
    fn offline_reads_the_environment_value() {
        assert!(!offline_from(None));
        assert!(!offline_from(Some("")));
        assert!(!offline_from(Some("0")));
        assert!(!offline_from(Some("false")));
        assert!(offline_from(Some("1")));
    }

    #[test]
    fn the_step_runs_once_per_root_and_argv() {
        let root = tempfile::tempdir().expect("tempdir");
        let argv = vec!["dependency".to_owned(), "build".to_owned()];
        let mut runs = 0;
        let first = once_per_root(root.path(), &argv, || {
            runs += 1;
            Err("offline registry".to_owned())
        });
        let second = once_per_root(root.path(), &argv, || {
            runs += 1;
            Ok(())
        });
        assert_eq!(runs, 1);
        assert_eq!(first, Err("offline registry".to_owned()));
        assert_eq!(second, first, "later askers see the first answer");
    }
}
