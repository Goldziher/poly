//! Global cleanup-hook registry.
//!
//! Callers register `Fn() + Send` closures that are invoked when the process is
//! about to exit (e.g. on `Ctrl-C`). The binary wires this into a `ctrlc` handler.

use std::sync::Mutex;

static CLEANUP_HOOKS: Mutex<Vec<Box<dyn Fn() + Send>>> = Mutex::new(Vec::new());

/// Run all registered cleanup functions in registration order.
pub fn cleanup() {
    let mut hooks = CLEANUP_HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for f in hooks.drain(..) {
        f();
    }
}

/// Register a cleanup function to be run on process exit or interrupt.
pub fn add_cleanup<F: Fn() + Send + 'static>(f: F) {
    CLEANUP_HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Box::new(f));
}
