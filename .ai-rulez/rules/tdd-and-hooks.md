---
priority: high
---

# TDD + Hooks Workflow

- Practice **red-green-refactor**. Write a failing test first when the change is observable
  from the public API or the CLI surface.
- **Per-backend `insta` fixtures** are the unit-test bar. Every backend ships a **known-bad
  file** (asserts the expected `Diagnostic`s) and a **known-unformatted file** (asserts the
  exact formatted output). Include a Python fixture proving docstring code blocks get
  formatted, and a tier-2 fixture (e.g. Go or shell) proving the generic formatter reindents
  with zero system tools installed. New backends need both fixtures before they are wired into
  the registry.
- This repo **dogfoods its own hooks**: the dev hooks live in `poly.toml` (`[hooks]`), not in a
  `.pre-commit-config.yaml`. Before every commit, run:
  - `cargo fmt`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `poly hooks run pre-commit --all-files` — runs the full pre-commit stage from `poly.toml`:
    polylint + polyfmt (typos, markdown, JSON/YAML/TOML, shell, Rust formatting, …), the
    pure-Rust file-safety checks, cargo clippy / sort / machete / deny, rustdoc-lint, and
    rust-max-lines (the 1000-line cap). `poly hooks install` wires the git-hook shims so this
    runs automatically on `git commit`.
- Clippy is strict (`-D warnings`); do not silence with `#[allow(...)]` unless the warning is
  genuinely incorrect — and write a one-line `//` comment explaining why when you do.
- **Commits are signed** and use **Conventional Commit prefixes** (`feat:`, `fix:`, `perf:`,
  `chore:`, `refactor:`); the gitfluff `commit-msg` hook enforces the prefix. Match the style
  in `git log`.
</content>
