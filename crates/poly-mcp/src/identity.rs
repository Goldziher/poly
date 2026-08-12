//! Who is answering, and is that binary still the one on disk?
//!
//! A CLI user can always run `poly --version`. An MCP caller cannot: it sees
//! only tool results, and a `format_check` returning `{"changed": false}` looks
//! identical whether a current build or a known-destructive one produced it.
//!
//! Two complementary mechanisms close that gap:
//!
//! - [`PolyIdentity`] rides along on **every** tool response, so a caller that
//!   looks can always tell which binary answered.
//! - [`ExecutableWatch`] makes the server **fail** for the caller who does not
//!   look. `poly mcp` servers are long-lived by design and outlive an upgrade:
//!   on macOS a deleted inode stays alive as long as a process holds it, so a
//!   server started before `brew upgrade` keeps serving the superseded build
//!   forever, with no expiry. The watch fingerprints the executable at startup
//!   and re-checks it per request.

use std::path::PathBuf;
use std::time::Instant;

use schemars::JsonSchema;
use serde::Serialize;

/// The key every tool response carries its identity under.
pub const IDENTITY_KEY: &str = "poly";

/// The identity of the `poly` binary serving this session.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PolyIdentity {
    /// Workspace version, e.g. `0.19.7`.
    pub version: String,
    /// Build identifier (`git describe` at compile time), e.g.
    /// `v0.19.7-8-g18aa5e8f9c01`, or `unknown` outside a git checkout.
    pub build_id: String,
    /// `release`, `dev`, or `unknown`. Only `release` is a shipped artifact —
    /// a `dev` build sharing a version number may behave differently.
    pub channel: String,
    /// Cargo profile the binary was built with.
    pub profile: String,
    /// Commit the binary was built from, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Path of the executable serving this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// PID of the serving process, so a caller can correlate a stale server
    /// with the process it needs to kill.
    pub pid: u32,
}

impl PolyIdentity {
    /// Read the identity of the running process.
    fn detect() -> Self {
        PolyIdentity {
            version: poly_buildinfo::VERSION.to_string(),
            build_id: poly_buildinfo::build_id().to_string(),
            channel: poly_buildinfo::channel().as_str().to_string(),
            profile: poly_buildinfo::profile().to_string(),
            commit: poly_buildinfo::commit().map(str::to_string),
            executable: std::env::current_exe().ok().map(|path| path.display().to_string()),
            pid: std::process::id(),
        }
    }
}

/// The identity of this process, computed once.
pub fn identity() -> &'static PolyIdentity {
    static IDENTITY: std::sync::LazyLock<PolyIdentity> = std::sync::LazyLock::new(PolyIdentity::detect);
    &IDENTITY
}

/// The identity as a JSON value, computed once.
pub fn identity_value() -> &'static serde_json::Value {
    static VALUE: std::sync::LazyLock<serde_json::Value> =
        std::sync::LazyLock::new(|| serde_json::to_value(identity()).unwrap_or(serde_json::Value::Null));
    &VALUE
}

/// A file's identity on disk — enough to notice a replacement, and nothing that
/// changes when the file is merely read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    /// Inode on unix; `0` elsewhere.
    inode: u64,
    /// Device on unix; `0` elsewhere.
    device: u64,
    /// Size in bytes — the cross-platform fallback signal.
    size: u64,
}

impl Fingerprint {
    /// Fingerprint the file at `path`, or `None` when it cannot be stat'd.
    fn of(path: &std::path::Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Fingerprint {
                inode: metadata.ino(),
                device: metadata.dev(),
                size: metadata.len(),
            })
        }
        #[cfg(not(unix))]
        {
            Some(Fingerprint {
                inode: 0,
                device: 0,
                size: metadata.len(),
            })
        }
    }
}

/// Why a served request should be refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    /// The executable's path no longer resolves to a file.
    Deleted,
    /// A different file now occupies the executable's path.
    Replaced,
}

impl Staleness {
    /// One-line description of what happened to the binary.
    pub fn describe(self) -> &'static str {
        match self {
            Staleness::Deleted => "the executable serving this session has been deleted from disk",
            Staleness::Replaced => "the executable serving this session has been replaced on disk",
        }
    }
}

/// Watches the executable this server is running from.
#[derive(Debug, Clone)]
pub struct ExecutableWatch {
    /// The executable path, when it could be determined.
    path: Option<PathBuf>,
    /// Its fingerprint at startup. `None` disables checking — an executable we
    /// could never fingerprint must not produce a false staleness report.
    baseline: Option<Fingerprint>,
    /// When this server started, so a caller can see how long a stale server
    /// has been answering.
    started: Instant,
}

impl ExecutableWatch {
    /// Fingerprint the running executable at server startup.
    pub fn capture() -> Self {
        Self::over(std::env::current_exe().ok())
    }

    /// Fingerprint a specific executable path.
    ///
    /// [`capture`](Self::capture) is the normal entry point; this exists so a
    /// caller that already knows which file backs it — a supervisor, or a test
    /// exercising the replacement path — can watch that file directly.
    pub fn over(path: Option<PathBuf>) -> Self {
        let baseline = path.as_deref().and_then(Fingerprint::of);
        ExecutableWatch {
            path,
            baseline,
            started: Instant::now(),
        }
    }

    /// Re-stat the executable. `Some(_)` means this server is serving a binary
    /// that is no longer the one on disk.
    pub fn check(&self) -> Option<Staleness> {
        let (path, baseline) = (self.path.as_deref()?, self.baseline?);
        match Fingerprint::of(path) {
            None => Some(Staleness::Deleted),
            Some(current) if current != baseline => Some(Staleness::Replaced),
            Some(_) => None,
        }
    }

    /// Seconds since this server started.
    pub fn uptime_seconds(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// The full refusal message handed to a caller when the binary moved.
    ///
    /// Names the process explicitly: the remedy is to restart the server, and a
    /// caller that cannot do so needs the PID to kill it.
    pub fn stale_message(&self, staleness: Staleness) -> String {
        let identity = identity();
        format!(
            "poly mcp refused to answer: {}. This server (pid {}) is still running poly {} (build {}), \
             started {}s ago, and its answers may not reflect the poly now installed. \
             Restart the MCP server — an upgrade does not replace an already-running one.",
            staleness.describe(),
            identity.pid,
            identity.version,
            identity.build_id,
            self.uptime_seconds(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_reports_this_process() {
        let identity = identity();
        assert_eq!(identity.version, poly_buildinfo::VERSION);
        assert_eq!(identity.pid, std::process::id());
        assert!(!identity.build_id.is_empty(), "build id falls back to `unknown`");
    }

    #[test]
    fn a_watch_over_an_untouched_executable_is_current() {
        let watch = ExecutableWatch::capture();
        assert_eq!(watch.check(), None, "the test binary is still on disk");
    }

    #[test]
    fn a_deleted_executable_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poly");
        std::fs::write(&path, b"binary").unwrap();
        let watch = ExecutableWatch::over(Some(path.clone()));
        assert_eq!(watch.check(), None, "unchanged file is current");

        std::fs::remove_file(&path).unwrap();
        assert_eq!(watch.check(), Some(Staleness::Deleted), "a deleted file is stale");
    }

    #[test]
    fn an_executable_replaced_at_the_same_path_is_detected() {
        // The upgrade case: same path, new file. Byte-identical replacement is
        // still caught on unix, because the inode changes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poly");
        std::fs::write(&path, b"old build").unwrap();
        let watch = ExecutableWatch::over(Some(path.clone()));

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"new build, different size").unwrap();
        assert_eq!(
            watch.check(),
            Some(Staleness::Replaced),
            "a new file at the same path is stale"
        );
    }

    #[test]
    fn a_watch_without_a_baseline_never_reports_staleness() {
        let watch = ExecutableWatch::over(Some(PathBuf::from("/nonexistent/poly")));
        assert_eq!(
            watch.check(),
            None,
            "an executable we could never fingerprint must not produce false failures"
        );
    }

    #[test]
    fn the_stale_message_names_the_process_and_the_remedy() {
        let watch = ExecutableWatch::capture();
        let message = watch.stale_message(Staleness::Deleted);
        assert!(message.contains("deleted from disk"), "{message}");
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "the caller needs the pid: {message}"
        );
        assert!(message.contains("Restart the MCP server"), "{message}");
    }
}
