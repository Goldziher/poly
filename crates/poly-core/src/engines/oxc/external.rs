//! The real `oxlint` and `oxfmt` binaries, for what poly's in-process engines cannot run.
//!
//! `jsPlugins` in an `.oxlintrc.json` are JavaScript modules and `sortTailwindcss` is a JavaScript
//! class sorter; neither can run without a JS runtime. `[lint.<lang>.oxc] js_plugins = "external"`
//! and `[fmt.<lang>.oxc] external = true` hand just those files to the project's own binary, found
//! in the nearest `node_modules/.bin` or on `PATH`, and read the answer back. Without the binary
//! (or without the option) the files are linted and formatted in process and the unsupported part
//! is skipped with a warning.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use serde::Deserialize;

use crate::config::EngineConfig;
use crate::engine::{Diagnostic, Severity, SourceFile, Span};
use crate::engines::line_index::LineIndex;

const OXLINT: &str = "oxlint";
const OXFMT: &str = "oxfmt";
const FILES_PER_RUN: usize = 400;

/// Whether `[lint.<lang>.oxc] js_plugins = "external"`.
pub(super) fn lint_requested(cfg: &EngineConfig) -> bool {
    cfg.options.get("js_plugins").and_then(toml::Value::as_str) == Some("external")
}

/// Whether `[fmt.<lang>.oxc] external = true`.
pub(super) fn format_requested(cfg: &EngineConfig) -> bool {
    cfg.options
        .get("external")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

/// `name` from the nearest `node_modules/.bin` at or above `start`, else from `path_var`.
pub(super) fn find_binary(name: &str, start: &Path, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join("node_modules").join(".bin").join(name))
        .find(|candidate| candidate.is_file())
        .or_else(|| {
            path_var
                .into_iter()
                .flat_map(std::env::split_paths)
                .map(|dir| dir.join(name))
                .find(|candidate| candidate.is_file())
        })
}

fn locate(name: &str, start: &Path) -> Option<PathBuf> {
    find_binary(name, start, std::env::var_os("PATH").as_deref())
}

/// Whether the oxlint config at `config` declares `jsPlugins`, parsed once per file.
fn declares_js_plugins(config: &Path) -> bool {
    static MEMO: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let memo = MEMO.get_or_init(Mutex::default);
    if let Some(known) = memo
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(config)
    {
        return *known;
    }
    let declares = oxc_linter::Oxlintrc::from_file(config)
        .map(|rc| rc.external_plugins.iter().flatten().next().is_some())
        .unwrap_or(false);
    memo.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(config.to_path_buf(), declares);
    declares
}

/// Lint `batch`: files governed by a config that declares `jsPlugins` are linted by `oxlint`, one
/// run per config, and every other file (or any file whose run could not happen) by `lint_one`.
pub(super) fn lint_batch(
    batch: &[SourceFile],
    cfg: &EngineConfig,
    lint_one: impl Fn(&SourceFile) -> anyhow::Result<Vec<Diagnostic>>,
) -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
    let mut groups: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (index, src) in batch.iter().enumerate() {
        let Some(governing) = super::native_lint::governing_for(cfg, &src.path) else {
            continue;
        };
        if governing
            .ignore
            .as_ref()
            .is_some_and(|ignore| ignore.is_ignored(&src.path))
        {
            continue;
        }
        if declares_js_plugins(&governing.nearest) {
            groups.entry(governing.nearest.clone()).or_default().push(index);
        }
    }

    let mut external: HashMap<usize, Vec<Diagnostic>> = HashMap::new();
    for (config, members) in groups {
        let dir = config.parent().unwrap_or(Path::new("."));
        let Some(binary) = locate(OXLINT, dir) else {
            tracing::warn!(
                "`js_plugins = \"external\"` needs `oxlint`, which was not found in node_modules/.bin or on PATH; \
                 the jsPlugins of {} are skipped. Install oxlint or set `js_plugins = \"skip\"`.",
                config.display()
            );
            continue;
        };
        let files: Vec<&SourceFile> = members.iter().map(|&index| &batch[index]).collect();
        match run_oxlint(&binary, &config, &files) {
            Ok(per_file) => external.extend(members.iter().copied().zip(per_file)),
            Err(error) => tracing::warn!(
                "{} failed for {}: {error:#}; its files are linted without the jsPlugins",
                binary.display(),
                config.display()
            ),
        }
    }

    Ok(batch
        .iter()
        .enumerate()
        .map(|(index, src)| match external.remove(&index) {
            Some(diagnostics) => Ok(diagnostics),
            None => lint_one(src),
        })
        .collect())
}

#[derive(Deserialize)]
struct Report {
    diagnostics: Vec<Finding>,
}

#[derive(Deserialize)]
struct Finding {
    message: String,
    #[serde(default)]
    code: Option<String>,
    severity: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    help: Option<String>,
    filename: String,
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Label {
    span: LabelSpan,
}

#[derive(Deserialize)]
struct LabelSpan {
    offset: usize,
    length: usize,
}

/// `eslint(no-debugger)` as `no-debugger`, `my-plugin(rule)` as `my-plugin/rule`.
fn rule_code(code: &str) -> String {
    match code.strip_suffix(')').and_then(|rest| rest.split_once('(')) {
        Some(("eslint", rule)) => rule.to_owned(),
        Some((scope, rule)) => format!("{scope}/{rule}"),
        None => code.to_owned(),
    }
}

fn severity_of(name: &str) -> Severity {
    match name {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    }
}

/// Lint `files` with one `oxlint` run per [`FILES_PER_RUN`], under the single `config`, and return
/// the findings of each file in order.
fn run_oxlint(binary: &Path, config: &Path, files: &[&SourceFile]) -> anyhow::Result<Vec<Vec<Diagnostic>>> {
    let dir = config.parent().unwrap_or(Path::new("."));
    let absolute: Vec<PathBuf> = files
        .iter()
        .map(|src| std::path::absolute(&src.path).unwrap_or_else(|_| src.path.clone()))
        .collect();
    let positions: HashMap<&Path, usize> = absolute
        .iter()
        .enumerate()
        .map(|(index, path)| (path.as_path(), index))
        .collect();
    let mut results: Vec<Vec<Diagnostic>> = vec![Vec::new(); files.len()];
    let mut indexes: HashMap<usize, LineIndex> = HashMap::new();

    for chunk in absolute.chunks(FILES_PER_RUN) {
        let output = Command::new(binary)
            .current_dir(dir)
            .arg("--config")
            .arg(config)
            .arg("--disable-nested-config")
            .args(["--format", "json"])
            .args(chunk)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("cannot run {}", binary.display()))?;
        let report: Report = serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "{} printed no JSON report ({}): {}",
                binary.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
        for finding in report.diagnostics {
            let named = Path::new(&finding.filename);
            let full = if named.is_absolute() {
                named.to_path_buf()
            } else {
                dir.join(named)
            };
            let Some(&index) = positions.get(full.as_path()) else {
                continue;
            };
            let span = finding.labels.first().map(|label| {
                let line_index = indexes
                    .entry(index)
                    .or_insert_with(|| LineIndex::new(&files[index].content));
                let (start_line, start_col) = line_index.line_col(label.span.offset);
                let (end_line, end_col) = line_index.line_col(label.span.offset + label.span.length);
                Span {
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                }
            });
            results[index].push(Diagnostic {
                engine: "oxc".to_owned(),
                code: finding.code.as_deref().map(rule_code),
                severity: severity_of(&finding.severity),
                title: finding.message,
                description: finding.help,
                span,
                url: finding.url,
                fix: Vec::new(),
                metadata: Default::default(),
            });
        }
    }
    Ok(results)
}

/// Format `src` with the project's `oxfmt`, or `None` when none is installed or it fails (the
/// caller then formats in process).
pub(super) fn format_with_oxfmt(src: &SourceFile, config: Option<&Path>) -> Option<String> {
    let start = std::path::absolute(&src.path).ok()?;
    let Some(binary) = locate(OXFMT, start.parent()?) else {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            tracing::warn!(
                "`external = true` needs `oxfmt`, which was not found in node_modules/.bin or on PATH; files that \
                 need its JavaScript-side options (Tailwind class sorting) are formatted without them"
            );
        });
        return None;
    };
    let mut command = Command::new(binary);
    command
        .current_dir(config.and_then(Path::parent).unwrap_or(Path::new(".")))
        .arg("--stdin-filepath")
        .arg(&start);
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let content = src.content.clone();
    let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, content.as_bytes()));
    let output = child.wait_with_output().ok()?;
    writer.join().ok()?.ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::config::GlobalDefaults;
    use crate::language::Language;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn source(path: &Path) -> SourceFile {
        SourceFile {
            path: path.to_path_buf(),
            language: Language::JavaScript,
            content: "debugger;\nfoo();\n".into(),
        }
    }

    const REPORT: &str = r#"{"diagnostics":[
        {"message":"custom finding","code":"my-plugin(no-foo)","severity":"error","filename":"a.js",
         "labels":[{"span":{"offset":10,"length":3,"line":2,"column":1}}]},
        {"message":"`debugger` statement is not allowed","code":"eslint(no-debugger)","severity":"warning",
         "help":"Remove the debugger statement","filename":"a.js",
         "labels":[{"span":{"offset":0,"length":9,"line":1,"column":1}}]},
        {"message":"elsewhere","code":"eslint(curly)","severity":"warning","filename":"other.js","labels":[]}
    ],"number_of_files":1}"#;

    #[test]
    fn the_nearest_node_modules_binary_wins_over_path() {
        let repo = tempfile::tempdir().unwrap();
        let local = script(&repo.path().join("node_modules/.bin"), "oxlint", "exit 0");
        let on_path = script(&repo.path().join("bin"), "oxlint", "exit 0");
        let nested = repo.path().join("packages/ui");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            find_binary("oxlint", &nested, Some(on_path.parent().unwrap().as_os_str())),
            Some(local)
        );
        std::fs::remove_dir_all(repo.path().join("node_modules")).unwrap();
        assert_eq!(
            find_binary("oxlint", &nested, Some(on_path.parent().unwrap().as_os_str())),
            Some(on_path)
        );
        assert_eq!(find_binary("oxlint", &nested, None), None);
    }

    #[test]
    fn an_oxlint_report_becomes_poly_diagnostics_per_file() {
        let repo = tempfile::tempdir().unwrap();
        let config = repo.path().join(".oxlintrc.json");
        std::fs::write(&config, "{}").unwrap();
        let canned = repo.path().join("report.json");
        std::fs::write(&canned, REPORT).unwrap();
        let fake = script(
            &repo.path().join("bin"),
            "oxlint",
            &format!(
                "printf '%s\\n' \"$@\" > '{}/args.txt'\ncat '{}'",
                repo.path().display(),
                canned.display()
            ),
        );
        let file = repo.path().join("a.js");
        let src = source(&file);

        let results = run_oxlint(&fake, &config, &[&src]).unwrap();

        assert_eq!(results.len(), 1);
        let codes: Vec<_> = results[0].iter().map(|d| d.code.as_deref().unwrap()).collect();
        assert_eq!(codes, ["my-plugin/no-foo", "no-debugger"]);
        assert_eq!(results[0][0].severity, Severity::Error);
        assert_eq!(results[0][0].span.as_ref().unwrap().start_line, 2);
        assert_eq!(
            results[0][1].description.as_deref(),
            Some("Remove the debugger statement")
        );
        let args = std::fs::read_to_string(repo.path().join("args.txt")).unwrap();
        assert!(args.contains("--disable-nested-config"), "{args}");
        assert!(args.contains("--format\njson"), "{args}");
        assert!(args.contains(&file.display().to_string()), "{args}");
    }

    #[test]
    fn oxfmt_formats_the_file_read_from_stdin() {
        let repo = tempfile::tempdir().unwrap();
        script(
            &repo.path().join("node_modules/.bin"),
            "oxfmt",
            "cat > /dev/null\necho 'sorted classes'",
        );
        let src = source(&repo.path().join("src/a.tsx"));
        assert_eq!(format_with_oxfmt(&src, None).as_deref(), Some("sorted classes\n"));
    }

    #[test]
    fn a_failing_oxfmt_leaves_the_file_to_the_in_process_formatter() {
        let repo = tempfile::tempdir().unwrap();
        script(
            &repo.path().join("node_modules/.bin"),
            "oxfmt",
            "cat > /dev/null\nexit 2",
        );
        let src = source(&repo.path().join("a.tsx"));
        assert_eq!(format_with_oxfmt(&src, None), None);
    }

    #[test]
    fn files_needing_javascript_side_options_go_to_oxfmt_only_when_asked() {
        let repo = tempfile::tempdir().unwrap();
        script(
            &repo.path().join("node_modules/.bin"),
            "oxfmt",
            "cat > /dev/null\necho 'from oxfmt'",
        );
        let src = source(&repo.path().join("a.tsx"));
        let table = |extra: &str| EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(&format!("requires_oxfmt = true\n{extra}")).unwrap(),
        };
        let formatted = |cfg: &EngineConfig| match super::super::format::format_js(&src, cfg).unwrap() {
            crate::engine::FormatOutput::Formatted(text) => text,
            crate::engine::FormatOutput::Unchanged => src.content.to_string(),
        };
        assert_eq!(formatted(&table("external = true")), "from oxfmt\n");
        assert_eq!(formatted(&table("")), "debugger;\nfoo();\n");
    }

    #[test]
    fn output_that_is_not_json_fails_the_run_instead_of_reporting_nothing() {
        let repo = tempfile::tempdir().unwrap();
        let config = repo.path().join(".oxlintrc.json");
        std::fs::write(&config, "{}").unwrap();
        let fake = script(&repo.path().join("bin"), "oxlint", "echo 'boom' >&2\nexit 2");
        let src = source(&repo.path().join("a.js"));
        assert!(run_oxlint(&fake, &config, &[&src]).is_err());
    }

    #[test]
    fn only_files_under_a_js_plugins_config_leave_the_process() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let root = repo.path().join(".oxlintrc.json");
        std::fs::write(&root, r#"{"rules":{}}"#).unwrap();
        std::fs::write(repo.path().join("plugin.js"), "export default {};\n").unwrap();
        std::fs::create_dir_all(repo.path().join("tools")).unwrap();
        std::fs::write(
            repo.path().join("tools/.oxlintrc.json"),
            r#"{"jsPlugins":["../plugin.js"],"rules":{}}"#,
        )
        .unwrap();
        let canned = repo.path().join("report.json");
        std::fs::write(&canned, REPORT).unwrap();
        let bin = script(
            &repo.path().join("node_modules/.bin"),
            "oxlint",
            &format!("cat '{}'", canned.display()),
        );
        assert!(bin.is_file());

        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(&format!("js_plugins = \"external\"\noxlintrc = \"{}\"", root.display())).unwrap(),
        };
        let plain = source(&repo.path().join("a.js"));
        let governed = source(&repo.path().join("tools/a.js"));
        let results = lint_batch(&[plain, governed], &cfg, |_| Ok(vec![])).unwrap();

        assert!(results[0].as_ref().unwrap().is_empty());
        let governed: Vec<_> = results[1]
            .as_ref()
            .unwrap()
            .iter()
            .map(|d| d.code.clone().unwrap())
            .collect();
        assert_eq!(governed, ["my-plugin/no-foo", "no-debugger"]);
    }
}
