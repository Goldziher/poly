//! What `engine_versions()` is allowed to claim.
//!
//! The map is an identity for the *binary*: which backends are compiled in and
//! what upstream versions they wrap. A consumer uses it to tell two clean
//! reports apart when the poly version is the same but its engines are not, so
//! anything in it that varies by host or by checkout would make it lie.

use poly_core::engine_versions;

#[test]
fn it_names_the_compiled_in_backends() {
    let versions = engine_versions();
    for expected in [
        "ruff",
        "oxc",
        "taplo",
        "rumdl",
        "typos",
        "astgrep",
        "quality",
        "treesitter",
    ] {
        assert!(
            versions.contains_key(expected),
            "{expected} is compiled in and must be listed: {:?}",
            versions.keys().collect::<Vec<_>>()
        );
    }
}

/// Native-toolchain backends probe `PATH` and fingerprint repo-local tool
/// config, so their `version()` differs between machines *and* between two
/// checkouts on one machine. Including them would make a binary identity that
/// changes when nothing about the binary did.
#[test]
fn it_excludes_the_host_toolchain_backends() {
    let versions = engine_versions();
    for excluded in ["rustfmt", "gofmt", "shellcheck", "zigfmt", "swift-format"] {
        assert!(
            !versions.contains_key(excluded),
            "{excluded} wraps a host tool and must not be part of the binary's identity"
        );
    }
}

/// Every version string has to actually say something — an empty one folded
/// into a digest is indistinguishable from an engine that was never there.
#[test]
fn every_version_is_non_empty() {
    for (name, version) in engine_versions() {
        assert!(!version.is_empty(), "{name} reports an empty version");
    }
}

/// Built once and shared, so repeated calls are a borrow rather than a walk of
/// every language's registry.
#[test]
fn it_is_computed_once() {
    assert!(std::ptr::eq(engine_versions(), engine_versions()));
}
