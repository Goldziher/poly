//! Discovery- and result-filtering helpers used by the runner: exclude-glob
//! merging, `[per-file-ignores]` suppression, and generated-lock-file
//! detection. Kept out of `runner.rs` so orchestration stays one concern per
//! file (and under the module line cap).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::engine::{Diagnostic, Severity};
use crate::language::Language;

/// Compiled `[per-file-ignores]`: each path glob paired with the rule codes to
/// suppress for files it matches. Built once per run, applied as a post-lint
/// filter on the normalized `Diagnostic.code` so it is engine-agnostic.
pub(crate) struct PerFileIgnores {
    entries: Vec<(globset::GlobMatcher, Vec<String>)>,
}

impl PerFileIgnores {
    /// Compile the config map; an invalid glob — or an entry whose rule list is
    /// empty after dropping blank codes — is skipped with a warning rather than
    /// failing the run. Dropping blank codes is a safety guard: an empty rule
    /// string would make the prefix test below match every code and silently
    /// suppress all diagnostics for the glob.
    pub(crate) fn compile(map: &BTreeMap<String, Vec<String>>) -> Self {
        let entries = map
            .iter()
            .filter_map(|(glob, rules)| {
                let rules: Vec<String> = rules.iter().filter(|rule| !rule.trim().is_empty()).cloned().collect();
                if rules.is_empty() {
                    tracing::warn!(%glob, "skipping [per-file-ignores] entry: no non-empty rule codes");
                    return None;
                }
                match globset::Glob::new(glob) {
                    Ok(compiled) => Some((compiled.compile_matcher(), rules)),
                    Err(error) => {
                        tracing::warn!(%glob, %error, "skipping invalid [per-file-ignores] glob");
                        None
                    }
                }
            })
            .collect();
        Self { entries }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop diagnostics whose `code` matches a rule listed for a glob the file
    /// matches. `rel` is the file path relative to the run root, forward-slash
    /// normalized (per-file-ignore globs are repo-rooted). Each glob is evaluated
    /// once per file (not once per diagnostic).
    ///
    /// Matching is exact, or a ruff-style prefix where the boundary character is
    /// non-alphabetic — so `"F"` suppresses `F401` but not `FOO`, and
    /// `"too-many"` suppresses `too-many-methods`. This keeps a short prefix from
    /// silently swallowing an unrelated code from another engine.
    pub(crate) fn apply(&self, rel: &str, diagnostics: &mut Vec<Diagnostic>) {
        let matched: Vec<&[String]> = self
            .entries
            .iter()
            .filter(|(matcher, _)| matcher.is_match(rel))
            .map(|(_, rules)| rules.as_slice())
            .collect();
        if matched.is_empty() {
            return;
        }
        diagnostics.retain(|diagnostic| {
            let Some(code) = diagnostic.code.as_deref() else {
                return true;
            };
            !matched
                .iter()
                .any(|rules| rules.iter().any(|rule| code_matches_rule(code, rule)))
        });
    }
}

/// Per-rule severity remap built from the `[lint.<lang>.<tool>.rules.<code>]
/// level` entries. Applied as a post-lint pass on the normalized
/// `Diagnostic.code` so it works uniformly for every engine — including those
/// with no native severity config. Mirrors [`PerFileIgnores`]: compile once per
/// engine plan, then apply per file.
pub(crate) struct SeverityRemap {
    /// `(rule_code, level)` pairs in config order; the first whose code matches a
    /// diagnostic wins. Blank rule codes are dropped on construction so a stray
    /// empty string cannot prefix-match (and thus remap) every code.
    entries: Vec<(String, Severity)>,
}

impl SeverityRemap {
    /// Build from the per-rule `level` pairs. Entries with a blank rule code are
    /// dropped: an empty rule would prefix-match every code and silently remap
    /// all diagnostics.
    pub(crate) fn new(entries: Vec<(String, Severity)>) -> Self {
        let entries = entries
            .into_iter()
            .filter(|(rule, _)| !rule.trim().is_empty())
            .collect();
        Self { entries }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Set each diagnostic's severity to the level of the FIRST rule whose code
    /// matches its `code` (via [`code_matches_rule`], the same exact-or-prefix
    /// semantics as per-file-ignores). Diagnostics without a code, or with no
    /// matching rule, are left untouched.
    pub(crate) fn apply(&self, diagnostics: &mut [Diagnostic]) {
        if self.entries.is_empty() {
            return;
        }
        for diagnostic in diagnostics.iter_mut() {
            let Some(code) = diagnostic.code.as_deref() else {
                continue;
            };
            let level = self
                .entries
                .iter()
                .find(|(rule, _)| code_matches_rule(code, rule))
                .map(|(_, level)| *level);
            if let Some(level) = level {
                diagnostic.severity = level;
            }
        }
    }
}

/// Whether `code` is suppressed by a per-file-ignore `rule`: exact match, or a
/// prefix match where the next character is not alphabetic (ruff-style code
/// families like `F` → `F401`, while `E` does not swallow `ERR_X`).
fn code_matches_rule(code: &str, rule: &str) -> bool {
    if code == rule {
        return true;
    }
    match code.strip_prefix(rule) {
        Some(rest) => rest.chars().next().is_none_or(|c| !c.is_alphabetic()),
        None => false,
    }
}

/// File path relative to the run root, forward-slash normalized, for matching
/// repo-rooted `[per-file-ignores]` globs. Strips the first of `bases` (cwd plus
/// the explicitly passed roots) that prefixes the path, so both `poly lint .`
/// (relative paths) and `poly lint /abs/repo` (absolute paths) resolve to a
/// repo-relative path the globs can match.
pub(crate) fn relative_for_match(path: &Path, bases: &[PathBuf]) -> String {
    let mut rel = path;
    for base in bases {
        if let Ok(stripped) = path.strip_prefix(base) {
            if stripped.as_os_str().is_empty() {
                continue;
            }
            rel = stripped;
            break;
        }
    }
    let rel = rel.strip_prefix(".").unwrap_or(rel);
    let text = rel.to_string_lossy();
    if text.contains('\\') {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

/// Prefix bases for [`relative_for_match`]: the working directory (when
/// available) followed by the explicitly passed roots, so per-file-ignore globs
/// resolve against whichever one prefixes a discovered file.
pub(crate) fn match_bases(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut bases = Vec::with_capacity(paths.len() + 1);
    match std::env::current_dir() {
        Ok(cwd) => bases.push(cwd),
        Err(error) => {
            tracing::warn!(%error, "cannot determine working directory; \
                 per-file-ignores fall back to matching against the passed paths");
        }
    }
    bases.extend(paths.iter().cloned());
    bases
}

/// Generated lock files, by exact name, that `poly fmt` never rewrites on a
/// directory walk. Any `*.lock` file is also treated as a lock file; these are
/// the ones whose names do not end in `.lock`.
const LOCKFILE_NAMES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "bun.lockb",
];

/// Whether `path` is a machine-generated lock file that must not be reformatted.
/// Matched by the `*.lock` extension (Cargo.lock, yarn.lock, poetry.lock,
/// uv.lock, composer.lock, Gemfile.lock, flake.lock, deno.lock, …) or by an
/// exact name in [`LOCKFILE_NAMES`] for the lock files that don't end in `.lock`.
pub(crate) fn is_generated_lockfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".lock") || LOCKFILE_NAMES.contains(&name)
}

/// Whether `content` carries a file-level "do not format this file" directive
/// for its `language`, in which case `poly fmt` leaves it untouched.
///
/// As the umbrella formatter, poly honors the ecosystem's established
/// whole-file skip markers so it does not fight a tool a project already opted
/// into (and does not disturb machine-generated bridge/glue files that carry
/// the marker). Currently:
///
/// - **Swift** — `// swift-format-ignore-file` (the directive `swift-format`
///   itself recognizes to skip a file entirely). Matched leniently anywhere in
///   the file; it is a distinctive marker that does not occur incidentally.
pub(crate) fn is_format_ignored(content: &str, language: &Language) -> bool {
    match language {
        Language::Swift => content.contains("swift-format-ignore-file"),
        _ => false,
    }
}

/// Markers that identify a machine-generated file, matched case-insensitively
/// against its opening lines.
///
/// Deliberately narrow: each is a phrase a generator writes to tell humans not
/// to edit the file, not merely a mention of generation.
const GENERATED_MARKERS: &[&str] = &[
    "do not edit",
    "@generated",
    "auto-generated",
    "autogenerated",
    "auto generated",
    "code generated",
];

/// How many opening lines are searched for a generated-file marker. Generators
/// put the banner at the top; scanning further would start matching prose.
const GENERATED_HEADER_LINES: usize = 5;

/// Alef's own marker-scan bound, mirrored here.
///
/// Upstream: `MARKER_SCAN_LINES` in `alef/src/core/hash.rs`. Both
/// `inject_hash_line` (the writer) and `generated_hash_line` (the reader) bound
/// their search for the generated-file banner by it, so Alef accepts its banner
/// at 0-based line 0..=9 and nowhere deeper.
///
/// This is a **mirrored upstream value, not a poly tuning knob.** The only
/// correct reason to change it is that Alef changed `MARKER_SCAN_LINES`; the
/// only correct response to Alef changing `MARKER_SCAN_LINES` is to change it
/// here. Nothing in either repo fails when the two drift — poly simply stops
/// protecting files the producer can still stamp (see the note on
/// [`HASH_STAMP_HEADER_LINES`], which this already caused once). The behaviour
/// is pinned by `should_honor_the_producer_stamp_window_exactly` below and by
/// `crates/poly-core/tests/alef_stamp_window.rs`, both written against literal
/// line numbers so that narrowing either constant cannot leave them green.
const ALEF_MARKER_SCAN_LINES: usize = 10;

/// How many lines **of [`generated_header_region`]** are searched for a
/// structured content-hash stamp. Like [`GENERATED_HEADER_LINES`] this is
/// measured from the start of that region, i.e. *after* any YAML frontmatter
/// block — not from byte 0 of the file. A frontmattered document therefore gets
/// this many lines on top of its frontmatter.
///
/// Wider than [`GENERATED_HEADER_LINES`] because this window has to match the
/// producer's, not ours. Alef — the generator that motivated this — accepts its
/// header marker anywhere in the first 10 lines and writes the `alef:hash:<hex>`
/// line directly beneath it, so a stamp legitimately lands well below line 5
/// whenever a licence preamble, SPDX block, or build-tag stanza precedes the
/// banner. Scanning only 5 left exactly those files unprotected: stamped by the
/// generator, invisible to poly, reformatted by `fmt --fix` — and the reflow then
/// reported as drift by the generator's own verify step, which is the loop the
/// skip exists to break. A consumer's vendored cgo and visitor bindings were the
/// casualties.
///
/// **This value is derived from Alef, not chosen.** [`ALEF_MARKER_SCAN_LINES`]
/// bounds where the banner may sit (index 0..=9). The `+ 1` is the injector's
/// offset: `inject_hash_line` writes the stamp on the line *below* the banner
/// and `generated_hash_line` reads it back by peeking that same line, so the
/// deepest stamp Alef can both write and read is index 10 — eleven lines of
/// window. Poly's window is the producer's window plus that one line, and
/// neither term is free to move on its own.
///
/// (An earlier revision of Alef read a bare `alef:hash:` token anywhere in
/// index 0..=9, so poly matched it at 10. Alef then moved to marker-then-peek,
/// which reaches one line deeper, and poly was silently one short until this was
/// noticed by hand — no test failed on either side. That is the failure this
/// derivation exists to make loud.)
///
/// Widening is safe *only* for the structured shape. `<project>:hash:<8+ hex>`
/// (see [`has_structured_hash_stamp`]) cannot occur in prose. The loose banners
/// and the bare-word [`CONTENT_HASH_MARKERS`] keep the narrow window: those are
/// English words that appear further down real files, and a false skip there
/// drops a hand-written file out of the format gate silently — the failure this
/// module weighs as strictly worse than formatting a generated file.
const HASH_STAMP_HEADER_LINES: usize = ALEF_MARKER_SCAN_LINES + 1;

fn generated_header_region(content: &str) -> Option<&str> {
    let mut offset = 0;
    for (index, line) in content.split_inclusive('\n').enumerate() {
        let line_content = line.trim_end_matches(['\r', '\n']);
        if index == 0 && line_content != "---" {
            return Some(content);
        }
        offset += line.len();
        if index > 0 && line_content == "---" {
            return Some(&content[offset..]);
        }
    }
    (!content.starts_with("---")).then_some(content)
}

/// Whether `content` announces itself as machine-generated.
///
/// `poly lint` still *reports* on these files — that is how a generator bug gets
/// noticed — but `--fix` leaves them alone, because rewriting generated output
/// is churn that the next generation run reverts, and worse, it can silence the
/// diagnostic that was the only evidence of the bug.
///
/// The case that motivated this: ruff's `F841` correctly fired on an unused
/// binding in a generated test, and that binding was the sole signal that 39
/// generated tests across 8 files called an API and asserted nothing. Running
/// `--fix` rewrote it to `_` and turned a loud, correct diagnostic about a real
/// upstream defect into a clean lint pass.
pub(crate) fn is_generated_source(content: &str) -> bool {
    generated_header_region(content).is_some_and(|header| {
        header.lines().take(GENERATED_HEADER_LINES).any(|line| {
            let lower = line.to_ascii_lowercase();
            GENERATED_MARKERS.iter().any(|marker| lower.contains(marker))
        }) || has_content_hash_stamp(header)
    })
}

/// Markers that mean the header carries a **content hash** of the file body.
const CONTENT_HASH_MARKERS: &[&str] = &["sourcehash", "@checksum"];
const STRUCTURED_HASH_MARKER: &str = ":hash:";

/// Hex lengths a real content hash can have: md5, sha1, sha224, sha256/blake3,
/// sha384, sha512.
///
/// Checking membership rather than a minimum is what separates a stamp from a
/// hex-looking token. A consumer measured the previous `>= 8` rule and found it
/// accepted 8, 63 and 65 hex characters — no digest is 63 or 65 characters, and
/// 8 is short enough to occur by accident — so each of those silently dropped a
/// hand-written file out of the format gate.
///
/// Deliberately *not* pinned to 64. Poly's marker is generator-agnostic
/// (`<project>:hash:<digest>`); 64 is only what one generator emits because it
/// uses blake3. Pinning to it would drop every sha1- or md5-stamped generator
/// out of the gate — the same defect aimed at a different producer.
const HASH_DIGEST_LENGTHS: &[usize] = &[32, 40, 56, 64, 96, 128];

/// What may legally follow the digest: nothing, whitespace, or a comment
/// terminator. A stamp is a whole comment line, so a token wrapped in something
/// else — markdown backticks being the case that was measured — is prose that
/// happens to mention a hash, not a stamp.
const DIGEST_TERMINATORS: &[&str] = &["-->", "*/", "#}", "--%>"];

fn has_structured_hash_stamp(line: &str) -> bool {
    let header = line.trim_start_matches(|character: char| !character.is_ascii_alphanumeric());
    let Some((project, digest)) = header.split_once(STRUCTURED_HASH_MARKER) else {
        return false;
    };
    if project.is_empty()
        || !project
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return false;
    }

    let digest = digest.trim_start();
    let hex_length = digest.chars().take_while(char::is_ascii_hexdigit).count();
    if !HASH_DIGEST_LENGTHS.contains(&hex_length) {
        return false;
    }

    // Whatever follows the digest must close the line or close the comment.
    // Anything else means the token was embedded in something — prose quoting a
    // hash rather than a header stamping one.
    let trailing = digest[hex_length..].trim();
    trailing.is_empty() || DIGEST_TERMINATORS.contains(&trailing)
}

/// Whether `header` carries a content-hash stamp over the body.
///
/// The two signal classes get different windows on purpose — see
/// [`HASH_STAMP_HEADER_LINES`]. The structured `<project>:hash:<hex>` shape is
/// searched to the producer's depth; the bare-word markers stay inside the
/// narrow banner window, because a lone `sourcehash` is an identifier that
/// occurs in ordinary code and a false positive here silently removes a
/// hand-written file from the format gate.
fn has_content_hash_stamp(header: &str) -> bool {
    header
        .lines()
        .take(HASH_STAMP_HEADER_LINES)
        .enumerate()
        .any(|(index, line)| {
            let lower = line.to_ascii_lowercase();
            has_structured_hash_stamp(&lower)
                || (index < GENERATED_HEADER_LINES && CONTENT_HASH_MARKERS.iter().any(|marker| lower.contains(marker)))
        })
}

/// Whether the header stamps a content hash over the body.
///
/// This is a strictly narrower question than [`is_generated_source`], and the
/// two drive different decisions:
///
/// - **`lint --fix`** withholds on *any* generated marker. Withholding a fix
///   still reports the diagnostics, so nothing leaves the gate.
/// - **`fmt`** skips only on a content hash, because skipping there removes the
///   file from the format gate entirely.
///
/// The distinction exists because generalising it caused real harm. Reformatting
/// a hash-stamped body invalidates the hash, so a verify step reports drift on a
/// file no human touched and the remedy is a regen that discards the formatting
/// — that is a loop, and skipping breaks it. But a bare "DO NOT EDIT" banner
/// makes no such promise, and a generator that stamps a *hand-written* file with
/// one would silently drop it out of lint and format enforcement — reported by a
/// consumer whose most user-facing code left the gate without anything failing.
/// Formatting a banner-only file is harmless; silently not checking it is not.
pub(crate) fn is_hash_stamped_source(content: &str) -> bool {
    generated_header_region(content).is_some_and(has_content_hash_stamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Severity;

    #[test]
    fn swift_format_ignore_file_directive_skips_formatting() {
        let ignored = "// swift-format-ignore-file\nimport Foundation\nstruct A{let x:Int}\n";
        assert!(is_format_ignored(ignored, &Language::Swift));
        let normal = "import Foundation\nstruct A{let x:Int}\n";
        assert!(!is_format_ignored(normal, &Language::Swift));
        assert!(!is_format_ignored(ignored, &Language::Python));
    }

    fn diag(code: Option<&str>) -> Diagnostic {
        Diagnostic {
            engine: "test".to_string(),
            code: code.map(str::to_owned),
            severity: Severity::Warning,
            title: "x".to_string(),
            description: None,
            span: None,
            url: None,
            fix: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn per_file_ignores_suppress_matching_codes() {
        let mut map = BTreeMap::new();
        map.insert(
            "tests/**".to_string(),
            vec!["F401".to_string(), "too-many-methods".to_string()],
        );
        let ignores = PerFileIgnores::compile(&map);

        let mut diags = vec![
            diag(Some("F401")),
            diag(Some("too-many-methods")),
            diag(Some("E501")),
            diag(None),
        ];
        ignores.apply("tests/unit/foo.py", &mut diags);
        let codes: Vec<_> = diags.iter().map(|d| d.code.clone()).collect();
        assert_eq!(codes, vec![Some("E501".to_string()), None]);

        let mut diags = vec![diag(Some("F401"))];
        ignores.apply("src/foo.py", &mut diags);
        assert_eq!(diags.len(), 1, "non-matching path is untouched");
    }

    #[test]
    fn prefix_match_respects_a_non_alphabetic_boundary() {
        assert!(code_matches_rule("E501", "E"), "E501 is in the E family");
        assert!(code_matches_rule("too-many-methods", "too-many"));
        assert!(code_matches_rule("F401", "F401"), "exact match");
        assert!(!code_matches_rule("ERR_X", "E"), "alphabetic boundary blocks");
        assert!(!code_matches_rule("FOO", "F"), "alphabetic boundary blocks");
    }

    #[test]
    fn empty_rule_string_is_dropped_not_a_wildcard() {
        let mut map = BTreeMap::new();
        map.insert("**".to_string(), vec![String::new(), "  ".to_string()]);
        let ignores = PerFileIgnores::compile(&map);
        assert!(ignores.is_empty(), "an entry with only blank codes is skipped entirely");
        let mut diags = vec![diag(Some("F401")), diag(None)];
        ignores.apply("anything.py", &mut diags);
        assert_eq!(diags.len(), 2, "nothing is suppressed");
    }

    #[test]
    fn relative_for_match_strips_cwd_and_passed_roots() {
        let cwd = PathBuf::from("/work/repo");
        assert_eq!(
            relative_for_match(Path::new("/work/repo/tests/a.py"), std::slice::from_ref(&cwd)),
            "tests/a.py"
        );
        let bases = vec![cwd, PathBuf::from("/other/root")];
        assert_eq!(
            relative_for_match(Path::new("/other/root/tests/a.py"), &bases),
            "tests/a.py"
        );
        assert_eq!(
            relative_for_match(Path::new("./tests/a.py"), &[PathBuf::from("/x")]),
            "tests/a.py"
        );
        let file = PathBuf::from("tests/a.py");
        assert_eq!(
            relative_for_match(Path::new("tests/a.py"), &[PathBuf::from("/cwd"), file]),
            "tests/a.py"
        );
    }

    #[test]
    fn severity_remap_sets_first_matching_rule_level() {
        let remap = SeverityRemap::new(vec![("F401".to_string(), Severity::Warning)]);
        assert!(!remap.is_empty());

        let mut diags = vec![diag(Some("F401")), diag(Some("E501")), diag(None)];
        diags[0].severity = Severity::Error;
        remap.apply(&mut diags);

        assert_eq!(diags[0].severity, Severity::Warning, "F401 is remapped");
        assert_eq!(
            diags[1].severity,
            Severity::Warning,
            "a non-matching code keeps its severity"
        );
        assert_eq!(diags[2].severity, Severity::Warning, "a code-less diag is untouched");
    }

    #[test]
    fn severity_remap_honors_prefix_family_and_first_match_wins() {
        let remap = SeverityRemap::new(vec![
            ("F".to_string(), Severity::Hint),
            ("F401".to_string(), Severity::Error),
        ]);
        let mut diags = vec![diag(Some("F401"))];
        diags[0].severity = Severity::Warning;
        remap.apply(&mut diags);
        assert_eq!(
            diags[0].severity,
            Severity::Hint,
            "the first matching rule wins (family prefix before the exact code)"
        );
        let mut other = vec![diag(Some("FOO"))];
        other[0].severity = Severity::Warning;
        remap.apply(&mut other);
        assert_eq!(other[0].severity, Severity::Warning, "FOO is not in the F family");
    }

    #[test]
    fn severity_remap_empty_is_a_noop() {
        let remap = SeverityRemap::new(Vec::new());
        assert!(remap.is_empty());
        let mut diags = vec![diag(Some("F401"))];
        diags[0].severity = Severity::Error;
        remap.apply(&mut diags);
        assert_eq!(diags[0].severity, Severity::Error, "empty remap changes nothing");
    }

    #[test]
    fn should_only_recognize_structured_hash_stamps() {
        for ordinary_source in ["use a::hash::b;\n", "a:hash:b\n"] {
            assert!(!is_generated_source(ordinary_source), "source: {ordinary_source:?}");
            assert!(!is_hash_stamped_source(ordinary_source), "source: {ordinary_source:?}");
        }

        for generated_header in [
            "# alef:hash: a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n",
            "// project_2:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n",
        ] {
            assert!(is_generated_source(generated_header), "header: {generated_header:?}");
            assert!(is_hash_stamped_source(generated_header), "header: {generated_header:?}");
        }
    }

    /// The exact header Alef writes: `hash::header(CommentStyle::DoubleSlash)`
    /// followed by `hash::inject_hash_line`, which stamps the line *below* the
    /// banner. Not a generic invented marker.
    fn alef_stamped_source(preamble_lines: usize) -> String {
        let mut source = "// Copyright 2026 xberg-io\n".repeat(preamble_lines);
        source.push_str("// This file is auto-generated by alef — DO NOT EDIT.\n");
        source.push_str("// alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n");
        source.push_str("// To regenerate: alef generate\n\npackage bridge\n");
        source
    }

    /// Alef accepts its banner anywhere in the first 10 lines and stamps
    /// directly beneath it, so a licence preamble pushes the stamp past the
    /// banner window. Scanning only 5 lines left those files stamped by the
    /// generator and unprotected in poly — `fmt --fix` reflowed a consumer's
    /// vendored cgo and visitor bindings.
    #[test]
    fn should_skip_stamped_source_when_a_preamble_precedes_the_alef_banner() {
        let source = alef_stamped_source(4);
        assert_eq!(
            source.lines().position(|line| line.contains("alef:hash:")),
            Some(5),
            "fixture must put the stamp past GENERATED_HEADER_LINES, or it proves nothing"
        );
        assert!(is_hash_stamped_source(&source), "fmt would reformat a stamped file");
        assert!(is_generated_source(&source), "lint --fix would rewrite a stamped file");
    }

    /// Both edges of the producer's reach, written as literals on purpose.
    ///
    /// Alef bounds its *marker* scan at index 0..=9 and reads the stamp on the
    /// line immediately after the marker, so the deepest stamp it can both write
    /// and read back is index **10**. Deriving these fixtures from
    /// [`HASH_STAMP_HEADER_LINES`] or [`ALEF_MARKER_SCAN_LINES`] would make them
    /// move with the constant, so narrowing it would leave them green — the
    /// literals are what pin poly's window to the producer's.
    ///
    /// **Do not refactor `9` / `10` / `11` here into expressions over the
    /// constants.** It reads like duplication and is the entire guard: a derived
    /// fixture tests that the code agrees with itself, which it always does.
    /// Same rule in `crates/poly-core/tests/alef_stamp_window.rs`.
    #[test]
    fn should_honor_the_producer_stamp_window_exactly() {
        let deepest_the_producer_can_reach = alef_stamped_source(9);
        assert_eq!(
            deepest_the_producer_can_reach
                .lines()
                .position(|line| line.contains("alef:hash:")),
            Some(10),
            "fixture must place the stamp at the producer's deepest reachable line"
        );
        assert!(
            is_hash_stamped_source(&deepest_the_producer_can_reach),
            "a stamp on the deepest line the producer can reach must be honored"
        );

        let beyond = alef_stamped_source(10);
        assert_eq!(beyond.lines().position(|line| line.contains("alef:hash:")), Some(11));
        assert!(
            !is_hash_stamped_source(&beyond),
            "a stamp deeper than the producer's own reader accepts must not skip"
        );
    }

    /// The wider window admits the structured shape only. Widening the loose
    /// banners would drop hand-written files out of the format gate silently,
    /// which this module treats as strictly worse than formatting a generated
    /// one.
    #[test]
    fn should_not_widen_the_window_for_prose_banners_or_bare_word_markers() {
        let banner_below_window = format!("{}// DO NOT EDIT.\nfn main() {{}}\n", "// filler\n".repeat(6));
        assert!(!is_generated_source(&banner_below_window));
        assert!(!is_hash_stamped_source(&banner_below_window));

        let bare_word_below_window = format!("{}let sourcehash = compute();\n", "// filler\n".repeat(6));
        assert!(
            !is_hash_stamped_source(&bare_word_below_window),
            "a `sourcehash` identifier below the banner window must not remove the file from the gate"
        );

        let bare_word_inside_window = "// sourcehash: abc\nfn main() {}\n";
        assert!(
            is_hash_stamped_source(bare_word_inside_window),
            "the narrow-window behavior of the bare-word markers is unchanged"
        );
    }

    /// A hex-looking token is not a digest. The previous rule accepted any run
    /// of 8 or more hex characters, which a consumer measured as accepting 8, 63
    /// and 65 — no digest is 63 or 65 characters long, and 8 is short enough to
    /// occur by accident. Each accepted length silently dropped a hand-written
    /// file out of the format gate.
    #[test]
    fn should_reject_digests_that_are_not_a_real_hash_length() {
        for length in [1usize, 7, 8, 16, 31, 63, 65, 127] {
            let source = format!("# alef:hash:{}\nbody\n", "a".repeat(length));
            assert!(
                !is_hash_stamped_source(&source),
                "{length} hex characters is not a digest length and must not skip"
            );
        }

        // Every real digest length a generator can emit still counts. Pinning
        // this to 64 would drop sha1- or md5-stamped generators out of the gate.
        for length in HASH_DIGEST_LENGTHS {
            let source = format!("# alef:hash:{}\nbody\n", "a".repeat(*length));
            assert!(
                is_hash_stamped_source(&source),
                "{length} hex characters is a real digest length and must skip"
            );
        }
    }

    /// A stamp is a whole comment line. A token wrapped in markdown backticks is
    /// prose quoting a hash — the leading backtick is stripped as punctuation, so
    /// only the *trailing* character distinguishes the two.
    #[test]
    fn should_reject_a_digest_that_is_wrapped_rather_than_terminated() {
        let digest = "a".repeat(64);

        for wrapped in [
            format!("`alef:hash:{digest}`\nbody\n"),
            format!("(alef:hash:{digest})\nbody\n"),
            format!("\"alef:hash:{digest}\"\nbody\n"),
        ] {
            assert!(!is_hash_stamped_source(&wrapped), "wrapped token: {wrapped:?}");
        }

        // Bare, and every comment terminator a generator actually closes with.
        for stamped in [
            format!("// alef:hash:{digest}\nbody\n"),
            format!("<!-- alef:hash:{digest} -->\nbody\n"),
            format!(" * alef:hash:{digest} */\nbody\n"),
        ] {
            assert!(is_hash_stamped_source(&stamped), "real stamp: {stamped:?}");
        }
    }

    /// Token shape and header depth are independent axes. Tightening the shape
    /// must not narrow the window — the natural instinct when tightening a
    /// matcher is to tighten everything at once, and that would silently undo
    /// the generator-reach fix while every shape test still passed.
    #[test]
    fn tightening_the_digest_shape_must_not_move_the_depth_boundary() {
        let deepest = alef_stamped_source(9);
        assert!(is_hash_stamped_source(&deepest), "depth boundary narrowed");

        let beyond = alef_stamped_source(10);
        assert!(!is_hash_stamped_source(&beyond), "depth boundary widened");
    }

    /// The #10 fix must survive the wider window: the extra lines are exactly
    /// where ordinary `use` statements live.
    #[test]
    fn should_still_reject_ordinary_paths_inside_the_wider_window() {
        for ordinary in [
            format!("{}use a::hash::b;\n", "// filler\n".repeat(6)),
            format!("{}a:hash:b\n", "// filler\n".repeat(6)),
            format!(
                "{}// see example.com/alef/hash/deadbeef12345678\n",
                "// filler\n".repeat(6)
            ),
        ] {
            assert!(!is_generated_source(&ordinary), "source: {ordinary:?}");
            assert!(!is_hash_stamped_source(&ordinary), "source: {ordinary:?}");
        }
    }

    #[test]
    fn recognizes_generated_markers_after_yaml_frontmatter() {
        let source = "---\r\nname: api\r\ndescription: generated API docs\r\n---\r\n\r\n\
                      <!-- This file is auto-generated. DO NOT EDIT. -->\r\n\
                      <!-- alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef -->\r\n\
                      # Heading\r\n";

        assert!(
            is_generated_source(source),
            "generated banner below frontmatter was missed"
        );
        assert!(
            is_hash_stamped_source(source),
            "hash stamp below frontmatter was missed"
        );
    }

    #[test]
    fn unterminated_yaml_frontmatter_does_not_expand_marker_scan() {
        let source = "---\nname: api\n# alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n";
        assert!(!is_generated_source(source));
        assert!(!is_hash_stamped_source(source));
    }

    /// Both windows are measured from the end of the frontmatter block, not from
    /// byte 0 — see [`generated_header_region`]. These fixtures are the ones that
    /// tell the two models apart: the stamp sits at post-frontmatter line 5,
    /// which is raw line 8, so a refactor that dropped the frontmatter stripping
    /// and counted from byte 0 would report "outside the window" and fail here.
    ///
    /// Starlight/Astro reference pages are the live instance — frontmatter, a
    /// blank line, the banner, then the stamp — and a consumer has 21 of them.
    #[test]
    fn windows_are_measured_after_the_frontmatter_block() {
        let stamped = "---\nname: api\n---\n1\n2\n3\n4\n5\n# alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n";
        assert!(
            is_hash_stamped_source(stamped),
            "a stamp at post-frontmatter line 5 is inside the producer's reach and must be protected"
        );

        // The prose banners keep the narrow window, also measured post-frontmatter.
        let banner = "---\nname: api\n---\n1\n2\n3\n4\n5\n# DO NOT EDIT\n";
        assert!(
            !is_generated_source(banner),
            "a prose banner past the banner window must stay ordinary source"
        );
    }

    /// The wider window is still bounded. A stamp deeper than the producer's own
    /// reader accepts must not silently drop the file out of the format gate.
    #[test]
    fn structured_stamp_beyond_the_post_frontmatter_window_is_not_generated() {
        let mut source = String::from("---\nname: api\n---\n");
        for line in 1..=HASH_STAMP_HEADER_LINES {
            source.push_str(&format!("{line}\n"));
        }
        source.push_str("# alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n");
        assert!(!is_generated_source(&source));
        assert!(!is_hash_stamped_source(&source));
    }

    #[test]
    fn recognizes_generated_lock_files() {
        for name in [
            "Cargo.lock",
            "yarn.lock",
            "poetry.lock",
            "uv.lock",
            "Gemfile.lock",
            "flake.lock",
            "composer.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "npm-shrinkwrap.json",
            "bun.lockb",
        ] {
            assert!(
                is_generated_lockfile(Path::new(name)),
                "{name} should be treated as a lock file"
            );
        }
        for name in ["main.rs", "Cargo.toml", "package.json", "lockfile.txt"] {
            assert!(
                !is_generated_lockfile(Path::new(name)),
                "{name} must not be treated as a lock file"
            );
        }
    }
}
