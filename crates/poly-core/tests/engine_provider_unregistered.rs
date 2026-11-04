//! Without a registered provider the registry plans no external engines, and a provider offered
//! after the engine-version table was built is refused rather than silently omitted from it.

use poly_core::{
    Engine, Language, ProviderRegistrationError, engine_languages, engine_versions, register_engine_provider,
};

fn provider(_: &Language) -> Vec<Box<dyn Engine>> {
    Vec::new()
}

#[test]
fn no_provider_means_no_external_engines_and_late_registration_is_refused() {
    assert!(engine_languages("probe-format", false).is_empty());
    let _ = engine_versions();
    assert_eq!(
        register_engine_provider(provider),
        Err(ProviderRegistrationError::TooLate)
    );
}
