//! Each component type describes exactly the engines the compiled component index records for it.
//!
//! The host refuses a component whose `describe()` differs from the index it was resolved with, so
//! a mismatch here is a component that installs and then fails every file it is asked about.

use poly_component_protocol::decode_describe;
use poly_engines::PolyComponent;

fn assert_matches_index(name: &str, component: &dyn PolyComponent) {
    let described = decode_describe(&component.describe().expect("describe")).expect("decode describe");
    let index = poly_component_index::index();
    let expected = &index
        .component(name)
        .unwrap_or_else(|| panic!("no `{name}` in the index"))
        .engines;
    assert!(!expected.is_empty(), "the index records no engines for `{name}`");
    assert_eq!(
        &described.engines, expected,
        "`{name}` describes other engines than the index records"
    );
}

macro_rules! describe_test {
    ($feature:literal, $test:ident, $name:literal, $component:ty) => {
        #[cfg(feature = $feature)]
        #[test]
        fn $test() {
            assert_matches_index($name, &<$component>::default());
        }
    };
}

describe_test!("sql", sql_matches_the_index, "sql", poly_engines::SqlComponent);
describe_test!(
    "python",
    python_matches_the_index,
    "python",
    poly_engines::PythonComponent
);
describe_test!("php", php_matches_the_index, "php", poly_engines::PhpComponent);
describe_test!("nix", nix_matches_the_index, "nix", poly_engines::NixComponent);
describe_test!("typos", typos_matches_the_index, "typos", poly_engines::TyposComponent);
describe_test!(
    "astgrep",
    astgrep_matches_the_index,
    "astgrep",
    poly_engines::AstgrepComponent
);
describe_test!("misc", misc_matches_the_index, "misc", poly_engines::MiscComponent);
