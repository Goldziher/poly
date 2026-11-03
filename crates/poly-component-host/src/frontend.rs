//! Running a frontend component: one that hosts no engines and serves the process's stdio.

use anyhow::Context;
use poly_engines::PolyFrontend;
use serde_json::Value;

use crate::components_gen::{POLY_FRONTEND_ENTRY_SYMBOL, PolyFrontendProxy};
use crate::host::ComponentHost;

/// Name of the contract a frontend component provides, as declared in `alef.toml`.
const CONTRACT: &str = "poly_frontend";

impl ComponentHost {
    /// Load the frontend `component_id` and hand it the process: `args` is what it receives in
    /// `PolyFrontend::run` (a JSON `FrontendArgs`). Returns when the frontend ends cleanly.
    ///
    /// The component must have been reported usable by [`ComponentHost::ensure_frontend`] when
    /// the host gates loads, so this never reaches the network. Its host services are the ones
    /// given in [`HostOptions::services`](crate::HostOptions::services).
    ///
    /// # Errors
    ///
    /// When the component was not ensured, does not load (contract or ABI mismatch), describes
    /// itself as another frontend, or its `run` fails.
    pub fn run_frontend(&self, component_id: &str, args: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.shared.may_load(component_id),
            "component `{component_id}` was not made available before it was run; loading never installs"
        );
        let loaded = self
            .manager
            .ensure_contract(component_id, CONTRACT, POLY_FRONTEND_ENTRY_SYMBOL)
            .with_context(|| format!("cannot load the `{component_id}` component"))?;
        crate::loaded::record(component_id);
        let proxy = PolyFrontendProxy::new(&loaded).map_err(anyhow::Error::msg)?;
        let described: Value =
            serde_json::from_slice(&proxy.describe()?).context("the component's description is not valid")?;
        anyhow::ensure!(
            described["frontend"] == component_id,
            "component `{component_id}` describes itself as `{}`",
            described["frontend"]
        );
        proxy.run(args)?;
        Ok(())
    }
}
