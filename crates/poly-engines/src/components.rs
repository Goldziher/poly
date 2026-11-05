//! The ready-made component implementations: one type per engine family, each a [`Producer`]
//! over that family's engines.
//!
//! `alef` names these types as the `implementation` of a component's contract, so each is
//! `Default`-constructible and takes its version from this crate, which inherits the workspace
//! version the component artifacts are published under.

/// Declares a component type: a [`Producer`](crate::Producer) over the engines `$engines` builds.
macro_rules! family_component {
    ($feature:literal, $module:ident, $name:ident, $doc:literal, $engines:expr $(, rules = $rules:expr)?) => {
        #[cfg(feature = $feature)]
        mod $module {
            use crate::{ComponentCallError, PolyComponent, Producer};

            #[doc = $doc]
            pub struct $name(Producer);

            impl Default for $name {
                fn default() -> Self {
                    let producer = Producer::new(env!("CARGO_PKG_VERSION"), $engines);
                    $(let producer = producer.with_rules($rules);)?
                    Self(producer)
                }
            }

            impl PolyComponent for $name {
                fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
                    self.0.describe()
                }

                fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
                    self.0.call(header, body)
                }
            }
        }

        #[cfg(feature = $feature)]
        pub use $module::$name;
    };
}

family_component!(
    "sql",
    sql,
    SqlComponent,
    "The `sql` component: sqruff behind the component protocol.",
    vec![Box::new(crate::sql::SqruffEngine)]
);
family_component!(
    "python",
    python,
    PythonComponent,
    "The `python` component: ruff, and ty for project type checking.",
    crate::python::engines_for(&poly_engine_api::language::Language::Python)
);
family_component!(
    "php",
    php,
    PhpComponent,
    "The `php` component: mago behind the component protocol.",
    vec![Box::new(crate::php::MagoEngine::default())]
);
family_component!(
    "nix",
    nix,
    NixComponent,
    "The `nix` component: alejandra behind the component protocol.",
    vec![Box::new(crate::nix::NixFmtEngine)]
);
family_component!(
    "typos",
    typos,
    TyposComponent,
    "The `typos` component: the cross-cutting spell-checker.",
    vec![Box::new(crate::typos::TyposEngine)]
);
family_component!(
    "astgrep",
    astgrep,
    AstgrepComponent,
    "The `astgrep` component: ast-grep with the built-in rule pack.",
    vec![Box::new(crate::astgrep::AstGrepEngine)],
    rules = crate::astgrep::report::rules_report
);
family_component!(
    "misc",
    misc,
    MiscComponent,
    "The `misc` component: the Dockerfile, dotenv, GraphQL and HCL engines.",
    vec![
        Box::new(crate::misc::dockerfile::DockerfileEngine),
        Box::new(crate::misc::dotenv::DotenvEngine),
        Box::new(crate::misc::graphql::GraphQlEngine),
        Box::new(crate::misc::hcl::HclEngine),
    ]
);

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
