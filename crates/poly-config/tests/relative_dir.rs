//! Native tool configs found from a relative run directory are recorded with absolute paths, so
//! the globs relative to them still match the absolute paths the runner later sees.

use poly_config::PolyConfig;

#[test]
fn native_configs_found_from_dot_are_absolute() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    std::fs::write(repo.path().join(".oxfmtrc.json"), r#"{"ignorePatterns": ["**/*.md"]}"#).unwrap();
    std::fs::write(repo.path().join(".oxlintrc.json"), "{}").unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    let config = PolyConfig::load(std::path::Path::new(".")).unwrap();

    let oxfmt = config.oxfmt_native.file.expect("the oxfmt config is found");
    assert!(oxfmt.is_absolute(), "{}", oxfmt.display());
    assert_eq!(config.oxfmt_native.ignore_patterns, ["**/*.md"]);
    let oxlint = config.oxlint_native.file.expect("the oxlint config is found");
    assert!(oxlint.is_absolute(), "{}", oxlint.display());
}
