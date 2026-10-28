//! The `requirements_txt_fixer` member: sorts a requirements file the way the
//! pre-commit-hooks `requirements-txt-fixer` does, rewriting it in place. The
//! lowered hook is `stage_fixed`, so the sorted file lands in the working tree and
//! the index like any other fixer's output.
//!
//! Requirements are ordered by their lower-cased package name; the comments above
//! a requirement travel with it, a comment block at the very top of the file stays
//! on top, trailing comments stay at the end, and a continued (`\`) requirement is
//! one unit. The `pkg-resources==0.0.0` line a broken Debian pip adds is dropped.
//! Like the original, the hook fails when it changed a file, so the commit stops
//! and the sorted file can be staged.

use std::path::{Path, PathBuf};

use super::{Violation, read_for_scan};

const CHECK: &str = "requirements-txt-fixer";

/// The value that marks a requirement as the file's top comment block.
const TOP_COMMENT: &str = "\n";

/// A bogus line a broken Debian pip writes.
const PKG_RESOURCES: &str = "pkg-resources==0.0.0\n";

#[derive(Debug, Default)]
struct Requirement {
    value: Option<String>,
    comments: Vec<String>,
}

impl Requirement {
    fn is_complete(&self) -> bool {
        self.value
            .as_deref()
            .is_some_and(|value| !value.trim_end_matches(['\r', '\n']).ends_with('\\'))
    }

    fn append_value(&mut self, line: &str) {
        match &mut self.value {
            Some(value) => value.push_str(line),
            None => self.value = Some(line.to_owned()),
        }
    }

    /// The package name the requirement sorts by.
    fn name(&self) -> String {
        let value = self.value.as_deref().unwrap_or_default();
        let lowered = value.to_lowercase();
        for egg in ["#egg=", "&egg="] {
            if value.contains(egg)
                && let Some((_, name)) = lowered.split_once(egg)
            {
                return name.to_owned();
            }
        }
        let specifier = lowered
            .find(|c: char| c == ';' || c.is_whitespace())
            .map_or(lowered.as_str(), |end| &lowered[..end]);
        let end = specifier
            .char_indices()
            .find(|(at, c)| {
                let next = specifier[at + c.len_utf8()..].chars().next();
                match c {
                    '=' | '!' | '~' => next == Some('='),
                    '>' | '<' => true,
                    _ => false,
                }
            })
            .map_or(specifier.len(), |(at, _)| at);
        specifier[..end].to_owned()
    }
}

/// `text` sorted, or `None` when it is empty or already sorted.
fn sorted(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
    if let Some(last) = lines.last_mut()
        && !last.ends_with('\n')
    {
        last.push('\n');
    }

    let mut requirements: Vec<Requirement> = Vec::new();
    for line in &lines {
        if requirements.last().is_none_or(|last| last.is_complete()) {
            requirements.push(Requirement::default());
        }
        let first = requirements.len() == 1;
        let Some(requirement) = requirements.last_mut() else {
            continue;
        };
        if first && line.trim().is_empty() {
            if requirement.comments.first().is_some_and(|comment| comment.starts_with('#')) {
                requirement.value = Some(TOP_COMMENT.to_owned());
            } else {
                requirement.comments.push(line.clone());
            }
        } else if line.trim_start().starts_with('#') || line.trim().is_empty() {
            requirement.comments.push(line.clone());
        } else {
            requirement.append_value(line);
        }
    }

    let trailing = if requirements.last().is_some_and(|last| last.value.is_none()) {
        requirements.pop().map(|last| last.comments).unwrap_or_default()
    } else {
        Vec::new()
    };
    requirements.retain(|requirement| requirement.value.as_deref() != Some(PKG_RESOURCES));
    requirements.sort_by_cached_key(|requirement| (requirement.value.as_deref() != Some(TOP_COMMENT), requirement.name()));

    let mut after = String::with_capacity(text.len() + 1);
    for requirement in requirements {
        requirement.comments.iter().for_each(|comment| after.push_str(comment));
        after.push_str(requirement.value.as_deref().unwrap_or_default());
    }
    trailing.iter().for_each(|comment| after.push_str(comment));
    (after != text).then_some(after)
}

/// Sort every requirements file in `files` that is out of order, naming each one
/// it rewrote on stderr. A file that could not be rewritten is a violation.
pub(super) fn fix_requirements(root: &Path, files: &[PathBuf]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for path in files {
        let absolute = root.join(path);
        let Some(text) = read_for_scan(&absolute).and_then(|bytes| String::from_utf8(bytes).ok()) else {
            continue;
        };
        let Some(fixed) = sorted(&text) else {
            continue;
        };
        match std::fs::write(&absolute, fixed) {
            Ok(()) => eprintln!("{CHECK}: sorted {}", path.display()),
            Err(error) => violations.push(Violation::new(
                CHECK,
                path.clone(),
                format!("requirements are not sorted and could not be rewritten: {error}"),
            )),
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(text: &str) -> String {
        sorted(text).unwrap_or_else(|| text.to_owned())
    }

    #[test]
    fn requirements_sort_by_lowercased_name() {
        assert_eq!(fixed("Zeta==1\nalpha>=2\nBeta\n"), "alpha>=2\nBeta\nZeta==1\n");
    }

    #[test]
    fn a_sorted_file_is_left_alone() {
        assert_eq!(sorted("alpha==1\nbeta==2\n"), None);
    }

    #[test]
    fn comments_travel_with_the_requirement_below_them() {
        assert_eq!(
            fixed("# for b\nb==1\n# for a\na==1\n"),
            "# for a\na==1\n# for b\nb==1\n"
        );
    }

    #[test]
    fn a_top_comment_block_stays_on_top_and_trailing_comments_stay_last() {
        assert_eq!(
            fixed("# header\n\nb==1\na==1\n# footer\n"),
            "# header\n\na==1\nb==1\n# footer\n"
        );
    }

    #[test]
    fn a_continued_requirement_is_one_unit() {
        assert_eq!(fixed("b==1\na==1 \\\n    --hash=sha256:x\n"), "a==1 \\\n    --hash=sha256:x\nb==1\n");
    }

    #[test]
    fn egg_names_and_extras_sort_by_package() {
        assert_eq!(
            fixed("git+https://h/r.git#egg=zeta\nrequests[security]>=2\n"),
            "requests[security]>=2\ngit+https://h/r.git#egg=zeta\n"
        );
    }

    #[test]
    fn the_debian_pkg_resources_line_is_dropped() {
        assert_eq!(fixed("b==1\npkg-resources==0.0.0\na==1\n"), "a==1\nb==1\n");
    }

    #[test]
    fn a_missing_final_newline_is_added() {
        assert_eq!(sorted("a==1\nb==2"), Some("a==1\nb==2\n".to_owned()));
    }

    #[test]
    fn an_empty_file_is_untouched() {
        assert_eq!(sorted(" \n\n"), None);
    }

    #[test]
    fn the_fixer_rewrites_only_the_unsorted_file_and_passes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "b==1\na==1\n").unwrap();
        std::fs::write(dir.path().join("requirements-dev.txt"), "a==1\nb==1\n").unwrap();
        let files = [PathBuf::from("requirements.txt"), PathBuf::from("requirements-dev.txt")];
        assert!(fix_requirements(dir.path(), &files).is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("requirements.txt")).unwrap(),
            "a==1\nb==1\n"
        );
    }
}
