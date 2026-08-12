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

    #[test]
    fn long_version_carries_version_channel_and_build_id() {
        let long = long_version();
        assert!(long.starts_with(VERSION), "{long} starts with the version");
        assert!(long.contains(channel().as_str()), "{long} names the channel");
        assert!(long.contains(build_id()), "{long} names the build id");
    }
}
