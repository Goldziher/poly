//! The write paths of recipe tools, end to end through the runner with a fake
//! tool script standing in for the real one: `poly lint --fix` applying the
//! tool's own fixes, and `poly fmt --fix` (what a `[hooks.builtin] fmt = { fix =
//! true }` hook runs) writing a recipe formatter's output back.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use poly_core::{Config, RunOptions};

/// A buildifier lookalike: `-mode=check` reports an auto-fixable warning for a
/// file containing OLD; `-mode=fix` rewrites OLD to NEW.
const FAKE_BUILDIFIER: &str = r#"#!/bin/sh
case "$*" in
  *-mode=fix*)
    for f in "$@"; do
      [ -f "$f" ] && sed 's/OLD/NEW/' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
    done
    exit 0 ;;
esac
for f in "$@"; do
  if [ -f "$f" ] && grep -q OLD "$f"; then
    printf '{"success":false,"files":[{"filename":"%s","formatted":true,"valid":true,"warnings":[{"start":{"line":1,"column":1},"end":{"line":1,"column":4},"category":"old","actionable":true,"autoFixable":true,"message":"use NEW","url":"https://example.invalid/old"}]}]}' "$f"
    exit 4
  fi
done
printf '{"success":true,"files":[]}'
exit 0
"#;

/// A `tofu fmt -` lookalike: upper-cases stdin.
const FAKE_TOFU: &str = "#!/bin/sh\ncase \"$1\" in fmt) tr a-z A-Z ;; esac\n";

fn script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path.to_string_lossy().into_owned()
}

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    }
}

fn config(dir: &Path, toml: &str) -> Config {
    let path = dir.join("poly.toml");
    std::fs::write(&path, toml).expect("poly.toml");
    Config::load_file(&path).expect("load poly.toml")
}

#[test]
fn lint_fix_applies_the_edit_a_recipe_tool_attaches_to_its_finding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = script(dir.path(), "buildifier", FAKE_BUILDIFIER);
    let config = config(
        dir.path(),
        &format!("[tools.buildifier]\nenabled = true\nbin = \"{bin}\"\n"),
    );
    let build = dir.path().join("BUILD");
    std::fs::write(&build, "OLD()\n").expect("BUILD");

    let reported = poly_core::lint(&[dir.path().to_path_buf()], &config, &options(), false, false).expect("lint");
    let finding = reported
        .iter()
        .flat_map(|result| &result.diagnostics)
        .find(|d| d.engine == "buildifier")
        .expect("the check reports the finding");
    assert_eq!(finding.code.as_deref(), Some("old"));
    assert_eq!(finding.fix.len(), 1, "the tool's rewrite is attached as one edit");
    assert_eq!(finding.fix[0].replacement, "NEW()\n");
    assert_eq!(std::fs::read_to_string(&build).expect("BUILD"), "OLD()\n", "a check writes nothing");

    poly_core::lint(&[dir.path().to_path_buf()], &config, &options(), true, false).expect("lint --fix");
    assert_eq!(std::fs::read_to_string(&build).expect("BUILD"), "NEW()\n");
}

#[test]
fn fmt_fix_writes_what_a_recipe_formatter_produces() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = script(dir.path(), "tofu", FAKE_TOFU);
    let config = config(dir.path(), &format!("[tools.tofu]\nenabled = true\nbin = \"{bin}\"\n"));
    let main = dir.path().join("main.tf");
    std::fs::write(&main, "resource\n").expect("main.tf");

    let checked = poly_core::format_run(&[dir.path().to_path_buf()], &config, &options(), false, false).expect("check");
    assert!(checked.results.iter().any(|r| r.changed), "the formatter's change is seen");
    assert_eq!(std::fs::read_to_string(&main).expect("main.tf"), "resource\n", "a check writes nothing");

    let written = poly_core::format_run(&[dir.path().to_path_buf()], &config, &options(), true, false).expect("fix");
    assert!(written.results.iter().any(|r| r.changed));
    assert_eq!(std::fs::read_to_string(&main).expect("main.tf"), "RESOURCE\n");
}
