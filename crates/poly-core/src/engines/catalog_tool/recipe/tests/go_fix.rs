//! golangci-lint's fixes, driven by a fake tool script.

use super::*;

/// A golangci-lint stand-in: `run --fix` appends a line to every `.go` file of
/// its working directory (refusing to run without the module's `go.mod` or with a
/// nested module copied in); a plain run reports one fixable finding.
const GO_FIXER: &str = r#"#!/bin/sh
echo "$PWD GOOS=$GOOS GIT_DIR=$GIT_DIR GIT_WORK_TREE=$GIT_WORK_TREE $*" >> "$POLY_TEST_LOG"
fix=0
for arg in "$@"; do [ "$arg" = "--fix" ] && fix=1; done
if [ "$fix" = 1 ]; then
  [ "$POLY_REJECT_FIX" = 1 ] && { echo "unknown flag: --fix" >&2; exit 2; }
  [ -f go.mod ] || { echo "no go.mod" >&2; exit 9; }
  [ -e nested ] && { echo "nested module copied" >&2; exit 8; }
  for f in *.go; do printf 'fixed\n' >> "$f"; done
  echo '{"Issues":[]}'
  exit 0
fi
printf '{"Issues":[{"FromLinter":"godot","Text":"fixable","Pos":{"Filename":"a.go","Line":1,"Column":1},"SuggestedFixes":[{"TextEdits":[]}]}]}'
exit 1
"#;

fn go_fix_recipe() -> serde_json::Value {
    serde_json::json!({
        "output": "golangci_json",
        "fix": "fix",
        "group": {"marker": ["go.mod"]},
        "target": "dir",
        "ok_exits": [1],
        "new_from_rev_flag": "--new-from-rev"
    })
}

fn go_fix_module(fx: &Fixture) -> Vec<SourceFile> {
    std::fs::write(fx.dir.path().join("go.mod"), "module x\n").expect("go.mod");
    std::fs::write(fx.dir.path().join("b.go"), "package a\n").expect("b.go");
    std::fs::create_dir_all(fx.dir.path().join("nested")).expect("nested");
    std::fs::write(fx.dir.path().join("nested/go.mod"), "module y\n").expect("nested go.mod");
    vec![fx.file("a.go", "package a\n")]
}

fn fix_log(fx: &Fixture) -> Vec<String> {
    std::fs::read_to_string(&fx.log)
        .expect("log")
        .lines()
        .filter(|line| line.contains("--fix"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn golangci_fixes_come_from_a_scratch_copy_of_the_module_and_the_tree_stays_untouched() {
    let fx = Fixture::new();
    let files = go_fix_module(&fx);
    let tool = fx.tool("run $DIR", go_fix_recipe(), Role::Lint, &fx.config(fx.script("golangci", GO_FIXER)));

    let results = tool.lint_files(&refs(&files));

    let diags = results[0].as_ref().expect("ok");
    let fix = &diags[0].fix;
    assert_eq!(fix.len(), 1, "the fixable finding carries the module-aware fix: {diags:?}");
    assert_eq!(fix[0].replacement, "package a\nfixed\n");
    assert_eq!(std::fs::read_to_string(fx.dir.path().join("a.go")).expect("a.go"), "package a\n");
    assert_eq!(std::fs::read_to_string(fx.dir.path().join("b.go")).expect("b.go"), "package a\n");
    let runs = fix_log(&fx);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].ends_with(" run --fix ."), "{}", runs[0]);
    let directory = runs[0].split(' ').next().expect("working directory");
    assert_ne!(
        Path::new(directory),
        std::fs::canonicalize(fx.dir.path()).expect("canonical"),
        "the fix must run in the scratch copy"
    );
}

#[test]
fn golangci_fixes_run_once_per_configured_goos() {
    let fx = Fixture::new();
    let files = go_fix_module(&fx);
    let mut config = fx.config(fx.script("golangci", GO_FIXER));
    config.goos = vec!["linux".to_owned(), "windows".to_owned()];
    let tool = fx.tool("run $DIR", go_fix_recipe(), Role::Lint, &config);

    let results = tool.lint_files(&refs(&files));

    let runs = fix_log(&fx);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs[0].contains("GOOS=linux") && runs[1].contains("GOOS=windows"), "{runs:?}");
    let diags = results[0].as_ref().expect("ok");
    assert_eq!(diags[0].fix[0].replacement, "package a\nfixed\nfixed\n");
}

#[test]
fn golangci_fixes_keep_the_new_from_rev_scope_and_the_repository() {
    let fx = Fixture::new();
    let files = go_fix_module(&fx);
    let git = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(fx.dir.path())
        .status()
        .expect("git init");
    assert!(git.success());
    let mut config = fx.config(fx.script("golangci", GO_FIXER));
    config.new_from_rev = Some("HEAD".to_owned());
    let tool = fx.tool("run $DIR", go_fix_recipe(), Role::Lint, &config);

    tool.lint_files(&refs(&files));

    let runs = fix_log(&fx);
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].contains(" run --fix --new-from-rev=HEAD ."), "{}", runs[0]);
    let real_git_dir = std::fs::canonicalize(fx.dir.path().join(".git")).expect(".git");
    assert!(
        runs[0].contains(&format!("GIT_DIR={}", real_git_dir.display())),
        "git must find the repository from the scratch copy: {}",
        runs[0]
    );
    assert!(
        !runs[0].contains(&format!("GIT_WORK_TREE={}", fx.dir.path().display())),
        "the work tree is the scratch copy: {}",
        runs[0]
    );
}

#[test]
fn a_golangci_without_the_fix_flag_leaves_the_findings_report_only() {
    let fx = Fixture::new();
    let files = go_fix_module(&fx);
    let mut config = fx.config(fx.script("golangci", GO_FIXER));
    config.env.insert("POLY_REJECT_FIX".to_owned(), "1".to_owned());
    let tool = fx.tool("run $DIR", go_fix_recipe(), Role::Lint, &config);

    let results = tool.lint_files(&refs(&files));

    let diags = results[0].as_ref().expect("lint still succeeds");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].fix.is_empty());
    assert!(diags[0].metadata.is_empty(), "no stale fixable marker: {:?}", diags[0].metadata);
}
