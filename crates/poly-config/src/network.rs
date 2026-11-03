//! The process-wide network switch every fetch path consults.
//!
//! `--offline` and `POLY_OFFLINE` are parsed in `poly-cli`, but the paths that
//! touch the network live in lower crates (`poly-core`'s grammar and tool
//! installs, `poly-cli`'s remote sources, the component host). They all read
//! [`offline`] from here, so there is exactly one answer per process.
//!
//! The component host consumes [`component_url`] and
//! [`component_cache_dir`]; they live beside the switch so every network knob
//! is documented in one place.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::bail;

/// Environment variable that forces offline mode.
pub const OFFLINE_ENV: &str = "POLY_OFFLINE";
/// Environment variable naming a mirror base URL for component downloads.
pub const COMPONENT_MIRROR_ENV: &str = "POLY_COMPONENT_MIRROR";
/// Environment variable overriding the component cache root.
pub const COMPONENT_CACHE_ENV: &str = "POLY_COMPONENT_CACHE";

static OFFLINE: AtomicBool = AtomicBool::new(false);

/// Record that this process must not use the network (`--offline`). Sticky for
/// the process: it only ever turns offline mode on, and `POLY_OFFLINE` still
/// applies when this was never called.
pub fn set_offline(value: bool) {
    if value {
        OFFLINE.store(true, Ordering::Relaxed);
    }
}

/// Whether the process must not touch the network: [`set_offline`] was called
/// with `true`, or `POLY_OFFLINE` is set to anything but a falsey word.
#[must_use]
pub fn offline() -> bool {
    OFFLINE.load(Ordering::Relaxed) || env_flag(OFFLINE_ENV)
}

/// Whether `value` (an environment variable's contents) means "on".
#[must_use]
pub fn is_truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| is_truthy(&value))
}

/// The message used when offline mode blocks `what`.
#[must_use]
pub fn offline_message(what: &str) -> String {
    format!("offline mode: refusing to {what} (unset --offline / {OFFLINE_ENV}, or provide it locally)")
}

/// Fail with a clear message when offline; call immediately before any network
/// access, naming the action (for example `"download tool helm 4.0.0"`).
///
/// # Errors
///
/// Returns an error when [`offline`] is true.
pub fn require_online(what: &str) -> anyhow::Result<()> {
    if offline() {
        bail!("{}", offline_message(what));
    }
    Ok(())
}

/// The mirror base URL from `POLY_COMPONENT_MIRROR`, without a trailing slash,
/// when set and non-empty.
#[must_use]
pub fn component_mirror() -> Option<String> {
    mirror_from(std::env::var(COMPONENT_MIRROR_ENV).ok().as_deref())
}

fn mirror_from(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim().trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Rewrite a component download `url` onto the configured mirror: the origin
/// (`scheme://host[:port]`) is replaced by the mirror base and the original
/// path is kept. Returns `url` unchanged when no mirror is set or `url` has no
/// `scheme://` prefix. Integrity never depends on the URL (the lock pins the
/// sha256), so the rewrite is safe.
#[must_use]
pub fn component_url(url: &str) -> String {
    rewrite_url(url, component_mirror().as_deref())
}

fn rewrite_url(url: &str, mirror: Option<&str>) -> String {
    let Some(mirror) = mirror else {
        return url.to_owned();
    };
    let Some((_, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    match rest.find('/') {
        Some(slash) => format!("{mirror}{}", &rest[slash..]),
        None => mirror.to_owned(),
    }
}

/// The component cache root from `POLY_COMPONENT_CACHE`, when set and non-empty.
#[must_use]
pub fn component_cache_dir() -> Option<PathBuf> {
    cache_from(std::env::var(COMPONENT_CACHE_ENV).ok().as_deref())
}

fn cache_from(value: Option<&str>) -> Option<PathBuf> {
    value.filter(|v| !v.trim().is_empty()).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_matches_the_documented_words() {
        for off in ["", "0", "false", "No", " off "] {
            assert!(!is_truthy(off), "{off:?}");
        }
        for on in ["1", "true", "yes", "anything"] {
            assert!(is_truthy(on), "{on:?}");
        }
    }

    #[test]
    fn mirror_rewrites_the_origin_and_keeps_the_path() {
        let mirror = mirror_from(Some(" https://mirror.corp/poly/ "));
        assert_eq!(mirror.as_deref(), Some("https://mirror.corp/poly"));
        assert_eq!(
            rewrite_url("https://github.com/org/repo/releases/x.tar.gz", mirror.as_deref()),
            "https://mirror.corp/poly/org/repo/releases/x.tar.gz"
        );
        assert_eq!(
            rewrite_url("https://host", mirror.as_deref()),
            "https://mirror.corp/poly"
        );
        assert_eq!(rewrite_url("file.tar.gz", mirror.as_deref()), "file.tar.gz");
        assert_eq!(rewrite_url("https://a/b", None), "https://a/b");
        assert_eq!(mirror_from(Some("  ")), None);
    }

    #[test]
    fn cache_override_ignores_blank_values() {
        assert_eq!(cache_from(Some("/x/y")), Some(PathBuf::from("/x/y")));
        assert_eq!(cache_from(Some(" ")), None);
        assert_eq!(cache_from(None), None);
    }

    #[test]
    fn offline_message_names_the_action() {
        assert!(offline_message("clone a").contains("offline mode: refusing to clone a"));
    }
}
