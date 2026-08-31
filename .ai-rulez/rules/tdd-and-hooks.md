---
priority: high
---

# TDD + Hooks Workflow

- Practice **red-green-refactor**. Write a failing test first when the change is observable
  from the public API or the CLI surface.
- **Per-backend `insta` fixtures** are the unit-test bar. Every backend ships a **known-bad
  file** (asserts the expected `Diagnostic`s) and a **known-unformatted file** (asserts the
  exact formatted output), under `crates/poly-core/tests/` with snapshots in
  `tests/snapshots/`. Include a Python fixture proving docstring code blocks get formatted, and
  a tier-2 fixture (e.g. Go or shell) proving the generic formatter reindents with zero system
  tools installed. New backends need both fixtures before they are wired into the registry.
- This repo **dogfoods its own hooks**: the dev hooks live in `poly.toml` (`[hooks]`), not in a
  `.pre-commit-config.yaml`. Before every commit, run:
  - `cargo fmt`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --no-fail-fast` — **`--no-fail-fast` is not optional.** Cargo stops
    after the first failing test *binary* and reports a partial pass count that reads like a
    green run.
  - `poly hooks run pre-commit --all-files` — runs the full pre-commit stage from `poly.toml`
    (the `[hooks.builtin]` `lint` / `fmt` keys, plus `commit`, `file_safety`, `cargo`): typos,
    markdown, JSON/YAML/TOML, shell, Rust formatting, …, the
    pure-Rust file-safety checks, cargo clippy / sort / machete / deny, rustdoc-lint, and
    rust-max-lines (the 1000-line cap). `poly hooks install` wires the git-hook shims so this
    runs automatically on `git commit`.
  - `task harden` (`scripts/harden.sh`, `docs/harden-corpus.md`) runs poly over real
    third-party repositories instead of fixtures; its per-rule counts feed a
    ship-this-rule-on-by-default decision. It never runs on the PR path — only nightly CI
    (the `hardening` job in `ci.yaml`) or manually via `task harden -- <roots>`.

## What a green `cargo fmt` + `cargo clippy` does NOT prove

Both pass with broken rustdoc links present, so they are **not** evidence the commit will land.
The pre-commit stage also runs `scripts/hooks/rustdoc-lint.sh`, which is:

```sh
RUSTDOCFLAGS="-D missing-docs -D rustdoc::broken_intra_doc_links" \
  cargo doc --workspace --no-deps --quiet --all-features
```

Run that yourself before staging (narrow it with `-p poly-core` for a faster local loop). Two
classes of failure it catches that nothing else does: an **undocumented public item**
(`-D missing-docs`), and a **broken intra-doc link**. Link forms seen to be error-level in
practice:

- `` [`super::lint`] `` for a **trait method** — a method is not a module item; use
  `` [`Engine::lint`](crate::engine::Engine::lint) ``.
- A name that lives in a **sibling module and is not in scope** — use the full `crate::…` path.
- `` [`super::mod@super`] `` — the `mod@` disambiguator goes **before** the path
  (`` [`mod@super`] ``), not inside it.

Links to **private** items are warnings only and do not block.

## Other ways the commit gate bites

- The pre-commit `fmt` hook formats **markdown too**. Run `poly fmt --fix` over every markdown
  file you touch (ADRs especially) before staging, or the commit is rejected. (`.ai-rulez/**` is
  in `[discovery] exclude`, so rule files themselves are not formatted by it.)
- The hooks validate the **staged snapshot**, not the worktree (ADR 0019): content is
  materialized from the git **index** with `git checkout-index` into the per-user cache dir, so
  unstaged edits and untracked files are invisible to the check. That is what makes a split
  commit checkable on its own content — and it means a fix you forgot to `git add` does not
  count. `[hooks] snapshot_include` is the one opt-in exception: it symlinks named untracked
  paths into the snapshot for a `workspace` hook's build to read, but a fix to an included file
  is withheld (there is no staged blob to write into) and it is never a per-file hook's input.
- **Never pipe `git commit` through `tail` / `head`.** A pipeline reports the *filter's* exit
  code, so a rejected commit looks like success and silently does not land. Run it unpiped and
  read the exit code.

## Style

- Clippy is strict (`-D warnings`); do not silence with `#[allow(...)]` unless the warning is
  genuinely incorrect — and write a one-line `//` comment explaining why when you do.
- **Commits are signed** and use **Conventional Commit prefixes** (`feat:`, `fix:`, `perf:`,
  `chore:`, `refactor:`, `docs:`, `test:`), optionally scoped (`fix(oxc):`); the gitfluff-backed
  `commit` builtin runs at the `commit-msg` stage and enforces the prefix. Match the style in
  `git log`.
