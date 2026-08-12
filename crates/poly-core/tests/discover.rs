//! File-discovery tests: vendored/build/cache directories must be pruned even
//! when they are tracked in git (so `.gitignore` does not exclude them).

use std::fs;
use std::path::Path;

use poly_core::discover::{discover, discover_reporting};
use poly_core::{Config, ConfigSet};

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

/// An exclude that pruned a whole subtree must be visible in the report, or a
/// summary built from it cannot distinguish "everything is clean" from
/// "everything I chose to look at is clean". Directories and files are counted
/// apart because only the file count is exact — a pruned directory is never
/// descended into.
#[test]
fn discovery_report_counts_pruned_directories_and_files_apart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write_file(&root.join("src/main.py"), "x = 1\n");
    write_file(&root.join("test_apps/app/main.py"), "y = 2\n");
    write_file(&root.join("test_apps/other/main.py"), "y = 3\n");
    write_file(&root.join("generated.py"), "z = 3\n");

    let exclude = vec!["test_apps/**".to_string(), "generated.py".to_string()];
    let cfg = ConfigSet::single(Config::default());
    let (discovered, report) = discover_reporting(&[root.to_path_buf()], &cfg, &exclude, false);

    assert_eq!(discovered.len(), 1, "only src/main.py survives: {discovered:?}");
    assert_eq!(
        report.excluded_directories, 1,
        "the excluded tree is pruned at its root, as one directory: {report:?}"
    );
    assert_eq!(
        report.excluded_files, 1,
        "an excluded file is counted exactly: {report:?}"
    );
    assert_eq!(report.excluded_explicit, 0);
    assert!(!report.is_empty());

    let by_pattern = |pattern: &str| {
        report
            .rules
            .iter()
            .find(|r| r.pattern == pattern)
            .unwrap_or_else(|| panic!("no rule {pattern} in {report:?}"))
            .clone()
    };
    assert_eq!(by_pattern("test_apps/**").directories, 1);
    assert_eq!(by_pattern("test_apps/**").files, 0);
    assert_eq!(by_pattern("generated.py").files, 1);
    assert_eq!(by_pattern("generated.py").directories, 0);
}

/// A rule that matched nothing is not reported: naming inert rules would train
/// readers to ignore the note that matters.
#[test]
fn discovery_report_omits_rules_that_matched_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_file(&root.join("src/main.py"), "x = 1\n");

    let exclude = vec!["never_matches/**".to_string()];
    let cfg = ConfigSet::single(Config::default());
    let (_, report) = discover_reporting(&[root.to_path_buf()], &cfg, &exclude, false);

    assert!(report.is_empty(), "nothing was pruned: {report:?}");
    assert!(report.rules.is_empty(), "inert rules stay unreported: {report:?}");
}

/// A run with no exclude configured must report nothing to qualify — the common
/// case stays silent.
#[test]
fn discovery_report_is_empty_without_excludes() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("src/main.py"), "x = 1\n");

    let cfg = ConfigSet::single(Config::default());
    let (_, report) = discover_reporting(&[dir.path().to_path_buf()], &cfg, &[], false);

    assert!(report.is_empty());
    assert_eq!(report.excluded_explicit, 0);
}

/// Under `--force-exclude` an explicitly named path can be dropped, leaving a run
/// that checked nothing at all. That is precisely the green-result-that-checked-
/// nothing case, so it is counted separately from a walk prune.
#[test]
fn discovery_report_counts_force_excluded_explicit_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let excluded = root.join("infra/main.tf");
    write_file(&excluded, "variable \"a\" {}\n");

    let exclude = vec!["**/*.tf".to_string()];
    let cfg = ConfigSet::single(Config::default());
    let (discovered, report) = discover_reporting(std::slice::from_ref(&excluded), &cfg, &exclude, true);

    assert!(discovered.is_empty(), "the named path is excluded: {discovered:?}");
    assert_eq!(report.excluded_explicit, 1, "{report:?}");
    assert_eq!(report.excluded_files, 1, "an explicit path is a file: {report:?}");
    assert_eq!(report.rules.len(), 1);
    assert_eq!(report.rules[0].pattern, "**/*.tf");

    // Without force_exclude the same path is checked and nothing is excluded, so
    // this cannot be mistaken for "the report always fires".
    let (discovered, report) = discover_reporting(&[excluded], &cfg, &exclude, false);
    assert_eq!(discovered.len(), 1);
    assert!(report.is_empty(), "{report:?}");
}

#[test]
fn explicitly_passed_path_is_unaffected_by_other_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let file = root.join("test_apps/app/main.py");
    write_file(&file, "x = 1\n");

    let cfg = ConfigSet::single(Config::default());
    let discovered = discover(&[root.join("test_apps/app")], &cfg, &["test_apps/**".to_string()]);
    let paths: Vec<_> = discovered.iter().map(|f| f.path.as_path()).collect();
    assert!(
        paths.contains(&file.as_path()),
        "a directly walked path is not pruned by a repo-rooted exclude, got {paths:?}"
    );
}
