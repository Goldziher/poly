//! `rules_report` in a core build asks the `astgrep` component; it must answer exactly what the
//! in-process ast-grep engine answers.
//!
//! The first group injects a host whose transport is the real `AstgrepComponent` running
//! in-process, so it needs no installed component. The second group installs the component the
//! `components:dev` lock names and runs only in a core-flavour build with `dev-lock`:
//!
//! ```text
//! ./scripts/components-dev.sh astgrep
//! POLY_COMPONENT_DEV_LOCK=$PWD/target/components/components.lock.json \
//!   cargo test -p poly-host-services --no-default-features --features dev-lock --test rules_component
//! ```
#![cfg(unix)]

use std::path::Path;

use poly_core::Config;
use poly_host_services::ops::{rules_report_for, rules_report_from_wire};

const RULE: &str = "id: marker\nlanguage: rust\nseverity: warning\nmessage: marker\n\
                    rule:\n  kind: line_comment\n  regex: MARKER\n";
const RULE_TEST: &str = "id: marker\ninvalid:\n  - \"// MARKER\\nfn f() {}\"\nvalid:\n  - \"// ok\\nfn f() {}\"\n";
const ORPHAN_TEST: &str = "id: ghost\ninvalid:\n  - \"// MARKER\\nfn f() {}\"\n";

fn rules_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker.yml"), RULE).unwrap();
    std::fs::write(dir.path().join("marker-test.yml"), RULE_TEST).unwrap();
    std::fs::write(dir.path().join("ghost-test.yml"), ORPHAN_TEST).unwrap();
    dir
}

fn dirs(dir: &Path) -> Vec<String> {
    vec![dir.to_string_lossy().into_owned()]
}

/// What the in-process engine reports, converted by the same function the host uses.
fn in_process(config: &Config, dirs: &[String], test: bool) -> poly_frontend_api::dto::RulesReport {
    use poly_core::{Kind, Language};
    let engine_config = config.engine_config(&Language::Other("astgrep".into()), "astgrep", Kind::Lint);
    let wire = poly_engines_astgrep::report::rules_report(dirs, &engine_config, test).unwrap();
    rules_report_from_wire(dirs.to_vec(), wire)
}

fn assert_same(left: &poly_frontend_api::dto::RulesReport, right: &poly_frontend_api::dto::RulesReport) {
    assert_eq!(
        serde_json::to_value(left).unwrap(),
        serde_json::to_value(right).unwrap()
    );
}

#[cfg(feature = "dev-lock")]
mod installed {
    use super::*;

    #[cfg(not(feature = "engine-astgrep"))]
    #[test]
    fn a_core_build_serves_the_listing_and_the_tests_from_the_installed_component() {
        let dir = rules_dir();
        let dirs = dirs(dir.path());
        let config = Config::default();
        for test in [false, true] {
            let via_component = rules_report_for(&config, &dirs, test, None).unwrap();
            assert!(
                via_component
                    .rules
                    .iter()
                    .any(|rule| rule.id == "marker" && rule.source == "user")
            );
            assert_same(&via_component, &in_process(&config, &dirs, test));
        }
    }
}

#[cfg(not(feature = "dev-lock"))]
mod injected {
    use std::path::PathBuf;
    use std::sync::Arc;

    use alef_component_runtime::ComponentManager;
    use poly_component_host::{ComponentHost, ComponentTransport, HostOptions, TransportConnector};
    use poly_config::v2::{ComponentInstallPolicy, ComponentsConfig};
    use poly_core::components::ComponentInjection;
    use poly_engines::{AstgrepComponent, PolyComponent};

    use super::*;

    const TARGET: &str = "test-target";

    /// The real astgrep component, called in-process instead of through a dylib.
    struct InProcess(AstgrepComponent);

    impl ComponentTransport for InProcess {
        fn describe(&self) -> anyhow::Result<Vec<u8>> {
            Ok(self.0.describe()?)
        }

        fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>> {
            Ok(self.0.call(header, body)?)
        }
    }

    struct Connector;

    impl TransportConnector for Connector {
        fn connect(&self, _: &ComponentManager, _: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
            Ok(Arc::new(InProcess(AstgrepComponent::default())))
        }
    }

    fn lock() -> String {
        format!(
            r#"{{"schema_version":2,"public_keys":{{}},"artifacts":[{{
                "identity":{{"crate_name":"c","component":"astgrep","version":"1.0.0","target":"{TARGET}",
                             "feature_hash":"{h}","contract_hash":"{h}"}},
                "provides":[],"mode":"download","url":"https://example.invalid/astgrep.tar.gz",
                "sha256":"{sha}","size":1,"manifest_sha256":"{h}","key_id":"k"}}]}}"#,
            h = "11".repeat(32),
            sha = "ab".repeat(32)
        )
    }

    fn injection(offline: bool) -> (ComponentInjection, tempfile::TempDir) {
        let cache = tempfile::tempdir().unwrap();
        let mut options = HostOptions::new(lock(), PathBuf::from(cache.path()));
        options.target = Some(TARGET.into());
        options.offline = offline;
        options.gate_loads = true;
        let host = ComponentHost::new(options).unwrap().with_connector(Arc::new(Connector));
        (
            ComponentInjection::new(host, poly_component_index::index().clone()),
            cache,
        )
    }

    #[test]
    fn the_component_report_equals_the_in_process_report() {
        let dir = rules_dir();
        let dirs = dirs(dir.path());
        let config = Config::default();
        let (injected, _cache) = injection(false);
        for test in [false, true] {
            let via_component = rules_report_for(&config, &dirs, test, Some(&injected)).unwrap();
            assert!(
                via_component
                    .rules
                    .iter()
                    .any(|rule| rule.id == "marker" && rule.source == "user")
            );
            assert!(via_component.rules.iter().any(|rule| rule.source == "builtin"));
            assert_eq!(via_component.tests.is_some(), test);
            assert_same(&via_component, &in_process(&config, &dirs, test));
        }
    }

    #[test]
    fn test_results_survive_the_round_trip() {
        let dir = rules_dir();
        let (injected, _cache) = injection(false);
        let report = rules_report_for(&Config::default(), &dirs(dir.path()), true, Some(&injected)).unwrap();
        assert_eq!(report.ok, Some(false), "the orphan test file names an unknown rule");
        assert_eq!(report.missing_rule_ids.as_deref(), Some(&["ghost".to_owned()][..]));
        assert_eq!(report.total_rules, Some(1));
        let outcomes = report.tests.expect("tests requested");
        assert_eq!(outcomes.iter().filter(|o| o.rule_id == "marker" && o.passed).count(), 2);
    }

    #[test]
    fn an_offline_host_without_the_component_fails_closed() {
        let (injected, _cache) = injection(true);
        let error = rules_report_for(&Config::default(), &[], false, Some(&injected)).unwrap_err();
        assert_eq!(format!("{error:#}"), "component not installed: astgrep (offline)");
    }

    #[test]
    fn install_never_refuses_to_fetch_the_component() {
        let (injected, _cache) = injection(false);
        let config = Config {
            components: ComponentsConfig {
                install: ComponentInstallPolicy::Never,
                ..ComponentsConfig::default()
            },
            ..Config::default()
        };
        let error = rules_report_for(&config, &[], false, Some(&injected)).unwrap_err();
        assert_eq!(format!("{error:#}"), "component not installed: astgrep (install_never)");
    }
}
