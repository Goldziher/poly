//! Registers the web family with `poly-core` the way the bundled `poly` binary does, so a test that
//! drives the pipeline (`poly_core::lint_run`, `poly_core::format`) reaches these engines.

use std::sync::Once;

/// Registers [`poly_engines_web::engines_for`] as the engine provider and marks `engine-web` as
/// linked so the component planner never tries to download the web component. Idempotent.
pub fn register() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        poly_core::register_engine_provider(poly_engines_web::engines_for).expect("register the web engines");
        poly_core::components::register_linked_features(&["engine-web"]);
    });
}
