//! The api crate must stay light: every engine family depends on it, so an engine crate in its
//! tree would drag that engine into all of them.

const ENGINE_PREFIXES: &[&str] = &[
    "oxc",
    "ruff",
    "biome",
    "mago",
    "sqruff",
    "typos",
    "ast-grep",
    "ast_grep",
    "uncomment",
    "rumdl",
    "adrlint",
    "malva",
    "markup_fmt",
    "taplo",
    "tree-sitter",
    "tree_sitter",
    "rubyfmt",
    "hcl",
    "sort-package-json",
    "poly-core",
    "serde_yaml",
    "dockerfile-parser",
    "graphql",
];

#[test]
fn manifest_declares_no_engine_dependencies() {
    let manifest: toml::Table = toml::from_str(include_str!("../Cargo.toml")).expect("valid manifest");
    let mut names = Vec::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(section).and_then(toml::Value::as_table) {
            names.extend(table.keys().cloned());
        }
    }
    assert!(
        names.iter().any(|n| n == "poly-config"),
        "dependency scan found nothing: {names:?}"
    );
    for name in &names {
        assert!(
            !ENGINE_PREFIXES.iter().any(|prefix| name.starts_with(prefix)),
            "`{name}` is an engine crate and must not be a dependency of poly-engine-api"
        );
    }
}
