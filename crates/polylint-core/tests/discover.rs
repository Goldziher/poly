//! File-discovery tests: vendored/build/cache directories must be pruned even
//! when they are tracked in git (so `.gitignore` does not exclude them).

use std::fs;
use std::path::Path;

use polylint_core::discover::discover;
use polylint_core::{Config, ConfigSet};

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

#[test]
fn skips_vendored_and_build_directories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let source = root.join("src/main.py");
    let vendored = root.join("node_modules/pkg/index.js");
    let dependency = root.join("deps/foo/CHANGELOG.md");

    write_file(&source, "x = 1\n");
    write_file(&vendored, "const a = 1;\n");
    write_file(&dependency, "# changelog\n");

    let cfg = ConfigSet::single(Config::default());
    let discovered = discover(&[root.to_path_buf()], &cfg, &[]);
    let paths: Vec<_> = discovered.iter().map(|f| f.path.as_path()).collect();

    assert!(
        paths.contains(&source.as_path()),
        "the root source file must be discovered, got {paths:?}"
    );
    assert!(
        !paths.contains(&vendored.as_path()),
        "files under node_modules must be pruned, got {paths:?}"
    );
    assert!(
        !paths.contains(&dependency.as_path()),
        "files under deps must be pruned, got {paths:?}"
    );
    assert_eq!(paths.len(), 1, "only the root source file should remain, got {paths:?}");
}

#[test]
fn honors_discovery_exclude_globs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let kept = root.join("src/main.py");
    let fixture = root.join("test_apps/app/main.py");
    let nested = root.join("packages/web/tools/vendor-x/gen.py");

    write_file(&kept, "x = 1\n");
    write_file(&fixture, "y = 2\n");
    write_file(&nested, "z = 3\n");

    let exclude = vec!["test_apps/**".to_string(), "packages/*/tools/vendor-*/**".to_string()];
    let cfg = ConfigSet::single(Config::default());
    let discovered = discover(&[root.to_path_buf()], &cfg, &exclude);
    let paths: Vec<_> = discovered.iter().map(|f| f.path.as_path()).collect();

    assert!(
        paths.contains(&kept.as_path()),
        "non-excluded source must survive, got {paths:?}"
    );
    assert!(
        !paths.contains(&fixture.as_path()),
        "files under an excluded dir must be pruned, got {paths:?}"
    );
    assert!(
        !paths.contains(&nested.as_path()),
        "wildcard excludes must match nested dirs, got {paths:?}"
    );
    assert_eq!(paths.len(), 1, "only the kept file remains, got {paths:?}");
}

#[test]
fn explicitly_passed_path_is_unaffected_by_other_roots() {
    // An exclude glob is matched relative to each walk root; passing a path
    // directly still discovers it (the glob never matches across roots).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let file = root.join("test_apps/app/main.py");
    write_file(&file, "x = 1\n");

    // Walk root is `test_apps/app`; the repo-rooted `test_apps/**` glob does not
    // match relative to this root, so the file is discovered.
    let cfg = ConfigSet::single(Config::default());
    let discovered = discover(&[root.join("test_apps/app")], &cfg, &["test_apps/**".to_string()]);
    let paths: Vec<_> = discovered.iter().map(|f| f.path.as_path()).collect();
    assert!(
        paths.contains(&file.as_path()),
        "a directly walked path is not pruned by a repo-rooted exclude, got {paths:?}"
    );
}
