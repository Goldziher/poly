//! Reading a hook `run` command line: shell tokenizing, file-placeholder
//! detection, and recognition of the tools poly can run itself.
//!
//! Recognition is deliberately strict. A command maps to a poly builtin only
//! when every flag on it is either known to be irrelevant to poly's own
//! invocation or equivalent to what poly does by default; anything else stays a
//! command job so its flags keep their effect.

/// Placeholders poly substitutes in a `run` line.
const SUPPORTED_PLACEHOLDERS: &[&str] = &["staged_files", "all_files"];

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
    /// Flags that were dropped because poly's own invocation has no equivalent.
    pub dropped_flags: Vec<String>,
}

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
        .filter(|name| !SUPPORTED_PLACEHOLDERS.contains(&name.as_str()))
        .collect();
    names.dedup();
    names
}

/// Whether `token` passes the staged file list.
pub(super) fn is_file_list(token: &str) -> bool {
    SUPPORTED_PLACEHOLDERS
        .iter()
        .any(|name| token.contains(&format!("{{{name}}}")))
}

/// Why a tool has no poly counterpart, keyed by a substring of the command.
const NO_EQUIVALENT: &[(&str, &str)] = &[
    (
        "pyproject-fmt",
        "poly formats TOML with taplo but has no pyproject-fmt layout engine",
    ),
    ("checkstyle", "poly has no checkstyle backend"),
    ("palantir", "poly has no palantir-java-format backend"),
    (
        "buf lint",
        "the catalog's buf entry is a formatter only; there is no buf lint",
    ),
    ("helm", "poly has no helm chart checker"),
];

/// Recognize the tool a `run` line invokes.
pub(super) fn recognize(run: &str) -> Recognition {
    let tokens = tokenize(run);
    if tokens
        .iter()
        .any(|t| matches!(t.as_str(), "&&" | "||" | ";" | "|" | ">" | ">>"))
    {
        return Recognition::Unknown;
    }
    let Some(program) = tokens.first().map(String::as_str) else {
        return Recognition::Unknown;
    };
    if !tokens.iter().skip(1).any(|t| is_file_list(t)) {
        return no_equivalent(run).map_or(Recognition::Unknown, Recognition::NoEquivalent);
    }
    let args: Vec<&str> = tokens[1..]
        .iter()
        .map(String::as_str)
        .filter(|t| !is_file_list(t))
        .collect();
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
        "actionlint" => simple("actionlint", Target::Lint, &["actionlint"], &args, &[]),
        "golangci-lint" => golangci(&args),
        "buf" => buf(&args),
        "npm-groovy-lint" => groovy(&args),
        _ => Recognition::Unknown,
    };
    match recognition {
        Recognition::Unknown => no_equivalent(run).map_or(Recognition::Unknown, Recognition::NoEquivalent),
        other => other,
    }
}

/// The documented reason poly has no counterpart for `run`, if known.
pub(super) fn no_equivalent(run: &str) -> Option<&'static str> {
    NO_EQUIVALENT
        .iter()
        .find(|(needle, _)| run.contains(needle))
        .map(|(_, reason)| *reason)
}

fn builtin(target: Target, catalog_tools: &'static [&'static str], dropped_flags: Vec<String>) -> Recognition {
    Recognition::Builtin(Recognized {
        target,
        catalog_tools,
        dropped_flags,
    })
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
            recognized.dropped_flags = rest
                .iter()
                .filter(|flag| **flag == "--unsafe-fixes")
                .map(|flag| (*flag).to_string())
                .collect();
            Recognition::Builtin(recognized)
        }
        other => other,
    }
}

const GOLANGCI_CONFIGS: &[&str] = &[".golangci.yml", ".golangci.yaml", ".golangci.toml", ".golangci.json"];

fn golangci(args: &[&str]) -> Recognition {
    let target = match args.first().copied() {
        Some("fmt") => Target::Fmt,
        Some("run") => Target::Lint,
        _ => return Recognition::Unknown,
    };
    let mut rest = &args[1..];
    let mut unknown = Vec::new();
    while let Some((&flag, tail)) = rest.split_first() {
        rest = tail;
        match flag {
            "--fix" => {}
            "-c" | "--config" => match rest.split_first() {
                Some((&value, tail)) if GOLANGCI_CONFIGS.contains(&unquote(value)) => rest = tail,
                Some((&value, tail)) => {
                    unknown.push(format!("{flag} {value}"));
                    rest = tail;
                }
                None => unknown.push(flag.to_string()),
            },
            other => unknown.push(other.to_string()),
        }
    }
    if unknown.is_empty() {
        builtin(target, &["golangci-lint"], Vec::new())
    } else {
        Recognition::FlagsNotRepresentable {
            tool: "golangci-lint",
            flags: unknown,
        }
    }
}

fn buf(args: &[&str]) -> Recognition {
    match args.first().copied() {
        Some("format") => {
            let rest: Vec<&str> = args[1..]
                .iter()
                .copied()
                .filter(|a| !a.starts_with("$(printf"))
                .collect();
            simple("buf", Target::Fmt, &["buf"], &rest, &["-w", "--write"])
        }
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
    fn unsafe_fixes_is_reported_as_dropped() {
        let Recognition::Builtin(r) = recognize("ruff check --fix --unsafe-fixes {staged_files}") else {
            panic!("ruff check must map to a builtin");
        };
        assert_eq!(r.dropped_flags, ["--unsafe-fixes"]);
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
            recognize("actionlint -ignore 'SC2129' {staged_files}"),
            Recognition::FlagsNotRepresentable { tool: "actionlint", .. }
        ));
        assert!(matches!(
            recognize("golangci-lint run -c custom.yml {staged_files}"),
            Recognition::FlagsNotRepresentable { .. }
        ));
    }

    #[test]
    fn commands_without_a_file_list_are_never_builtins() {
        assert_eq!(recognize("ruff check ."), Recognition::Unknown);
    }

    #[test]
    fn tools_without_a_counterpart_give_a_reason() {
        assert!(matches!(
            recognize("pyproject-fmt --keep-full-version {staged_files}"),
            Recognition::NoEquivalent(_)
        ));
        assert!(matches!(
            recognize("buf lint {staged_files}"),
            Recognition::NoEquivalent(_)
        ));
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
            unsupported_placeholders("a {staged_files} {0} {push_files}"),
            ["0", "push_files"]
        );
    }
}
