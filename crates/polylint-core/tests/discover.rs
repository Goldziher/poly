//! File-discovery tests: vendored/build/cache directories must be pruned even
//! when they are tracked in git (so `.gitignore` does not exclude them).

use std::fs;
use std::path::Path;

use polylint_core::discover::discover;

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

    let discovered = discover(&[root.to_path_buf()]);
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
    assert_eq!(
        paths.len(),
        1,
        "only the root source file should remain, got {paths:?}"
    );
}
