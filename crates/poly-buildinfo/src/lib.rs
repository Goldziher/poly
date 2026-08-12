//! The running `poly` binary's build identity.
//!
//! `poly --version` alone cannot distinguish "the 0.19.7 we tagged and shipped"
//! from "0.19.7 plus eight unreleased fixes", and a consumer who quotes the bare
//! version in a bug report sends everyone chasing the wrong code. This crate
//! carries the extra bit: a **build id** (`git describe` at compile time) and a
//! **channel** that says, honestly, whether this binary is a release, a
//! development build, or something whose provenance could not be established.
//!
//! Everything here is captured by `build.rs` — see its module docs for why the
//! identity is network-free and free of any dirty-tree or timestamp probe.
//!
//! ```
//! // The version always resolves; the channel is honest about what it knows.
//! assert!(!poly_buildinfo::VERSION.is_empty());
//! ```

use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

/// The workspace version this binary was built from (e.g. `0.19.7`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Rendered wherever an identity component could not be established.
pub const UNKNOWN: &str = "unknown";

/// `git describe --tags --always` at build time, or the `POLY_BUILD_ID`
/// override. Empty when the build happened outside a git checkout.
const RAW_BUILD_ID: &str = env!("POLY_BUILD_ID");

/// Short commit the binary was built from; empty outside a git checkout.
const RAW_COMMIT: &str = env!("POLY_BUILD_COMMIT");

/// The cargo profile (`debug` / `release`) the binary was built with.
const RAW_PROFILE: &str = env!("POLY_BUILD_PROFILE");

/// How much provenance the binary can honestly claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildChannel {
    /// A release-profile build from the exact `v<VERSION>` tag — what the
    /// installer, Homebrew, and the GitHub release ship.
    Release,
    /// Anything built from a git checkout that is *not* the matching release
    /// tag: a debug build, a branch, or commits past the tag. Its behaviour may
    /// differ from the released binary of the same version number.
    Development,
    /// The build had no git checkout and no `POLY_BUILD_ID`, so provenance could
    /// not be established. Reported as-is rather than guessed.
    Unknown,
}

impl BuildChannel {
    /// Stable lowercase token for display, logs, and JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            BuildChannel::Release => "release",
            BuildChannel::Development => "dev",
            BuildChannel::Unknown => UNKNOWN,
        }
    }

    /// Whether this channel identifies a genuine released artifact.
    pub const fn is_release(self) -> bool {
        matches!(self, BuildChannel::Release)
    }
}

/// The build id: `v0.19.7` for a release, `v0.19.7-8-g18aa5e8f9c01` for a build
/// past that tag, or [`UNKNOWN`].
pub fn build_id() -> &'static str {
    if RAW_BUILD_ID.is_empty() { UNKNOWN } else { RAW_BUILD_ID }
}

/// The commit the binary was built from, when it was built inside a checkout.
pub fn commit() -> Option<&'static str> {
    if RAW_COMMIT.is_empty() { None } else { Some(RAW_COMMIT) }
}

/// The cargo profile the binary was built with (`debug` / `release`).
pub fn profile() -> &'static str {
    if RAW_PROFILE.is_empty() { UNKNOWN } else { RAW_PROFILE }
}

/// The channel this binary can honestly claim.
///
/// A release requires **both** the release profile and a build id equal to the
/// `v<VERSION>` tag; a debug build sitting on the tag is still a development
/// build, because it is not the artifact that was shipped.
pub fn channel() -> BuildChannel {
    if RAW_BUILD_ID.is_empty() {
        return BuildChannel::Unknown;
    }
    if RAW_PROFILE == "release" && RAW_BUILD_ID.strip_prefix('v') == Some(VERSION) {
        BuildChannel::Release
    } else {
        BuildChannel::Development
    }
}

/// The full identity line: `0.19.7 (dev build v0.19.7-8-g18aa5e8f9c01, debug)`.
///
/// Used as the `poly --version` string so a consumer who quotes it cannot
/// accidentally present a development build as a release.
pub fn long_version() -> &'static str {
    static LONG_VERSION: LazyLock<String> =
        LazyLock::new(|| format!("{VERSION} ({} build {}, {})", channel().as_str(), build_id(), profile()));
    LONG_VERSION.as_str()
}

/// The identity of this build as far as poly's **result cache** is concerned:
/// two binaries that share this string are allowed to share cache entries.
///
/// The cache stores what an engine produced for a given input. Serving one
/// binary's results to a *different* binary is only safe when the two provably
/// behave the same, and the version number alone does not prove that — every
/// unreleased build of `0.19.7` also calls itself `0.19.7`. That is how a stale
/// "already formatted" verdict gets served for a file the current binary would
/// actually rewrite, which is the worst outcome a format/lint gate can have.
///
/// The identity is therefore scoped per [`BuildChannel`]:
///
/// - **Release** — `release/<version>`. A tagged release-profile build is
///   reproducible from its version alone, so every machine's `v0.19.7` shares
///   one identity and CI cache reuse keeps working. Nothing machine-local
///   (path, mtime) is folded in.
/// - **Development / unknown** — channel, version, build id, profile **and a
///   fingerprint of the executable file**. The build id (`git describe`)
///   separates commits, but it deliberately ignores uncommitted work, so two
///   builds of the same commit with different edits share it; the executable
///   fingerprint is what separates those. A development build is expected to
///   re-run work after a rebuild — that is the correct trade against serving a
///   verdict computed by code that no longer exists.
///
/// Computed once per process and cached, so the per-file hot path only folds a
/// borrowed `&'static str` into the cache key.
pub fn cache_identity() -> &'static str {
    static CACHE_IDENTITY: LazyLock<String> =
        LazyLock::new(|| compose_cache_identity(channel(), VERSION, build_id(), profile(), &executable_fingerprint()));
    CACHE_IDENTITY.as_str()
}

/// Assemble the [`cache_identity`] string from its parts.
///
/// Split out from [`cache_identity`] so the per-channel scoping rule can be
/// tested directly, without a process whose build identity we cannot choose.
fn compose_cache_identity(
    channel: BuildChannel,
    version: &str,
    build_id: &str,
    profile: &str,
    executable_fingerprint: &str,
) -> String {
    if channel.is_release() {
        return format!("{}/{version}", channel.as_str());
    }
    format!(
        "{}/{version}/{build_id}/{profile}/{executable_fingerprint}",
        channel.as_str()
    )
}

/// A cheap fingerprint of the running executable: `<device>.<inode>-<size>-<mtime-nanos>`
/// (the device/inode pair is `unknown` off unix, where size and mtime carry it).
///
/// This is the same file identity `poly mcp`'s `ExecutableWatch` uses to notice
/// its own binary being replaced, plus the modification time — a linker writes a
/// new file and renames it over the old one, so every one of those four fields
/// moves on a rebuild.
///
/// The alternative, hashing the executable's bytes, is the only way two
/// byte-identical development builds could keep sharing a cache — and it was
/// measured and rejected: blake3 over the 83 MiB release binary costs ~45 ms
/// (~55 ms including the read), and ~500 ms over the 353 MiB debug binary, on
/// **every** invocation — including the three-file pre-commit gate poly exists
/// to make instant. Over-invalidating a development build is the cheaper error.
///
/// Cost here is one `stat`, once per process. Falls back to [`UNKNOWN`] when
/// the executable cannot be located or stat-ed — the identity is then coarser
/// (build id + profile only) but never claims a provenance it does not have.
fn executable_fingerprint() -> String {
    let Ok(path) = std::env::current_exe() else {
        return UNKNOWN.to_string();
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return UNKNOWN.to_string();
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or_else(|| UNKNOWN.to_string(), |since_epoch| since_epoch.as_nanos().to_string());
    format!("{}-{}-{modified}", file_identity(&metadata), metadata.len())
}

/// The `<device>.<inode>` pair identifying a file on unix.
#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}.{}", metadata.dev(), metadata.ino())
}

/// Off unix there is no stable stat-cheap file id; size and mtime carry the
/// fingerprint alone.
#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> String {
    UNKNOWN.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_the_workspace_package_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn build_id_is_never_empty() {
        assert!(!build_id().is_empty(), "build id falls back to `unknown`");
    }

    #[test]
    fn channel_token_round_trips() {
        assert_eq!(BuildChannel::Release.as_str(), "release");
        assert_eq!(BuildChannel::Development.as_str(), "dev");
        assert_eq!(BuildChannel::Unknown.as_str(), "unknown");
    }

    #[test]
    fn only_release_channel_claims_a_release() {
        assert!(BuildChannel::Release.is_release());
        assert!(!BuildChannel::Development.is_release());
        assert!(!BuildChannel::Unknown.is_release());
    }

    #[test]
    fn a_debug_build_is_never_reported_as_a_release() {
        // The test suite itself is compiled in the debug profile, so whatever
        // git says, this binary must not claim to be a release artifact.
        if profile() == "debug" {
            assert_ne!(channel(), BuildChannel::Release);
        }
    }

    /// A release identity must depend on nothing but the channel and version, so
    /// two machines building the same tag reuse each other's cache entries.
    #[test]
    fn release_cache_identity_ignores_everything_machine_local() {
        let first = compose_cache_identity(BuildChannel::Release, "0.19.7", "v0.19.7", "release", "111-222");
        let second = compose_cache_identity(BuildChannel::Release, "0.19.7", "v0.19.7", "release", "333-444");
        assert_eq!(first, "release/0.19.7");
        assert_eq!(first, second, "a release identity must not depend on the local binary");
    }

    #[test]
    fn a_release_version_bump_changes_the_cache_identity() {
        assert_ne!(
            compose_cache_identity(BuildChannel::Release, "0.19.7", "v0.19.7", "release", ""),
            compose_cache_identity(BuildChannel::Release, "0.19.8", "v0.19.8", "release", ""),
        );
    }

    #[test]
    fn a_development_build_never_shares_a_release_identity() {
        assert_ne!(
            compose_cache_identity(BuildChannel::Development, "0.19.7", "v0.19.7", "release", "111-222"),
            compose_cache_identity(BuildChannel::Release, "0.19.7", "v0.19.7", "release", "111-222"),
            "an unreleased 0.19.7 must not read the released 0.19.7's cache entries"
        );
    }

    #[test]
    fn development_builds_from_different_commits_get_different_identities() {
        assert_ne!(
            compose_cache_identity(
                BuildChannel::Development,
                "0.19.7",
                "v0.19.7-1-gaaaaaaa",
                "release",
                "111-222"
            ),
            compose_cache_identity(
                BuildChannel::Development,
                "0.19.7",
                "v0.19.7-2-gbbbbbbb",
                "release",
                "111-222"
            ),
        );
    }

    /// `git describe` cannot see uncommitted work, so two builds of one commit
    /// share a build id; only the executable fingerprint separates them.
    #[test]
    fn development_builds_of_one_commit_get_different_identities_per_binary() {
        assert_ne!(
            compose_cache_identity(
                BuildChannel::Development,
                "0.19.7",
                "v0.19.7-1-gaaaaaaa",
                "release",
                "111-222"
            ),
            compose_cache_identity(
                BuildChannel::Development,
                "0.19.7",
                "v0.19.7-1-gaaaaaaa",
                "release",
                "111-333"
            ),
        );
    }

    #[test]
    fn debug_and_release_profiles_of_one_commit_get_different_identities() {
        assert_ne!(
            compose_cache_identity(
                BuildChannel::Development,
                "0.19.7",
                "v0.19.7-1-gaaaaaaa",
                "debug",
                "111-222"
            ),
            compose_cache_identity(
                BuildChannel::Development,
                "0.19.7",
                "v0.19.7-1-gaaaaaaa",
                "release",
                "111-222"
            ),
        );
    }

    #[test]
    fn an_unknown_provenance_build_gets_its_own_identity() {
        assert_ne!(
            compose_cache_identity(BuildChannel::Unknown, "0.19.7", UNKNOWN, "release", "111-222"),
            compose_cache_identity(BuildChannel::Development, "0.19.7", UNKNOWN, "release", "111-222"),
        );
    }

    #[test]
    fn cache_identity_is_stable_within_a_process_and_names_the_channel() {
        let identity = cache_identity();
        assert_eq!(identity, cache_identity(), "the identity is computed once and reused");
        assert!(
            identity.starts_with(channel().as_str()),
            "{identity} is scoped by channel"
        );
        assert!(identity.contains(VERSION), "{identity} carries the version");
    }

    #[test]
    fn executable_fingerprint_is_stable_for_one_binary() {
        assert_eq!(executable_fingerprint(), executable_fingerprint());
        assert_ne!(
            executable_fingerprint(),
            UNKNOWN,
            "the running test binary can be stat-ed, so the fingerprint must resolve"
        );
    }

    #[test]
    fn long_version_carries_version_channel_and_build_id() {
        let long = long_version();
        assert!(long.starts_with(VERSION), "{long} starts with the version");
        assert!(long.contains(channel().as_str()), "{long} names the channel");
        assert!(long.contains(build_id()), "{long} names the build id");
    }
}
