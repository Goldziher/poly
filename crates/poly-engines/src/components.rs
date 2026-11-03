//! The ready-made component implementations: one type per engine family, each a [`Producer`]
//! over that family's engines.
//!
//! `alef` names these types as the `implementation` of a component's contract, so each is
//! `Default`-constructible and takes its version from this crate, which inherits the workspace
//! version the component artifacts are published under.

#[cfg(feature = "sql")]
mod sql {
    use crate::{ComponentCallError, PolyComponent, Producer};

    /// The `sql` component: sqruff behind the component protocol.
    pub struct SqlComponent(Producer);

    impl Default for SqlComponent {
        fn default() -> Self {
            Self(Producer::new(
                env!("CARGO_PKG_VERSION"),
                vec![Box::new(crate::sql::SqruffEngine)],
            ))
        }
    }

    impl PolyComponent for SqlComponent {
        fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
            self.0.describe()
        }

        fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
            self.0.call(header, body)
        }
    }
}

#[cfg(feature = "sql")]
pub use sql::SqlComponent;

#[cfg(feature = "mcp")]
mod mcp {
    use std::path::PathBuf;
    use std::sync::Arc;

    use alef_component_abi::{FromHost, HostApi, HostServiceError};
    use poly_frontend_api::ServiceCaller;
    use poly_frontend_api::wire::FrontendArgs;

    use crate::{ComponentCallError, PolyFrontend};

    /// Reaches the host's services through the component ABI's service table.
    struct HostCaller(HostApi);

    impl ServiceCaller for HostCaller {
        fn call(&self, service: &str, request: &[u8]) -> Result<Vec<u8>, String> {
            self.0.call_service(service, request).map_err(|error| match error {
                HostServiceError::Failed { message, .. } if !message.is_empty() => message,
                other => other.to_string(),
            })
        }
    }

    /// The `mcp` component: the MCP server, working through the host's services.
    ///
    /// Built from the host API the component is created with; without a service table (a host
    /// that predates it) every tool call reports that the host provides none.
    pub struct McpComponent {
        host: HostApi,
    }

    impl FromHost for McpComponent {
        fn from_host(host: &HostApi) -> Self {
            Self { host: *host }
        }
    }

    impl PolyFrontend for McpComponent {
        fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
            let description = serde_json::json!({
                "frontend": "mcp",
                "version": env!("CARGO_PKG_VERSION"),
                "services": self.host.has_services(),
            });
            serde_json::to_vec(&description).map_err(|error| ComponentCallError::with_source("cannot encode", error))
        }

        fn run(&self, args: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
            let args: FrontendArgs = if args.is_empty() {
                FrontendArgs::default()
            } else {
                serde_json::from_slice(args)
                    .map_err(|error| ComponentCallError::with_source("the frontend arguments do not decode", error))?
            };
            let caller: Arc<dyn ServiceCaller> = Arc::new(HostCaller(self.host));
            poly_mcp::serve(caller, args.config.map(PathBuf::from))
                .map_err(|error| ComponentCallError::new(format!("{error:#}")))?;
            Ok(Vec::new())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_detached_host_describes_itself_without_services() {
            let component = McpComponent::from_host(&HostApi::detached());
            let described: serde_json::Value = serde_json::from_slice(&component.describe().unwrap()).unwrap();
            assert_eq!(described["frontend"], "mcp");
            assert_eq!(described["services"], false);
        }

        #[test]
        fn a_detached_host_reports_that_it_provides_no_services() {
            let caller = HostCaller(HostApi::detached());
            let error = caller.call("engines.versions", b"null").unwrap_err();
            assert!(error.contains("service table"), "{error}");
        }

        #[test]
        fn undecodable_arguments_are_an_error_not_a_default() {
            let component = McpComponent::from_host(&HostApi::detached());
            assert!(component.run(b"not json").is_err());
        }
    }
}

#[cfg(feature = "mcp")]
pub use mcp::McpComponent;
