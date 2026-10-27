//! Pinning a batch invocation's failure on the members that caused it.
//!
//! A tool run over many files reports one exit status. A non-zero status is not
//! a per-file verdict: a tool rejecting one malformed member exits non-zero
//! having correctly processed the rest, and a tool rejecting a *flag* exits
//! non-zero having done nothing at all — and from outside, after the fact, those
//! are indistinguishable. Reading results back in the second case would report
//! every member clean, a false pass reached without any tool having looked at
//! the source. So a failing invocation is never read back: it is halved and each
//! half re-run from freshly written inputs, until the failure is pinned to
//! single members, which are left to the per-file path.
//!
//! Giving up on the whole range at the first failure — the first version of
//! batching — is correct and ruinous: one unparseable script among a hundred
//! sent all hundred back to one spawn per file, and real repositories always
//! have one. Halving costs about `log2(n)` extra invocations per bad member.

use std::ops::Range;

/// What a batch needs from the thing it is isolating failures in.
pub(super) trait Members {
    /// Run the tool over `range` from freshly written inputs. `true` when it
    /// accepted the range as a whole.
    ///
    /// Fresh because a failed invocation has already rewritten the members it
    /// liked, and a second pass over already-processed input is not guaranteed
    /// to be a no-op.
    fn attempt(&mut self, range: &Range<usize>) -> anyhow::Result<bool>;

    /// Record the outcomes of `range`, which the last [`Self::attempt`] accepted.
    fn accept(&mut self, range: &Range<usize>);

    /// Record that `index`, on its own, was refused.
    fn refuse(&mut self, index: usize);

    /// The tool's name, for the error raised when it refuses everything.
    fn tool(&self) -> &str;
}

/// How many invocations have been accepted and how many single members refused,
/// which is what tells a bad member from a bad invocation.
#[derive(Default)]
pub(super) struct Isolation {
    accepted: usize,
    refused: usize,
}

impl Isolation {
    /// Establish an outcome for every member of `range`.
    ///
    /// Two members refused with nothing ever accepted means the invocation
    /// itself is wrong (a flag the installed tool does not know), and the whole
    /// batch is abandoned with an `Err` instead of being halved into one doomed
    /// spawn per file.
    pub(super) fn run(&mut self, members: &mut impl Members, range: Range<usize>) -> anyhow::Result<()> {
        if members.attempt(&range)? {
            self.accepted += 1;
            members.accept(&range);
            return Ok(());
        }
        if range.len() == 1 {
            self.refused += 1;
            members.refuse(range.start);
            if self.accepted == 0 && self.refused >= 2 {
                anyhow::bail!(
                    "'{}' refused every batch it was given; falling back to per-file execution",
                    members.tool()
                );
            }
            return Ok(());
        }
        let middle = range.start + range.len() / 2;
        self.run(members, range.start..middle)?;
        self.run(members, middle..range.end)
    }
}
