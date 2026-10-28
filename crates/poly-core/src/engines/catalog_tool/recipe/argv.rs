//! Turning a catalog argv template into concrete command lines.
//!
//! A template mixes literal flags with placeholders (see `poly_catalog`):
//!
//! | token          | expands to                                           |
//! |----------------|------------------------------------------------------|
//! | `$FILES`       | every file of the invocation                         |
//! | `$FILES:--path`| `--path a --path b`                                  |
//! | `$DIR`         | the directory of every file, once each (`./pkg`)     |
//! | `…$DIR…`       | one invocation per directory, the token spliced in   |
//! | `$ROOT`        | `.`, the project root the tool is run from           |
//! | `$PATH`        | the first file (the single-file form)                |
//!
//! A run's items do not always fit one command line, so [`Template::chunks`]
//! splits them to stay inside [`ARGV_BUDGET`].

use std::ops::Range;

use poly_catalog::{DIR_PLACEHOLDER, FILES_PLACEHOLDER, PATH_PLACEHOLDER, ROOT_PLACEHOLDER};

/// Characters of arguments per invocation. Windows' `CreateProcess` caps a whole
/// command line at 32,767; this leaves room for the program and the fixed flags
/// and is far under every Unix `ARG_MAX`.
pub(super) const ARGV_BUDGET: usize = 30_000;

/// What a template spends its list of items on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Items {
    /// `$FILES`: the files themselves.
    Files,
    /// `$DIR`: the directory of each file.
    Dirs,
    /// Neither: a single invocation regardless of the file count.
    Nothing,
}

/// A command-line template ready to be rendered against a list of items.
#[derive(Debug, Clone)]
pub(super) struct Template {
    tokens: Vec<String>,
    items: Items,
    /// A token embeds `$DIR` (`--chdir=$DIR`), so each directory needs its own
    /// invocation.
    per_item: bool,
}

fn is_target(token: &str) -> bool {
    token == FILES_PLACEHOLDER
        || token.starts_with("$FILES:")
        || token == PATH_PLACEHOLDER
        || token == ROOT_PLACEHOLDER
        || token.contains(DIR_PLACEHOLDER)
}

impl Template {
    /// Build from the catalog `arguments`, inserting `extra` (user config flags)
    /// ahead of the first placeholder, or at the end when there is none.
    pub(super) fn new(arguments: &[String], extra: &[String]) -> Self {
        let at = arguments.iter().position(|t| is_target(t)).unwrap_or(arguments.len());
        let mut tokens = Vec::with_capacity(arguments.len() + extra.len());
        tokens.extend_from_slice(&arguments[..at]);
        tokens.extend_from_slice(extra);
        tokens.extend_from_slice(&arguments[at..]);
        let per_item = tokens
            .iter()
            .any(|t| t != DIR_PLACEHOLDER && t.contains(DIR_PLACEHOLDER));
        let items = if tokens
            .iter()
            .any(|t| t == FILES_PLACEHOLDER || t.starts_with("$FILES:"))
        {
            Items::Files
        } else if tokens.iter().any(|t| t.contains(DIR_PLACEHOLDER)) {
            Items::Dirs
        } else {
            Items::Nothing
        };
        Template {
            tokens,
            items,
            per_item,
        }
    }

    pub(super) fn items(&self) -> Items {
        self.items
    }

    /// Every token, placeholders included.
    pub(super) fn tokens(&self) -> &[String] {
        &self.tokens
    }

    /// The template as one string, for the engine's cache key.
    pub(super) fn fingerprint(&self) -> String {
        self.tokens.join("\u{1f}")
    }

    /// Split `items` into the ranges that each become one invocation.
    pub(super) fn chunks(&self, items: &[String]) -> Vec<Range<usize>> {
        if self.items == Items::Nothing {
            #[allow(clippy::single_range_in_vec_init)]
            return vec![0..0];
        }
        if self.per_item {
            return (0..items.len()).map(|i| i..i + 1).collect();
        }
        let fixed: usize = self.tokens.iter().filter(|t| !is_target(t)).map(|t| t.len() + 1).sum();
        let flag = self
            .tokens
            .iter()
            .find_map(|t| t.strip_prefix("$FILES:"))
            .map_or(0, |flag| flag.len() + 1);
        let mut ranges = Vec::new();
        let mut start = 0;
        let mut spent = fixed;
        for (index, item) in items.iter().enumerate() {
            let cost = item.len() + 1 + flag;
            if index > start && spent + cost > ARGV_BUDGET {
                ranges.push(start..index);
                start = index;
                spent = fixed;
            }
            spent += cost;
        }
        if start < items.len() {
            ranges.push(start..items.len());
        }
        ranges
    }

    /// The command line (without the program) for `items`, which must be one
    /// range of [`Template::chunks`].
    pub(super) fn render(&self, items: &[String]) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.tokens.len() + items.len());
        for token in &self.tokens {
            if token == FILES_PLACEHOLDER {
                argv.extend(items.iter().cloned());
            } else if let Some(flag) = token.strip_prefix("$FILES:") {
                for item in items {
                    argv.push(flag.to_owned());
                    argv.push(item.clone());
                }
            } else if token == DIR_PLACEHOLDER {
                argv.extend(items.iter().cloned());
            } else if token.contains(DIR_PLACEHOLDER) {
                argv.push(token.replace(DIR_PLACEHOLDER, items.first().map_or(".", String::as_str)));
            } else if token == ROOT_PLACEHOLDER {
                argv.push(".".to_owned());
            } else if token == PATH_PLACEHOLDER {
                argv.push(items.first().cloned().unwrap_or_default());
            } else {
                argv.push(token.clone());
            }
        }
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn extra_arguments_land_before_the_file_list() {
        let template = Template::new(&strings(&["-f", "xml", "$FILES"]), &strings(&["-c", "cs.xml"]));
        assert_eq!(
            template.render(&strings(&["A.java", "B.java"])),
            strings(&["-f", "xml", "-c", "cs.xml", "A.java", "B.java"])
        );
    }

    #[test]
    fn a_per_file_flag_is_repeated_for_each_file() {
        let template = Template::new(&strings(&["lint", "$FILES:--path"]), &[]);
        assert_eq!(
            template.render(&strings(&["a.proto", "b.proto"])),
            strings(&["lint", "--path", "a.proto", "--path", "b.proto"])
        );
    }

    #[test]
    fn a_bare_dir_token_lists_every_directory() {
        let template = Template::new(&strings(&["run", "$DIR"]), &[]);
        assert_eq!(template.items(), Items::Dirs);
        assert_eq!(
            template.render(&strings(&["./a", "./b"])),
            strings(&["run", "./a", "./b"])
        );
    }

    #[test]
    fn an_embedded_dir_token_needs_one_invocation_per_directory() {
        let template = Template::new(&strings(&["--chdir=$DIR"]), &[]);
        let dirs = strings(&["./a", "./b"]);
        assert_eq!(template.chunks(&dirs), vec![0..1, 1..2]);
        assert_eq!(template.render(&dirs[1..2]), strings(&["--chdir=./b"]));
    }

    #[test]
    fn a_root_template_is_one_invocation_whatever_the_file_count() {
        let template = Template::new(&strings(&["lint", "$ROOT"]), &[]);
        assert_eq!(template.items(), Items::Nothing);
        assert_eq!(template.chunks(&strings(&["a", "b", "c"])), vec![0..0]);
        assert_eq!(template.render(&[]), strings(&["lint", "."]));
    }

    #[test]
    fn chunks_partition_the_items_and_respect_the_budget() {
        let template = Template::new(&strings(&["-f", "xml", "$FILES"]), &[]);
        let files: Vec<String> = (0..2000)
            .map(|i| format!("src/main/java/com/example/pkg{i:05}/Thing.java"))
            .collect();
        let ranges = template.chunks(&files);
        assert!(ranges.len() > 1, "2000 long paths cannot share one command line");
        let mut next = 0;
        for range in &ranges {
            assert_eq!(range.start, next, "chunks must be contiguous");
            next = range.end;
            let line: usize = template.render(&files[range.clone()]).iter().map(|a| a.len() + 1).sum();
            assert!(line <= ARGV_BUDGET, "chunk {range:?} is {line} characters");
        }
        assert_eq!(next, files.len(), "every file must land in a chunk");
    }

    #[test]
    fn an_oversized_single_item_still_gets_its_own_chunk() {
        let template = Template::new(&strings(&["$FILES"]), &[]);
        let huge = vec!["x".repeat(ARGV_BUDGET * 2)];
        assert_eq!(template.chunks(&huge), vec![0..1]);
    }
}
