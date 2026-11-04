//! A provider's formatter keeps the order the engines had when they were built in: TOML lints with
//! the provider's `taplo` before core's lint-only `polyconfig`, and the tree-sitter reindenter is
//! gone. The provider slot is process-global, so this lives in its own binary.

use poly_core::{Capabilities, Engine, Language, register_engine_provider, registered_engines};

struct Taplo;

impl Engine for Taplo {
    fn name(&self) -> &'static str {
        "taplo"
    }

    fn languages(&self) -> &'static [Language] {
        &[Language::Toml]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: false,
            project: false,
        }
    }

    fn version(&self) -> &str {
        "0"
    }
}

fn provider(lang: &Language) -> Vec<Box<dyn Engine>> {
    match lang {
        Language::Toml => vec![Box::new(Taplo)],
        _ => Vec::new(),
    }
}

#[test]
fn a_provider_formatter_precedes_the_lint_only_core_engine_for_toml() {
    register_engine_provider(provider).expect("first registration succeeds");

    let names: Vec<&str> = registered_engines()
        .iter()
        .filter(|engine| engine.languages().contains(&Language::Toml))
        .map(|engine| engine.name())
        .collect();

    assert_eq!(names, ["taplo", "polyconfig"]);
}
