//! Rewriting files with a recipe tool: in place on a scratch copy, one batch at
//! a time.

use std::ops::Range;
use std::path::Path;

use anyhow::{Context, anyhow};

use super::argv::Template;
use super::lint::Member;
use super::mirror::Mirror;
use super::{RecipeTool, exec, paths};
use crate::engine::{FormatOutput, SourceFile};

/// One answer per input file, in order.
pub(in crate::engines::catalog_tool) type FormatResults = Vec<anyhow::Result<FormatOutput>>;

/// Single refusals after which a batch with no success is abandoned: the
/// invocation itself is wrong (a flag this version of the tool lacks), and
/// halving it into a spawn per file would only repeat the failure.
const ABANDON_AFTER: usize = 2;

impl RecipeTool {
    /// Format `files`, one result per file.
    pub(in crate::engines::catalog_tool) fn format_files(&self, files: &[&SourceFile]) -> FormatResults {
        let members = self.locate(files);
        let refs: Vec<&Member<'_>> = members.iter().collect();
        self.rewrite(&self.passes, &refs)
            .into_iter()
            .map(|outcome| {
                outcome.map(|rewritten| match rewritten {
                    Some(text) => FormatOutput::Formatted(text),
                    None => FormatOutput::Unchanged,
                })
            })
            .collect()
    }

    /// Whether the tool has a stdin form cheaper than a scratch copy for one file.
    pub(in crate::engines::catalog_tool) fn has_stdin_form(&self) -> bool {
        self.stdin.is_some()
    }

    /// Format one file through the tool's stdin form.
    pub(in crate::engines::catalog_tool) fn format_stdin(&self, src: &SourceFile) -> anyhow::Result<FormatOutput> {
        let argv = self.stdin.as_deref().context("this tool has no stdin form")?;
        let captured = exec::run_with_stdin(&self.launcher, argv, &self.cwd, &self.env, &src.content)?;
        if !captured.exited_ok(&self.recipe.ok_exits) {
            return Err(anyhow!("{} failed: {}", self.tool.name, captured.explanation()));
        }
        Ok(if captured.stdout == *src.content {
            FormatOutput::Unchanged
        } else {
            FormatOutput::Formatted(captured.stdout)
        })
    }

    /// Run `passes` over copies of `members` and return each file's new content,
    /// or `None` where it came out unchanged.
    pub(super) fn rewrite(&self, passes: &[Template], members: &[&Member<'_>]) -> Vec<anyhow::Result<Option<String>>> {
        let mut outcomes: Vec<Option<anyhow::Result<Option<String>>>> = members.iter().map(|_| None).collect();
        if members.is_empty() || passes.is_empty() {
            return members.iter().map(|_| Ok(None)).collect();
        }
        let mirror = match Mirror::new() {
            Ok(mirror) => mirror,
            Err(error) => return members.iter().map(|_| Err(anyhow!("{error:#}"))).collect(),
        };
        for member in members {
            mirror.copy_context(&self.cwd, &member.rel, &self.context_names);
        }
        let rels: Vec<String> = members.iter().map(|m| paths::slashed(&m.rel)).collect();
        let mut run = Rewrite {
            tool: self,
            mirror: &mirror,
            passes,
            members,
            rels: &rels,
            outcomes: &mut outcomes,
            accepted: 0,
            refused: 0,
            abandoned: None,
        };
        run.range(0..members.len());
        outcomes
            .into_iter()
            .map(|outcome| outcome.unwrap_or_else(|| Err(anyhow!("not rewritten"))))
            .collect()
    }
}

/// One rewrite in flight, isolating failures by halving.
struct Rewrite<'a, 'm> {
    tool: &'a RecipeTool,
    mirror: &'a Mirror,
    passes: &'a [Template],
    members: &'a [&'a Member<'m>],
    rels: &'a [String],
    outcomes: &'a mut Vec<Option<anyhow::Result<Option<String>>>>,
    accepted: usize,
    refused: usize,
    abandoned: Option<String>,
}

impl Rewrite<'_, '_> {
    fn range(&mut self, range: Range<usize>) {
        if let Some(reason) = &self.abandoned {
            for index in range {
                self.outcomes[index] = Some(Err(anyhow!("{reason}")));
            }
            return;
        }
        match self.attempt(&range) {
            Ok(()) => self.accepted += 1,
            Err(reason) if range.len() == 1 => {
                self.refused += 1;
                if self.accepted == 0 && self.refused >= ABANDON_AFTER {
                    self.abandoned = Some(reason.clone());
                }
                self.outcomes[range.start] = Some(Err(anyhow!("{} failed: {reason}", self.tool.tool.name)));
            }
            Err(_) => {
                let middle = range.start + range.len() / 2;
                self.range(range.start..middle);
                self.range(middle..range.end);
            }
        }
    }

    /// Write fresh copies of `range`, run every pass over them, read them back.
    /// Fresh because a failed attempt may already have rewritten the files it
    /// liked, and a second pass over formatted input is not a no-op for every tool.
    fn attempt(&mut self, range: &Range<usize>) -> Result<(), String> {
        for index in range.clone() {
            let member = self.members[index];
            self.mirror
                .write(&member.rel, &member.src.content)
                .map_err(|error| format!("{error:#}"))?;
        }
        let items = &self.rels[range.clone()];
        for pass in self.passes {
            for chunk in pass.chunks(items) {
                let argv = pass.render(&items[chunk]);
                let captured = exec::run(&self.tool.launcher, &argv, self.mirror.root(), &self.tool.env)
                    .map_err(|error| format!("{error:#}"))?;
                if !captured.exited_ok(&self.tool.recipe.ok_exits) {
                    return Err(captured.explanation());
                }
            }
        }
        for index in range.clone() {
            let member = self.members[index];
            let written = read_back(&self.mirror.root().join(&member.rel)).map_err(|error| format!("{error:#}"))?;
            self.outcomes[index] = Some(Ok((written != *member.src.content).then_some(written)));
        }
        Ok(())
    }
}

fn read_back(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading back {}", path.display()))?;
    String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8 after formatting", path.display()))
}
