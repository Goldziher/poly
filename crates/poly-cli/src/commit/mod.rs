//! `poly commit`: commit-message linting and cleanup, driven by the upstream `gitfluff`
//! library. This module owns the poly-specific glue: the command line, loading the `[commit]`
//! table of `poly.toml`, and the read / report / write-back flow.

mod args;
mod config;
mod run;

pub use args::CommitArgs;
pub use run::run_commit_lint;
