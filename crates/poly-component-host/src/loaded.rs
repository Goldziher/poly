//! Which components this process has loaded.
//!
//! A loaded component is never unloaded, and the library it came from stays mapped, so removing
//! its archive underneath the process is unsafe on some platforms and misleading on all of them.
//! The record is process-wide because every run builds its own [`ComponentHost`](crate::ComponentHost)
//! and a host dropped after its run still leaves the library mapped.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

static LOADED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Note that `component_id` is loaded in this process.
pub(crate) fn record(component_id: &str) {
    LOADED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(component_id.to_owned());
}

/// The components loaded in this process, sorted.
#[must_use]
pub fn loaded_components() -> Vec<String> {
    LOADED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_component_is_listed_once() {
        record("loaded-test-a");
        record("loaded-test-a");
        let loaded = loaded_components();
        assert_eq!(loaded.iter().filter(|name| *name == "loaded-test-a").count(), 1);
    }
}
