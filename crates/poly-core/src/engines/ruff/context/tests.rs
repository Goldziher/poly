use std::fs;
use std::path::{Path, PathBuf};

use super::{PackageRoots, module_tree, package_root, walk};
use crate::config::{Config, Kind};
use crate::language::Language;

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "").unwrap();
}

fn digest_of(root: &Path) -> String {
    let mut entries = walk(root);
    entries.sort_unstable();
    entries.join("\n")
}

#[test]
fn the_package_root_follows_init_files_up_to_the_first_gap() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("proj/pkg");
    touch(&pkg.join("__init__.py"));
    touch(&pkg.join("sub/__init__.py"));
    touch(&pkg.join("sub/mod.py"));

    assert_eq!(package_root(&pkg.join("sub/mod.py")), pkg.to_string_lossy());
}

#[test]
fn a_directory_without_an_init_file_has_no_package_root() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("loose/mod.py"));

    assert_eq!(package_root(&dir.path().join("loose/mod.py")), "-");
}

#[test]
fn adding_or_removing_a_sibling_init_file_changes_the_package_root() {
    let dir = tempfile::tempdir().unwrap();
    let module = dir.path().join("pkg/mod.py");
    touch(&module);
    let before = package_root(&module);

    touch(&dir.path().join("pkg/__init__.py"));
    let during = package_root(&module);
    fs::remove_file(dir.path().join("pkg/__init__.py")).unwrap();

    assert_ne!(before, during);
    assert_eq!(before, package_root(&module));
}

#[test]
fn an_init_file_in_a_parent_directory_moves_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let module = dir.path().join("outer/inner/mod.py");
    touch(&module);
    touch(&dir.path().join("outer/inner/__init__.py"));
    let inner_only = package_root(&module);

    touch(&dir.path().join("outer/__init__.py"));

    assert_ne!(inner_only, package_root(&module));
}

#[test]
fn remembered_package_roots_hold_until_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("pkg");
    touch(&pkg.join("mod.py"));
    let roots = PackageRoots::default();
    assert_eq!(roots.get(&pkg), None);

    touch(&pkg.join("__init__.py"));
    assert_eq!(roots.get(&pkg), None, "the answer is remembered, not recomputed");

    roots.clear();
    assert_eq!(roots.get(&pkg), Some(pkg.clone()));
}

#[test]
fn the_module_tree_changes_when_a_module_is_added_removed_or_renamed() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("a/x.py"));
    let src = [PathBuf::from(dir.path())];
    let before = module_tree(&src);

    touch(&dir.path().join("a/y.py"));
    let added = module_tree(&src);
    fs::rename(dir.path().join("a/y.py"), dir.path().join("a/z.py")).unwrap();
    let renamed = module_tree(&src);
    fs::remove_file(dir.path().join("a/z.py")).unwrap();

    assert_ne!(before, added);
    assert_ne!(added, renamed);
    assert_eq!(before, module_tree(&src));
}

#[test]
fn editing_a_module_does_not_change_the_module_tree() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("a/x.py"));
    let src = [PathBuf::from(dir.path())];
    let before = module_tree(&src);

    fs::write(dir.path().join("a/x.py"), "import os\n").unwrap();

    assert_eq!(before, module_tree(&src));
}

#[test]
fn the_module_tree_ignores_vendored_and_non_identifier_directories() {
    let dir = tempfile::tempdir().unwrap();
    touch(&dir.path().join("a/x.py"));
    let src = [PathBuf::from(dir.path())];
    let before = module_tree(&src);

    touch(&dir.path().join("node_modules/pkg/index.py"));
    touch(&dir.path().join("bazel-out/gen.py"));
    touch(&dir.path().join(".git/hooks/pre.py"));
    touch(&dir.path().join("a/notes.txt"));

    assert_eq!(before, module_tree(&src));
    assert!(!digest_of(dir.path()).contains("bazel-out"));
}

#[test]
fn namespace_packages_are_not_configurable() {
    let mut cfg = Config::default().engine_config(&Language::Python, "ruff", Kind::Lint);
    cfg.options.insert("select".into(), toml::Value::Array(vec![]));
    assert!(super::super::build_settings(&cfg).namespace_packages.is_empty());
}
