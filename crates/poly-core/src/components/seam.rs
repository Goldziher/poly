//! Per-run inputs to the pre-plan pass: the dev lock variable and the test seam that injects a
//! host.

use std::sync::Arc;

use poly_component_host::ComponentHost;
use poly_component_index::ComponentIndex;

/// Names a component lock file that stands in for the embedded one, so a development build can
/// install from a `file://` lock.
pub const LOCK_ENV: &str = "POLY_COMPONENT_DEV_LOCK";

/// A host and the index its components are described by, carried in
/// [`RunOptions`](crate::RunOptions) so a run that uses it shares nothing with any other run.
///
/// A test seam: the compiled-in index names only components whose families the default build
/// links, so nothing there is downloadable. The injected host is used as given, so a test
/// builds it with `gate_loads` and its own connector. Not part of the supported API.
#[doc(hidden)]
#[derive(Clone)]
pub struct ComponentInjection {
    pub(super) host: Arc<ComponentHost>,
    pub(super) index: Arc<ComponentIndex>,
}

impl ComponentInjection {
    /// `host` serving the components `index` describes, instead of the embedded index and a
    /// host built from the embedded lock.
    #[must_use]
    pub fn new(host: ComponentHost, index: ComponentIndex) -> Self {
        Self {
            host: Arc::new(host),
            index: Arc::new(index),
        }
    }
}

impl std::fmt::Debug for ComponentInjection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentInjection").finish_non_exhaustive()
    }
}
