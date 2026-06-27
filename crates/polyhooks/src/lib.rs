//! `polyhooks` — a vendored fork of [`prek`](https://github.com/j178/prek)
//! (a fast, pure-Rust reimplementation of `pre-commit`), merged into the
//! polylint workspace as a single crate.
//!
//! Phase 1 is a faithful vendoring: behavior, config format, and CLI surface
//! are unchanged from upstream `prek` v0.4.5. The three upstream helper crates
//! (`prek-consts`, `prek-identify`, `prek-pty`) are flattened into the public
//! modules below so the binary target (`src/main.rs`) and the integration
//! tests can reach them without separate crates.
//!
//! See `NOTICE` and `LICENSE` for upstream attribution (MIT, © 2024 j178).

// Vendored upstream code: prek does not document its full internal surface, and
// flattening its four crates into submodules shifted some intra-doc-link paths.
// Holding 75k LOC of upstream to our rustdoc gates would make merging future
// upstream changes intractable, so exempt this one vendored crate from both
// -D missing-docs and -D broken-intra-doc-links. Our own conventions resume at
// the poly-hooks integration boundary.
#![allow(missing_docs)]
#![allow(rustdoc::broken_intra_doc_links)]

/// Shared constants and environment-variable helpers (was `prek-consts`).
pub mod consts;

/// File-type identification by filename, shebang, and interpreter
/// (was `prek-identify`).
pub mod identify;

/// Pseudo-terminal utilities used to run hooks with a PTY (was `prek-pty`).
///
/// Unix-only, matching upstream `prek`, which gated `prek-pty` behind
/// `cfg(unix)`.
#[cfg(unix)]
pub mod pty;
