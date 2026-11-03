//! The component lock compiled into a release build.

/// The lock staged by `build.rs` from `POLY_COMPONENT_LOCK_FILE`; empty when the variable was
/// not set at build time.
const EMBEDDED_LOCK: &str = include_str!(concat!(env!("OUT_DIR"), "/components.lock.json"));

/// The embedded lock, or `None` for a build that embeds none.
pub(super) fn embedded_lock() -> Option<&'static str> {
    (!EMBEDDED_LOCK.trim().is_empty()).then_some(EMBEDDED_LOCK)
}
