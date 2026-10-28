//! Catalog integrity + model tests. These validate the **vendored data** parses
//! and is internally consistent, and that the model helpers behave — they do not
//! invoke any external tool (the golden command fixtures are exercised by the
//! native-tool engine, which is where execution lives).

use serde::Deserialize;

use crate::{Catalog, Findings, Group, Output, PATH_PLACEHOLDER, Recipe, Target};

/// One `(input, output)` command fixture from the vendored `data/golden.json`.
#[derive(Debug, Deserialize)]
struct GoldenFixture {
    tool: String,
    command: String,
    language: String,
    input: String,
    output: String,
}

const GOLDEN_JSON: &str = include_str!("../data/golden.json");

#[test]
fn catalog_parses_and_is_non_trivial() {
    let catalog = Catalog::get();
    assert!(
        catalog.tools().len() > 300,
        "expected the full vendored catalog, got {}",
        catalog.tools().len()
    );
}

#[test]
fn every_tool_has_a_binary_and_at_least_one_command() {
    for tool in Catalog::get().tools() {
        assert!(!tool.name.is_empty(), "tool with empty name");
        assert!(!tool.binary.is_empty(), "{} has empty binary", tool.name);
        assert!(!tool.commands.is_empty(), "{} has no commands", tool.name);
    }
}

#[test]
fn lookup_resolves_known_tools_and_misses_unknown() {
    let catalog = Catalog::get();
    assert!(catalog.tool("shfmt").is_some());
    assert!(catalog.tool("gofmt").is_some());
    assert!(catalog.tool("this-tool-does-not-exist").is_none());
}

#[test]
fn shfmt_is_a_path_based_formatter() {
    let shfmt = Catalog::get().tool("shfmt").expect("shfmt present");
    assert_eq!(shfmt.binary, "shfmt");
    assert!(shfmt.is_formatter());
    let (_name, command) = shfmt.format_command().expect("shfmt formats");
    assert!(command.uses_path(), "shfmt formats a file path");
    assert!(!command.stdin);
}

#[test]
fn a_stdin_tool_is_flagged_as_stdin() {
    let cedar = Catalog::get().tool("cedar").expect("cedar present");
    let command = cedar.command("format").expect("cedar has a format command");
    assert!(command.stdin);
    assert!(!command.uses_path());
}

#[test]
fn pure_linter_does_not_masquerade_as_formatter() {
    if let Some(shellcheck) = Catalog::get().tool("shellcheck") {
        assert!(shellcheck.is_linter());
        assert!(shellcheck.format_command().is_none(), "shellcheck is not a formatter");
    }
}

#[test]
fn argv_substitutes_the_path_placeholder() {
    let gofmt = Catalog::get().tool("gofmt").expect("gofmt present");
    let (_name, command) = gofmt.format_command().expect("gofmt formats");
    let argv = command.argv("/work/main.go");
    assert!(
        argv.iter().any(|argument| argument == "/work/main.go"),
        "expected the path substituted into argv, got {argv:?}"
    );
    assert!(
        !argv.iter().any(|argument| argument == PATH_PLACEHOLDER),
        "no placeholder should survive substitution"
    );
}

#[test]
fn golden_fixtures_reference_real_catalog_commands() {
    let catalog = Catalog::get();
    let fixtures: Vec<GoldenFixture> = serde_json::from_str(GOLDEN_JSON).expect("vendored golden.json must be valid");
    assert!(!fixtures.is_empty(), "expected golden fixtures");

    for fixture in &fixtures {
        let tool = catalog
            .tool(&fixture.tool)
            .unwrap_or_else(|| panic!("golden fixture names unknown tool {}", fixture.tool));
        assert!(
            tool.command(&fixture.command).is_some(),
            "golden fixture for {} names unknown command {:?}",
            fixture.tool,
            fixture.command
        );
        assert!(!fixture.language.is_empty());
        let _ = (&fixture.input, &fixture.output);
    }
}

#[test]
fn actionlint_has_path_globs_scoped_to_workflows() {
    let catalog = Catalog::get();
    let tool = catalog.tool("actionlint").expect("actionlint present");
    assert!(
        !tool.path_globs.is_empty(),
        "actionlint must declare path_globs (it only lints GitHub Actions workflows)"
    );
    assert!(
        tool.path_globs.iter().any(|g| g.contains(".github/workflows")),
        "actionlint path_globs must reference .github/workflows; got: {:?}",
        tool.path_globs
    );
}

#[test]
fn the_overlay_adds_the_tools_the_mdsf_snapshot_lacks() {
    let catalog = Catalog::get();
    for name in [
        "checkstyle",
        "palantir-java-format",
        "helm",
        "helm-template",
        "tflint",
        "pyproject-fmt",
    ] {
        assert!(
            catalog.tool(name).is_some_and(|tool| tool.recipe.is_some()),
            "{name} needs a recipe"
        );
    }
}

#[test]
fn the_overlay_keeps_the_mdsf_commands_the_golden_fixtures_name() {
    let actionlint = Catalog::get().tool("actionlint").expect("actionlint");
    assert!(
        actionlint.command("").is_some(),
        "mdsf's default command survives the merge"
    );
    assert!(actionlint.command("lint").is_some(), "the recipe command is added");
}

#[test]
fn every_recipe_command_is_runnable_with_the_run_s_files() {
    for tool in Catalog::get().tools().iter().filter(|tool| tool.recipe.is_some()) {
        for (name, command) in &tool.commands {
            if name.is_empty() || command.arguments.iter().any(|a| a == PATH_PLACEHOLDER) {
                continue;
            }
            let takes_target = command.batch_arguments().is_some()
                || command
                    .arguments
                    .iter()
                    .any(|a| a.contains("$ROOT") || a.contains("$DIR"));
            assert!(takes_target, "{}:{name} names no file, directory or root", tool.name);
        }
    }
}

#[test]
fn pinned_releases_carry_a_sha256_for_every_asset() {
    for tool in Catalog::get().tools() {
        let Some(install) = tool.recipe.as_ref().and_then(|recipe| recipe.install.as_ref()) else {
            continue;
        };
        for (version, release) in &install.releases {
            assert!(!release.assets.is_empty(), "{} {version} has no assets", tool.name);
            for (platform, asset) in &release.assets {
                assert_eq!(asset.sha256.len(), 64, "{} {version} {platform}", tool.name);
                assert!(
                    asset
                        .sha256
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                    "{}",
                    tool.name
                );
                assert!(asset.url.starts_with("https://"), "{} {version} {platform}", tool.name);
            }
        }
    }
}

#[test]
fn buildifier_formats_lints_and_fixes_starlark() {
    let buildifier = Catalog::get().tool("buildifier").expect("buildifier");
    assert_eq!(buildifier.languages, ["starlark"]);
    assert!(buildifier.format_command().is_some());
    assert!(buildifier.lint_command().is_some());
    assert!(buildifier.command("fix").is_some_and(|c| c.uses_files()));
}

fn recipe_of(name: &str) -> &'static Recipe {
    Catalog::get()
        .tool(name)
        .and_then(|tool| tool.recipe.as_ref())
        .unwrap_or_else(|| panic!("{name} has a recipe"))
}

#[test]
fn helm_is_a_chart_wide_recipe_that_prepares_dependencies_and_takes_values_files() {
    for name in ["helm", "helm-template"] {
        let recipe = recipe_of(name);
        assert_eq!(recipe.output, Output::HelmText, "{name}");
        assert_eq!(recipe.group, Group::Marker(vec!["Chart.yaml".to_owned()]), "{name}");
        assert_eq!(recipe.target, Target::Root, "{name}");
        assert_eq!(recipe.findings, Findings::All, "{name}");
        assert_eq!(recipe.values_flag.as_deref(), Some("-f"), "{name}");
        let prepare = recipe.prepare.as_deref().expect("a prepare step");
        let command = Catalog::get().tool(name).expect(name).command(prepare).expect("prepare command");
        assert_eq!(command.arguments, ["dependency", "build", "$ROOT"], "{name}");
        let tool = Catalog::get().tool(name).expect(name);
        assert!(tool.languages.iter().any(|l| l == "helm-template"), "{name} claims .tpl and NOTES.txt");
        assert!(tool.path_globs.iter().any(|g| g == "**/templates/**"), "{name}");
    }
}

#[test]
fn golangci_lint_is_a_per_module_recipe_with_new_from_rev_and_config_discovery() {
    let recipe = recipe_of("golangci-lint");
    assert_eq!(recipe.output, Output::GolangciJson);
    assert_eq!(recipe.group, Group::Marker(vec!["go.mod".to_owned()]));
    assert_eq!(recipe.target, Target::Dir);
    assert!(recipe.serial, "golangci-lint holds a global cache lock");
    assert_eq!(recipe.ok_exits, [1]);
    assert_eq!(recipe.new_from_rev_flag.as_deref(), Some("--new-from-rev"));
    assert!(recipe.config_files.iter().any(|f| f == ".golangci.yml"));
    assert!(recipe.config_flags.iter().any(|f| f == "-c"));
}

#[test]
fn buf_discovers_its_module_and_config_through_buf_yaml() {
    let recipe = recipe_of("buf");
    assert_eq!(recipe.output, Output::BufJson);
    assert_eq!(
        recipe.group,
        Group::Marker(vec!["buf.yaml".to_owned(), "buf.work.yaml".to_owned()])
    );
    assert!(recipe.config_files.iter().any(|f| f == "buf.yaml"));
    assert!(recipe.dependencies.iter().any(|d| d == "**/*.proto"));
    let lint = Catalog::get().tool("buf").expect("buf").command("lint").expect("lint");
    assert_eq!(lint.arguments, ["lint", "--error-format=json", "$FILES:--path"]);
}

#[test]
fn tflint_runs_once_per_directory_and_treats_exit_2_as_findings() {
    let recipe = recipe_of("tflint");
    assert_eq!(recipe.output, Output::TflintJson);
    assert_eq!(recipe.group, Group::Dir);
    assert_eq!(recipe.ok_exits, [2]);
    assert!(recipe.dependencies.iter().any(|d| d == ".tflint.hcl"));
}

#[test]
fn hadolint_reads_json_and_treats_exit_1_as_findings() {
    let recipe = recipe_of("hadolint");
    assert_eq!(recipe.output, Output::HadolintJson);
    assert_eq!(recipe.ok_exits, [1]);
    assert!(recipe.config_files.iter().any(|f| f == ".hadolint.yaml"));
}

#[test]
fn archive_tools_are_pinned_for_every_platform_with_a_member_inside_the_archive() {
    for (name, version) in [
        ("helm", "4.3.1"),
        ("helm-template", "4.3.1"),
        ("golangci-lint", "2.15.0"),
        ("actionlint", "1.7.12"),
        ("tflint", "0.65.0"),
        ("tofu", "1.13.2"),
    ] {
        let install = recipe_of(name).install.as_ref().expect("install");
        let release = install.releases.get(version).unwrap_or_else(|| panic!("{name} {version}"));
        assert_eq!(release.kind, crate::ReleaseKind::Binary);
        for platform in ["darwin-aarch64", "darwin-x86_64", "linux-aarch64", "linux-x86_64"] {
            let asset = release.assets.get(platform).unwrap_or_else(|| panic!("{name} lacks {platform}"));
            let member = asset.member.as_deref().unwrap_or_else(|| panic!("{name} {platform} has no member"));
            assert!(!member.starts_with('/') && !member.split('/').any(|part| part == ".."), "{member}");
            assert!(
                [".tar.gz", ".tgz", ".zip"].iter().any(|ext| asset.url.ends_with(ext)),
                "{name} {platform}: {}",
                asset.url
            );
        }
    }
}

#[test]
fn buf_and_hadolint_are_pinned_for_every_desktop_and_server_platform() {
    for (name, version) in [("buf", "1.75.0"), ("hadolint", "2.15.1")] {
        let install = recipe_of(name).install.as_ref().expect("install");
        let release = install.releases.get(version).unwrap_or_else(|| panic!("{name} {version}"));
        assert_eq!(release.kind, crate::ReleaseKind::Binary);
        for platform in ["darwin-aarch64", "darwin-x86_64", "linux-aarch64", "linux-x86_64"] {
            assert!(release.assets.contains_key(platform), "{name} lacks {platform}");
        }
    }
}
