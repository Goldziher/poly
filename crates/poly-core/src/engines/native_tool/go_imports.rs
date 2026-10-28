//! `goimports` import grouping on top of `gofmt` output.
//!
//! `gofmt` sorts each run of consecutive imports by path. `goimports` (what `golangci-lint fmt`
//! runs) additionally orders a run by import *group* first and separates groups with a blank
//! line: standard library, then third-party paths (first element contains a dot), then
//! `appengine`. Blank-line-separated runs are never merged, exactly as `goimports` does.

/// Regroup every parenthesised `import ( … )` block of already-`gofmt`-formatted Go source.
/// A block this cannot read with certainty (block comments, unusual specs) is left as is.
pub(crate) fn regroup_imports(source: &str) -> String {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut out = String::with_capacity(source.len() + 16);
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim_end() != "import (" {
            out.push_str(line);
            index += 1;
            continue;
        }
        let Some(close) = lines[index + 1..].iter().position(|l| l.trim_end() == ")") else {
            out.push_str(line);
            index += 1;
            continue;
        };
        let body = &lines[index + 1..index + 1 + close];
        out.push_str(line);
        match regroup_block(body) {
            Some(regrouped) => out.push_str(&regrouped),
            None => body.iter().for_each(|l| out.push_str(l)),
        }
        out.push_str(lines[index + 1 + close]);
        index += close + 2;
    }
    out
}

struct Entry<'a> {
    lines: Vec<&'a str>,
    group: u8,
}

fn regroup_block(body: &[&str]) -> Option<String> {
    let mut out = String::new();
    let mut run: Vec<&str> = Vec::new();
    for line in body {
        if line.trim().is_empty() {
            out.push_str(&regroup_run(&run)?);
            run.clear();
            out.push_str(line);
        } else {
            run.push(line);
        }
    }
    out.push_str(&regroup_run(&run)?);
    Some(out)
}

fn regroup_run(run: &[&str]) -> Option<String> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for line in run {
        pending.push(line);
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.contains("/*") {
            return None;
        }
        let group = import_group(&import_path(trimmed)?);
        entries.push(Entry {
            lines: std::mem::take(&mut pending),
            group,
        });
    }
    if !pending.is_empty() {
        return None;
    }
    entries.sort_by_key(|entry| entry.group);
    let mut out = String::new();
    let mut previous: Option<u8> = None;
    for entry in &entries {
        if previous.is_some_and(|group| group != entry.group) {
            out.push('\n');
        }
        previous = Some(entry.group);
        entry.lines.iter().for_each(|l| out.push_str(l));
    }
    Some(out)
}

fn import_path(spec: &str) -> Option<String> {
    let start = spec.find(['"', '`'])?;
    let quote = spec[start..].chars().next()?;
    let rest = &spec[start + 1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_owned())
}

fn import_group(path: &str) -> u8 {
    if path.starts_with("appengine") {
        return 2;
    }
    if path.split('/').next().is_some_and(|first| first.contains('.')) {
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::regroup_imports;

    #[test]
    fn a_run_is_split_into_standard_and_third_party_groups() {
        let src = "package a\n\nimport (\n\t\"deploy/x/utils\"\n\t\"github.com/a/b\"\n\t\"os\"\n)\n";
        assert_eq!(
            regroup_imports(src),
            "package a\n\nimport (\n\t\"deploy/x/utils\"\n\t\"os\"\n\n\t\"github.com/a/b\"\n)\n"
        );
    }

    #[test]
    fn existing_blank_line_runs_are_not_merged() {
        let src = "import (\n\t\"strconv\"\n\n\t\"deploy/x\"\n\t\"github.com/a/b\"\n)\n";
        assert_eq!(
            regroup_imports(src),
            "import (\n\t\"strconv\"\n\n\t\"deploy/x\"\n\n\t\"github.com/a/b\"\n)\n"
        );
    }

    #[test]
    fn a_canonical_block_is_untouched() {
        let src = "import (\n\t\"fmt\"\n\n\tfoo \"github.com/a/b\"\n\t_ \"github.com/c/d\" // side effect\n)\n";
        assert_eq!(regroup_imports(src), src);
    }

    #[test]
    fn doc_comments_travel_with_their_import() {
        let src = "import (\n\t// the tree\n\t\"github.com/a/b\"\n\t\"os\"\n)\n";
        assert_eq!(
            regroup_imports(src),
            "import (\n\t\"os\"\n\n\t// the tree\n\t\"github.com/a/b\"\n)\n"
        );
    }

    #[test]
    fn a_block_with_a_block_comment_is_left_alone() {
        let src = "import (\n\t\"github.com/a/b\" /* x */\n\t\"os\"\n)\n";
        assert_eq!(regroup_imports(src), src);
    }

    #[test]
    fn single_line_imports_are_untouched() {
        let src = "import \"os\"\nimport \"github.com/a/b\"\n";
        assert_eq!(regroup_imports(src), src);
    }
}
