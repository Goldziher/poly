//! An external engine provider's engines join the registry for the languages it serves.
//!
//! The provider slot is process-global and set once, so the unregistered case lives in its own
//! binary (`engine_provider_unregistered.rs`). One provider serves two tree-sitter-tier
//! languages differently: a formatter for Elixir, a lint-only engine for C.

use poly_core::{
    Capabilities, Engine, Language, ProviderRegistrationError, engine_languages, register_engine_provider,
    registered_engines,
};

struct ProbeEngine {
    name: &'static str,
    languages: &'static [Language],
    format: bool,
}

impl Engine for ProbeEngine {
    fn name(&self) -> &'static str {
        self.name
    }

    fn languages(&self) -> &'static [Language] {
        self.languages
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: self.format,
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
        Language::Elixir => vec![Box::new(ProbeEngine {
            name: "probe-format",
            languages: &[Language::Elixir],
            format: true,
        })],
        Language::C => vec![Box::new(ProbeEngine {
            name: "probe-lint",
            languages: &[Language::C],
            format: false,
        })],
        _ => Vec::new(),
    }
}

#[test]
fn provider_engines_join_their_language_and_a_formatter_displaces_tree_sitter() {
    register_engine_provider(provider).expect("first registration succeeds");

    assert_eq!(engine_languages("probe-format", true), vec!["elixir".to_string()]);
    assert_eq!(engine_languages("probe-lint", false), vec!["c".to_string()]);
    assert!(engine_languages("probe-lint", true).is_empty());

    let formatters = engine_languages("treesitter", true);
    assert!(
        !formatters.contains(&"elixir".to_string()),
        "formatter provider drops treesitter"
    );
    assert!(
        formatters.contains(&"c".to_string()),
        "lint-only provider keeps treesitter"
    );

    assert!(
        registered_engines()
            .iter()
            .any(|engine| engine.name() == "probe-format")
    );
    assert_eq!(
        register_engine_provider(provider),
        Err(ProviderRegistrationError::AlreadyRegistered)
    );
}
