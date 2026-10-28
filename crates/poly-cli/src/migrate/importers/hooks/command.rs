//! Reading a hook `run` command line: shell tokenizing, file-placeholder
//! detection, and recognition of the tools poly can run itself.
//!
//! Recognition is deliberately strict. A command maps to a poly builtin only
//! when every flag on it is either known to be irrelevant to poly's own
//! invocation or equivalent to what poly does by default; anything else stays a
//! command job so its flags keep their effect.

use super::acc::ToolSettings;

/// File-list placeholders poly substitutes in a `run` line.
const FILE_PLACEHOLDERS: &[&str] = &["staged_files", "all_files"];

/// Whether poly substitutes `{name}` in a `run` line: the file lists, the git hook
/// arguments joined (`{0}`, `{git_args}`) and one git hook argument (`{1}`, ...).
pub(super) fn is_supported_placeholder(name: &str) -> bool {
    FILE_PLACEHOLDERS.contains(&name)
        || name == "git_args"
        || (!name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `token` references the git hook arguments.
pub(super) fn uses_git_args(token: &str) -> bool {
    placeholders(token)
        .iter()
        .any(|name| name == "git_args" || name.bytes().all(|b| b.is_ascii_digit()))
}

/// Which poly builtin a recognized tool invocation maps onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Target {
    Fmt,
    Lint,
}

/// A tool invocation poly can run through `[hooks.builtin]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Recognized {
    pub target: Target,
    /// Catalog tools to enable under `[tools.<name>]`; empty for native engines.
    pub catalog_tools: &'static [&'static str],
    /// Whether the command rewrites the files it is given, so the builtin must run
    /// in fix mode to behave the same.
    pub fixes: bool,
    /// Engine options the command's flags map onto.
    pub options: Vec<EngineOption>,
    /// How the catalog tools are configured, so the flags that shape their
    /// output survive the migration (`tool`, settings).
    pub settings: Vec<(&'static str, ToolSettings)>,
    /// Behaviour of the original command that poly's equivalent does not have.
    pub approximations: Vec<&'static str>,
}

/// A boolean engine option switched on by a command-line flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EngineOption {
    /// The `poly.toml` table, e.g. `["lint", "python", "ruff"]`.
    pub table: &'static [&'static str],
    pub key: &'static str,
}

pub(super) const RUFF_UNSAFE_FIXES: EngineOption = EngineOption {
    table: &["lint", "python", "ruff"],
    key: "unsafe_fixes",
};

/// Outcome of looking at a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Recognition {
    /// Maps onto a builtin.
    Builtin(Recognized),
    /// A known tool, but with flags poly cannot reproduce.
    FlagsNotRepresentable { tool: &'static str, flags: Vec<String> },
    /// A tool poly has no equivalent for (with the reason).
    NoEquivalent(&'static str),
    /// Nothing recognized.
    Unknown,
}

/// Split a command line into shell words, keeping quotes and `$(...)` intact.
pub(super) fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                current.push(c);
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                current.push(c);
            }
            (None, '$') if chars.peek() == Some(&'(') => {
                depth += 1;
                current.push(c);
                current.push(chars.next().expect("peeked"));
            }
            (None, '(') if depth > 0 => {
                depth += 1;
                current.push(c);
            }
            (None, ')') if depth > 0 => {
                depth -= 1;
                current.push(c);
            }
            (None, c) if c.is_whitespace() && depth == 0 => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            (None, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Remove one level of surrounding quotes from a token.
pub(super) fn unquote(token: &str) -> &str {
    for quote in ['\'', '"'] {
        if let Some(inner) = token.strip_prefix(quote).and_then(|t| t.strip_suffix(quote)) {
            return inner;
        }
    }
    token
}

/// Names of every `{placeholder}` in `run` that is not `${...}` shell syntax.
pub(super) fn placeholders(run: &str) -> Vec<String> {
    let bytes = run.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'{'
            && (index == 0 || bytes[index - 1] != b'$')
            && let Some(len) = run[index + 1..].find('}')
        {
            let name = &run[index + 1..index + 1 + len];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                found.push(name.to_string());
            }
            index += len + 2;
            continue;
        }
        index += 1;
    }
    found
}

/// Placeholders in `run` that poly does not substitute.
pub(super) fn unsupported_placeholders(run: &str) -> Vec<String> {
    let mut names: Vec<String> = placeholders(run)
        .into_iter()
        .filter(|name| !is_supported_placeholder(name))
        .collect();
    names.dedup();
    names
}

/// Whether `token` passes the staged file list.
pub(super) fn is_file_list(token: &str) -> bool {
    FILE_PLACEHOLDERS
        .iter()
        .any(|name| token.contains(&format!("{{{name}}}")))
}

/// Why a tool has no poly counterpart, keyed by a substring of the command.
const NO_EQUIVALENT: &[(&str, &str)] = &[];

/// Wrapper scripts that exist only to find, pin and launch a tool, and the tool
/// they launch. Recognised by file name: the wrapper's job is what `[tools]`
/// `version` / `install` / `bin` now do.
const LAUNCHER_SCRIPTS: &[(&str, &str)] = &[
    ("run_checkstyle.sh", "checkstyle"),
    ("run_palantir_format.sh", "palantir-java-format"),
    ("run_groovy_lint.sh", "npm-groovy-lint"),
    ("lint_changed.sh", "golangci-lint-changed"),
    ("check_charts.sh", "helm-charts"),
];

/// The program a command runs, looking through `bash script.sh` / `./script.sh`
/// launchers to the tool inside. Returns the tool and the arguments after it.
fn program_and_args(tokens: &[String]) -> Option<(&str, &[String])> {
    let first = tokens.first()?.as_str();
    let (script, rest) = match first {
        "bash" | "sh" | "zsh" => (tokens.get(1)?.as_str(), &tokens[2..]),
        other => (other, &tokens[1..]),
    };
    let name = script.rsplit('/').next().unwrap_or(script);
    match LAUNCHER_SCRIPTS.iter().find(|(file, _)| *file == name) {
        Some((_, tool)) => Some((tool, rest)),
        None if matches!(first, "bash" | "sh" | "zsh") => None,
        None => Some((first, &tokens[1..])),
    }
}

/// Recognize the tool a `run` line invokes.
pub(super) fn recognize(run: &str) -> Recognition {
    let tokens = tokenize(run);
    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), "&&" | "||" | ";" | "|" | ">" | ">>"))
    {
        return Recognition::Unknown;
    }
    let Some((program, rest)) = program_and_args(&tokens) else {
        return no_equivalent(run).map_or(Recognition::Unknown, Recognition::NoEquivalent);
    };
    if !rest.iter().any(|t| is_file_list(t)) {
        return no_equivalent(run).map_or(Recognition::Unknown, Recognition::NoEquivalent);
    }
    let args: Vec<&str> = rest.iter().map(String::as_str).filter(|t| !is_file_list(t)).collect();
    let recognition = match program {
        "ruff" => ruff(&args),
        "oxlint" => simple(
            "oxlint",
            Target::Lint,
            &[],
            &args,
            &["--fix", "--no-error-on-unmatched-pattern", "--quiet"],
        ),
        "oxfmt" => simple(
            "oxfmt",
            Target::Fmt,
            &[],
            &args,
            &["--write", "--no-error-on-unmatched-pattern"],
        ),
        "rubyfmt" => simple("rubyfmt", Target::Fmt, &[], &args, &["--in-place", "-i"]),
        "actionlint" => actionlint(&args),
        "golangci-lint" => golangci(&args),
        "golangci-lint-changed" => golangci_changed(&args),
        "buf" => buf(&args),
        "npm-groovy-lint" => groovy(&args),
        "checkstyle" => checkstyle(&args),
        "palantir-java-format" => palantir(&args),
        "pyproject-fmt" => pyproject_fmt(&args),
        "buildifier" => buildifier(&args),
        "terraform" => terraform("terraform", &["terraform"], &args),
        "tofu" => terraform("tofu", &["tofu"], &args),
        "helm-charts" => helm_charts(&args),
        _ => Recognition::Unknown,
    };
    match recognition {
        Recognition::Unknown => no_equivalent(run).map_or(Recognition::Unknown, Recognition::NoEquivalent),
        Recognition::Builtin(mut recognized) => {
            recognized.fixes = rewrites_files(program, &args);
            Recognition::Builtin(recognized)
        }
        other => other,
    }
}

/// Whether `program args` rewrites the files it is given rather than only
/// reporting on them.
fn rewrites_files(program: &str, args: &[&str]) -> bool {
    let has = |flags: &[&str]| args.iter().any(|arg| flags.contains(arg));
    match (program, args.first().copied()) {
        ("ruff", Some("format")) | ("golangci-lint", Some("fmt")) => true,
        ("ruff", _) => has(&["--fix", "--fix-only"]),
        ("oxlint", _) => has(&["--fix"]),
        ("oxfmt", _) => has(&["--write"]),
        ("rubyfmt", _) => has(&["--in-place", "-i"]),
        ("golangci-lint", _) => has(&["--fix"]),
        ("buf", _) => has(&["-w", "--write"]),
        ("npm-groovy-lint", _) => has(&["--fix", "--format"]),
        ("palantir-java-format", _) => has(&["--replace", "-r", "--in-place"]),
        ("pyproject-fmt", _) => !has(&["--check", "--stdout"]),
        ("buildifier", _) => !args.iter().any(|arg| {
            matches!(
                arg.trim_start_matches('-'),
                "mode=check" | "mode=diff" | "mode=print_if_changed"
            )
        }),
        ("terraform" | "tofu", _) => !has(&["-check", "-write=false"]),
        _ => false,
    }
}

/// The documented reason poly has no counterpart for `run`, if known.
pub(super) fn no_equivalent(run: &str) -> Option<&'static str> {
    NO_EQUIVALENT
        .iter()
        .find(|(needle, _)| run.contains(needle))
        .map(|(_, reason)| *reason)
}

fn builtin(target: Target, catalog_tools: &'static [&'static str], options: Vec<EngineOption>) -> Recognition {
    Recognition::Builtin(Recognized {
        target,
        catalog_tools,
        fixes: false,
        options,
        settings: Vec::new(),
        approximations: Vec::new(),
    })
}

fn configured(
    target: Target,
    catalog_tools: &'static [&'static str],
    settings: Vec<(&'static str, ToolSettings)>,
    approximations: Vec<&'static str>,
) -> Recognition {
    Recognition::Builtin(Recognized {
        target,
        catalog_tools,
        fixes: false,
        options: Vec::new(),
        settings,
        approximations,
    })
}

fn not_representable(tool: &'static str, flags: Vec<String>) -> Recognition {
    Recognition::FlagsNotRepresentable { tool, flags }
}

fn simple(
    tool: &'static str,
    target: Target,
    catalog_tools: &'static [&'static str],
    args: &[&str],
    benign: &[&str],
) -> Recognition {
    let unknown: Vec<String> = args
        .iter()
        .filter(|a| !benign.contains(a))
        .map(|a| (*a).to_string())
        .collect();
    if unknown.is_empty() {
        builtin(target, catalog_tools, Vec::new())
    } else {
        Recognition::FlagsNotRepresentable { tool, flags: unknown }
    }
}

fn ruff(args: &[&str]) -> Recognition {
    let (target, rest, benign): (Target, &[&str], &[&str]) = match args.first().copied() {
        Some("format") => (Target::Fmt, &args[1..], &["--force-exclude", "--no-cache", "--quiet"]),
        Some("check") => (
            Target::Lint,
            &args[1..],
            &[
                "--fix",
                "--unsafe-fixes",
                "--fix-only",
                "--exit-non-zero-on-fix",
                "--force-exclude",
                "--no-cache",
                "--quiet",
            ],
        ),
        _ => return Recognition::Unknown,
    };
    match simple("ruff", target, &[], rest, benign) {
        Recognition::Builtin(mut recognized) => {
            if rest.contains(&"--unsafe-fixes") {
                recognized.options.push(RUFF_UNSAFE_FIXES);
            }
            Recognition::Builtin(recognized)
        }
        other => other,
    }
}

fn golangci(args: &[&str]) -> Recognition {
    let target = match args.first().copied() {
        Some("fmt") => Target::Fmt,
        Some("run") => Target::Lint,
        _ => return Recognition::Unknown,
    };
    let mut rest = &args[1..];
    let mut unknown = Vec::new();
    let mut extra = Vec::new();
    while let Some((&flag, tail)) = rest.split_first() {
        rest = tail;
        match flag {
            "--fix" => {}
            "-c" | "--config" => match rest.split_first() {
                Some((&value, tail)) => {
                    extra.extend([flag.to_string(), unquote(value).to_string()]);
                    rest = tail;
                }
                None => unknown.push(flag.to_string()),
            },
            other => unknown.push(other.to_string()),
        }
    }
    if !unknown.is_empty() {
        return not_representable("golangci-lint", unknown);
    }
    let settings = ToolSettings {
        extra_args: extra,
        ..ToolSettings::default()
    };
    configured(target, &["golangci-lint"], vec![("golangci-lint", settings)], Vec::new())
}

/// `dev_tools/go/lint_changed.sh`: runs `golangci-lint run -c .golangci.yml
/// --new-from-rev=HEAD ./...` once per Go module and GOOS touched.
fn golangci_changed(args: &[&str]) -> Recognition {
    if !args.is_empty() {
        return not_representable("lint_changed.sh", args.iter().map(|a| (*a).to_string()).collect());
    }
    let settings = ToolSettings {
        extra_args: vec!["-c".to_string(), ".golangci.yml".to_string()],
        ..ToolSettings::default()
    };
    configured(
        Target::Lint,
        &["golangci-lint"],
        vec![("golangci-lint", settings)],
        vec![
            "the wrapper passed --new-from-rev=HEAD so only new findings failed the commit; poly reports every finding in the package, so add `lint_extra_args = [\"--new-from-rev=HEAD\"]` to keep that",
            "the wrapper ran once per GOOS implied by *_windows.go / *_darwin.go / *_linux.go files; poly runs for the host GOOS only",
        ],
    )
}

/// `actionlint -ignore PATTERN ... files`: the ignore patterns are the config.
fn actionlint(args: &[&str]) -> Recognition {
    let mut extra = Vec::new();
    let mut unknown = Vec::new();
    let mut rest = args;
    while let Some((&flag, tail)) = rest.split_first() {
        rest = tail;
        match (flag, rest.split_first()) {
            ("-ignore", Some((&value, tail))) => {
                extra.extend(["-ignore".to_string(), unquote(value).to_string()]);
                rest = tail;
            }
            (other, _) => unknown.push(other.to_string()),
        }
    }
    if !unknown.is_empty() {
        return not_representable("actionlint", unknown);
    }
    let settings = ToolSettings {
        extra_args: extra,
        ..ToolSettings::default()
    };
    configured(Target::Lint, &["actionlint"], vec![("actionlint", settings)], Vec::new())
}

/// `checkstyle -c checkstyle.xml -p checkstyle.properties files`.
fn checkstyle(args: &[&str]) -> Recognition {
    let mut extra = Vec::new();
    let mut unknown = Vec::new();
    let mut rest = args;
    while let Some((&flag, tail)) = rest.split_first() {
        rest = tail;
        match (flag, rest.split_first()) {
            ("-c" | "-p", Some((&value, tail))) => {
                extra.extend([flag.to_string(), unquote(value).to_string()]);
                rest = tail;
            }
            (other, _) => unknown.push(other.to_string()),
        }
    }
    if !unknown.is_empty() {
        return not_representable("checkstyle", unknown);
    }
    let settings = ToolSettings {
        extra_args: extra,
        ..ToolSettings::default()
    };
    configured(
        Target::Lint,
        &["checkstyle"],
        vec![("checkstyle", settings)],
        vec!["the wrapper selected the JDK named in .java-version; poly uses `bin`, JAVA_HOME or the java on PATH"],
    )
}

/// `run_palantir_format.sh --replace files`: armis's two-pass pipeline.
fn palantir(args: &[&str]) -> Recognition {
    let unknown: Vec<String> = args
        .iter()
        .filter(|a| **a != "--replace")
        .map(|a| (*a).to_string())
        .collect();
    if !unknown.is_empty() {
        return not_representable("palantir-java-format", unknown);
    }
    let settings = ToolSettings {
        command: Some("aosp".to_string()),
        ..ToolSettings::default()
    };
    configured(
        Target::Fmt,
        &["palantir-java-format"],
        vec![("palantir-java-format", settings)],
        Vec::new(),
    )
}

/// `pyproject-fmt --keep-full-version files`: every flag is formatting policy.
fn pyproject_fmt(args: &[&str]) -> Recognition {
    let settings = ToolSettings {
        extra_args: args.iter().map(|a| (*a).to_string()).collect(),
        ..ToolSettings::default()
    };
    configured(Target::Fmt, &["pyproject-fmt"], vec![("pyproject-fmt", settings)], Vec::new())
}

/// `buildifier -mode=fix -lint=fix --warnings=... files`: `-lint=fix` shapes the
/// formatting role, `--warnings` every role, and `-mode=check` selects linting.
fn buildifier(args: &[&str]) -> Recognition {
    let mut extra_args = Vec::new();
    let mut format_extra_args = Vec::new();
    let mut unknown = Vec::new();
    let mut checks_only = false;
    for arg in args {
        match arg.trim_start_matches('-').split_once('=') {
            Some(("mode", "fix") | ("lint", "warn")) => {}
            Some(("mode", "check" | "diff")) => checks_only = true,
            Some(("lint", "fix")) => format_extra_args.push("-lint=fix".to_string()),
            Some(("warnings", _)) => extra_args.push((*arg).to_string()),
            _ => unknown.push((*arg).to_string()),
        }
    }
    if !unknown.is_empty() {
        return not_representable("buildifier", unknown);
    }
    let settings = ToolSettings {
        extra_args,
        format_extra_args,
        ..ToolSettings::default()
    };
    let target = if checks_only { Target::Lint } else { Target::Fmt };
    configured(target, &["buildifier"], vec![("buildifier", settings)], Vec::new())
}

/// `terraform fmt` / `tofu fmt` over the staged files; `-check` only reports.
fn terraform(program: &'static str, catalog_tools: &'static [&'static str], args: &[&str]) -> Recognition {
    if args.first().copied() != Some("fmt") {
        return Recognition::Unknown;
    }
    simple(
        program,
        Target::Fmt,
        catalog_tools,
        &args[1..],
        &["-check", "-diff", "-list=false", "-write=true", "-write=false", "-no-color"],
    )
}

/// `dev_tools/helm/check_charts.sh files`: `helm lint` and `helm template` per
/// chart that contains a changed file.
fn helm_charts(args: &[&str]) -> Recognition {
    if args.iter().any(|a| *a != "--annotations") {
        return not_representable("check_charts.sh", args.iter().map(|a| (*a).to_string()).collect());
    }
    configured(
        Target::Lint,
        &["helm", "helm-template"],
        Vec::new(),
        vec![
            "the wrapper built chart dependencies (`helm dependency build`, with retries) and chose a values file per chart from dev_tools/helm/charts.py; poly runs helm as it finds the chart, so run the dependency build first and pass `-f` through `extra_args` where a chart needs values",
        ],
    )
}

fn buf(args: &[&str]) -> Recognition {
    let rest: Vec<&str> = args
        .iter()
        .skip(1)
        .copied()
        .filter(|a| !a.starts_with("$(printf"))
        .collect();
    match args.first().copied() {
        Some("format") => simple("buf", Target::Fmt, &["buf"], &rest, &["-w", "--write"]),
        Some("lint") => simple("buf", Target::Lint, &["buf"], &rest, &[]),
        _ => Recognition::Unknown,
    }
}

fn groovy(args: &[&str]) -> Recognition {
    let target = if args.contains(&"--format") {
        Target::Fmt
    } else {
        Target::Lint
    };
    let unknown: Vec<String> = args
        .iter()
        .filter(|a| !matches!(**a, "--no-insight" | "--fix" | "--format") && !a.starts_with("--failon="))
        .map(|a| (*a).to_string())
        .collect();
    if unknown.is_empty() {
        builtin(target, &["npm-groovy-lint"], Vec::new())
    } else {
        Recognition::FlagsNotRepresentable {
            tool: "npm-groovy-lint",
            flags: unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(run: &str) -> Option<(Target, Vec<&'static str>)> {
        match recognize(run) {
            Recognition::Builtin(r) => Some((r.target, r.catalog_tools.to_vec())),
            _ => None,
        }
    }

    #[test]
    fn tokenizer_keeps_quotes_and_command_substitution_whole() {
        let tokens = tokenize("buf format -w $(printf -- '--path %s ' {staged_files}) 'a b'");
        assert_eq!(
            tokens,
            [
                "buf",
                "format",
                "-w",
                "$(printf -- '--path %s ' {staged_files})",
                "'a b'"
            ]
        );
    }

    #[test]
    fn ruff_subcommands_map_to_fmt_and_lint() {
        assert_eq!(target("ruff format {staged_files}"), Some((Target::Fmt, vec![])));
        assert_eq!(
            target("ruff check --fix --unsafe-fixes {staged_files}"),
            Some((Target::Lint, vec![]))
        );
    }

    #[test]
    fn unsafe_fixes_maps_onto_the_ruff_engine_option() {
        let Recognition::Builtin(r) = recognize("ruff check --fix --unsafe-fixes {staged_files}") else {
            panic!("ruff check must map to a builtin");
        };
        assert_eq!(r.options, [RUFF_UNSAFE_FIXES]);
        assert!(r.fixes);
    }

    fn fixes(run: &str) -> bool {
        match recognize(run) {
            Recognition::Builtin(r) => r.fixes,
            other => panic!("{run} must map to a builtin, got {other:?}"),
        }
    }

    #[test]
    fn rewriting_invocations_are_fix_mode_and_reporting_ones_are_not() {
        assert!(fixes("ruff format {staged_files}"));
        assert!(fixes("ruff check --fix {staged_files}"));
        assert!(!fixes("ruff check {staged_files}"));
        assert!(fixes("oxfmt --write {staged_files}"));
        assert!(fixes("rubyfmt --in-place {staged_files}"));
        assert!(fixes("golangci-lint fmt -c .golangci.yml {staged_files}"));
        assert!(!fixes("golangci-lint run {staged_files}"));
        assert!(!fixes("actionlint {staged_files}"));
        assert!(fixes("npm-groovy-lint --format {staged_files}"));
    }

    #[test]
    fn in_place_formatters_are_fix_mode_and_their_check_modes_are_not() {
        assert!(fixes("bash dev_tools/checkstyle/run_palantir_format.sh --replace {staged_files}"));
        assert!(!fixes("bash dev_tools/checkstyle/run_checkstyle.sh -c checkstyle.xml {staged_files}"));
        assert!(fixes("pyproject-fmt --keep-full-version {staged_files}"));
        assert!(!fixes("pyproject-fmt --check {staged_files}"));
        assert!(fixes("buildifier -mode=fix -lint=fix {staged_files}"));
        assert!(fixes("buildifier {staged_files}"));
        assert!(!fixes("buildifier -mode=check {staged_files}"));
        assert!(fixes("terraform fmt {staged_files}"));
        assert!(!fixes("terraform fmt -check {staged_files}"));
        assert!(fixes("tofu fmt {staged_files}"));
    }

    #[test]
    fn buildifier_flags_split_between_the_formatting_and_every_role() {
        let settings = settings_of(
            "buildifier -mode=fix -lint=fix --warnings=-native-build {staged_files}",
            "buildifier",
        );
        assert_eq!(settings.format_extra_args, ["-lint=fix"]);
        assert_eq!(settings.extra_args, ["--warnings=-native-build"]);
        assert_eq!(
            target("buildifier -mode=check {staged_files}"),
            Some((Target::Lint, vec!["buildifier"]))
        );
        assert_eq!(
            target("buildifier -mode=fix {staged_files}"),
            Some((Target::Fmt, vec!["buildifier"]))
        );
        assert!(matches!(
            recognize("buildifier -r {staged_files}"),
            Recognition::FlagsNotRepresentable { tool: "buildifier", .. }
        ));
    }

    #[test]
    fn catalog_backed_tools_name_their_catalog_entry() {
        assert_eq!(
            target("golangci-lint fmt -c .golangci.yml {staged_files}"),
            Some((Target::Fmt, vec!["golangci-lint"]))
        );
        assert_eq!(
            target("buf format -w $(printf -- '--path %s ' {staged_files})"),
            Some((Target::Fmt, vec!["buf"]))
        );
        assert_eq!(
            target("actionlint {staged_files}"),
            Some((Target::Lint, vec!["actionlint"]))
        );
    }

    #[test]
    fn behavior_changing_flags_keep_the_command_a_job() {
        assert!(matches!(
            recognize("actionlint -shellcheck= {staged_files}"),
            Recognition::FlagsNotRepresentable { tool: "actionlint", .. }
        ));
        assert!(matches!(
            recognize("checkstyle -x foo {staged_files}"),
            Recognition::FlagsNotRepresentable { tool: "checkstyle", .. }
        ));
    }

    fn settings_of(run: &str, tool: &str) -> super::ToolSettings {
        let Recognition::Builtin(recognized) = recognize(run) else {
            panic!("`{run}` must map to a builtin");
        };
        recognized
            .settings
            .into_iter()
            .find(|(name, _)| *name == tool)
            .unwrap_or_else(|| panic!("no settings for {tool}"))
            .1
    }

    #[test]
    fn actionlint_ignores_become_extra_args() {
        let settings = settings_of("actionlint -ignore 'SC2129' -ignore 'SC2016' {staged_files}", "actionlint");
        assert_eq!(settings.extra_args, ["-ignore", "SC2129", "-ignore", "SC2016"]);
    }

    #[test]
    fn golangci_config_flag_is_kept_as_an_extra_arg() {
        assert_eq!(
            settings_of("golangci-lint run -c custom.yml {staged_files}", "golangci-lint").extra_args,
            ["-c", "custom.yml"]
        );
    }

    #[test]
    fn launcher_scripts_resolve_to_the_tool_they_start() {
        assert_eq!(
            target("bash dev_tools/checkstyle/run_checkstyle.sh -c checkstyle.xml -p checkstyle.properties {staged_files}"),
            Some((Target::Lint, vec!["checkstyle"]))
        );
        assert_eq!(
            settings_of(
                "bash dev_tools/checkstyle/run_checkstyle.sh -c checkstyle.xml -p checkstyle.properties {staged_files}",
                "checkstyle"
            )
            .extra_args,
            ["-c", "checkstyle.xml", "-p", "checkstyle.properties"]
        );
        let palantir = settings_of(
            "bash dev_tools/checkstyle/run_palantir_format.sh --replace {staged_files}",
            "palantir-java-format",
        );
        assert_eq!(palantir.command.as_deref(), Some("aosp"));
        assert_eq!(
            target("bash dev_tools/helm/check_charts.sh {staged_files}"),
            Some((Target::Lint, vec!["helm", "helm-template"]))
        );
        assert_eq!(
            target("bash dev_tools/go/lint_changed.sh {staged_files}"),
            Some((Target::Lint, vec!["golangci-lint"]))
        );
        assert_eq!(
            target("dev_tools/groovy/run_groovy_lint.sh --no-insight --format {staged_files}"),
            Some((Target::Fmt, vec!["npm-groovy-lint"]))
        );
    }

    #[test]
    fn an_unrelated_script_is_not_mistaken_for_a_launcher() {
        assert_eq!(recognize("bash scripts/other.sh {staged_files}"), Recognition::Unknown);
    }

    #[test]
    fn pyproject_fmt_flags_and_buf_lint_map_to_catalog_tools() {
        assert_eq!(
            settings_of("pyproject-fmt --keep-full-version {staged_files}", "pyproject-fmt").extra_args,
            ["--keep-full-version"]
        );
        assert_eq!(
            target("buf lint $(printf -- '--path %s ' {staged_files})"),
            Some((Target::Lint, vec!["buf"]))
        );
    }

    #[test]
    fn wrapper_behaviour_poly_lacks_is_reported_as_an_approximation() {
        let Recognition::Builtin(recognized) = recognize("bash dev_tools/go/lint_changed.sh {staged_files}") else {
            panic!("lint_changed.sh must map to a builtin");
        };
        assert!(recognized.approximations.iter().any(|a| a.contains("--new-from-rev=HEAD")));
    }

    #[test]
    fn commands_without_a_file_list_are_never_builtins() {
        assert_eq!(recognize("ruff check ."), Recognition::Unknown);
    }

    #[test]
    fn tools_without_a_counterpart_give_a_reason() {
        assert_eq!(recognize("some-unknown-tool {staged_files}"), Recognition::Unknown);
    }

    #[test]
    fn compound_commands_are_not_recognized() {
        assert_eq!(
            recognize("ruff format {staged_files} && echo done"),
            Recognition::Unknown
        );
    }

    #[test]
    fn placeholder_scan_ignores_shell_expansions() {
        assert_eq!(placeholders("echo ${HOME} {staged_files} {0}"), ["staged_files", "0"]);
        assert_eq!(
            unsupported_placeholders("a {staged_files} {0} {push_files} {git_args} {2}"),
            ["push_files"]
        );
    }

    #[test]
    fn git_argument_placeholders_are_recognised() {
        assert!(uses_git_args("{0}"));
        assert!(uses_git_args("{2}"));
        assert!(uses_git_args("{git_args}"));
        assert!(!uses_git_args("{staged_files}"));
    }
}
