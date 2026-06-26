---
priority: high
---

# TDD + prek Workflow

- Practice **red-green-refactor**. Write a failing test first when the change is observable
  from the public API or the CLI surface.
- **Per-backend `insta` fixtures** are the unit-test bar. Every backend ships a **known-bad
  file** (asserts the expected `Diagnostic`s) and a **known-unformatted file** (asserts the
  exact formatted output). Include a Python fixture proving docstring code blocks get
  formatted, and a tier-2 fixture (e.g. Go or shell) proving the generic formatter reindents
  with zero system tools installed. New backends need both fixtures before they are wired into
  the registry.
- Before every commit, run:
  - `cargo fmt`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `prek run -a` — covers typos, markdown line length, cargo fmt / clippy / sort / machete /
    deny, rustdoc-lint, and rust-max-lines (the 1000-line cap).
- Clippy is strict (`-D warnings`); do not silence with `#[allow(...)]` unless the warning is
  genuinely incorrect — and write a one-line `//` comment explaining why when you do.
- **Commits are signed** and use **Conventional Commit prefixes** (`feat:`, `fix:`, `perf:`,
  `chore:`, `refactor:`); the gitfluff `commit-msg` hook enforces the prefix. Match the style
  in `git log`.
</content>
