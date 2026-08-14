# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The single `poly`
binary drives lint, format, hooks, and commit checks from one `poly.toml`.

## [Unreleased]

### Fixed

- **Generated-file detection now scans below leading YAML frontmatter.** Hash stamps and generated
  banners in the five lines after a valid frontmatter block are recognized, while unterminated
  frontmatter and markers beyond that bounded window remain ordinary source.

## [0.21.3] - 2026-08-14

### Fixed

- **Generated-file detection now requires a structured content-hash stamp.** Ordinary source such
  as `use a::hash::b;` is formatted and lint-fixed normally instead of being mistaken for generated
  code. Markers such as `alef:hash:<digest>` remain protected.

## [0.21.2] - 2026-08-14

### Fixed

- **Hook exclusions now share discovery's repo-root anchoring syntax.** A leading slash is treated
  as a repo-root anchor when hook globs are compiled, so `/e2e/**` excludes the root `e2e` tree
  without excluding nested package directories. Recursive patterns such as `**/target/**` retain
  their existing behavior.

## [0.21.1] - 2026-08-14

### Fixed

- **Bare Jinja, Vento, and Mustache templates now fail closed instead of being guessed from their
  contents.** XML documentation in a C#-generating `.jinja` file looked like rendered markup, so
  markup_fmt reflowed the template and produced invalid generated code. Generic templates are now
  formatted only when a second extension names a markup target, such as `.html.jinja` or
  `.xml.jinja`; `.cs.jinja` remains code, and ambiguous bare templates are preserved byte for byte
  with an actionable skip reason.

- **Explicit roots now honor configured exclusions consistently.** Naming a file no longer silently
  bypasses `[discovery] exclude`, and a relative file preceding an excluded directory no longer
  loses the directory's config anchor and exposes its files to formatting. Root count and argument
  order now have the same result. `--include-excluded` is the deliberate one-off override for a
  named file or directory; the existing `--force-exclude` flag remains accepted for compatibility.

## [0.21.0] - 2026-08-13

### Fixed

- **A hook that fails under staged isolation now says so.** Since 0.20.0 every hook in a
  commit-gating run validates the staged snapshot, not the worktree — correct, and nobody wants it
  back — but a failure gave no hint the content checked was not what is in the working tree. Two
  consumers lost a debugging round to the same shape independently: a clippy error naming a field
  that exists in their worktree, because the matching change sat in a file they had not staged, and
  `cargo check` had passed locally seconds earlier. The stage banner already named the tree, but it
  has scrolled off by the time a wall of compiler output ends. The failure now carries the reason
  and the two ways out — stage the rest, or `[hooks] isolate = false`. Printed once per run at the
  first qualifying failure, only under staged validation, and never for a timeout or a withheld
  fix, which carry their own remedies.

- **A language poly has no lint rules for is no longer counted as linted.** `poly lint .` over a
  tree holding `a.kt`, `c.xyz` and `d.py` reported `No issues found. (2 file(s) linted)` and
  exited 0. The Kotlin file was in that count: it routes to the cross-cutting backends
  (spell-check, ast-grep, comment removal) and to nothing carrying a single Kotlin rule, so a
  consumer with Kotlin, Swift and Zig as first-class languages read a green run as full coverage
  of three languages nothing had examined. `--deny-skips` could not catch it either, because
  none of it was reported as a skip.

  Such a file now leaves the linted count and joins the skipped set with the language named —
  `skipped a.kt: no lint rules for Kotlin` — which puts it in front of `--deny-skips` and
  `--max-skips`, and in the `skipped` field of the `--format json` / `toon` document. The reason
  is deliberately distinct from the existing `no matching engine for this file type`: an engine
  *did* match, poly simply has nothing to say about the language, and the fix for that is a
  linter for it (`[tools.<name>]`) rather than a rename or an exclude.

  The cross-cutting backends still run over these files and their findings are still reported —
  a Kotlin file can carry a typo finding and still be uncovered by any Kotlin rule. The same
  accounting applies wherever a language's linter is opt-in or absent, so a shell script with no
  `shellcheck` installed is reported rather than counted. **Default behaviour is unchanged: this
  is coverage information, not a failure, and a run whose only skips are no-rules languages
  still exits 0.** Only `--deny-skips` / `--max-skips` turn it into a gate.

  **The whole-project phase counts as coverage.** `poly lint` is two halves, and the per-file
  tier has no Rust rules — but the whole-project phase runs `cargo clippy`, so a plain
  `poly lint .` over a Rust repository was printing hundreds of `no lint rules for Rust` lines a
  few lines above the `✓ cargo-clippy` that contradicted them. `poly lint` now resolves which
  languages its whole-project tool set covers *before* the per-file run and those files are not
  recorded as skipped at all — so the linted count, the note, the JSON payload and
  `--deny-skips` describe the same run rather than being reconciled at display time. The mapping
  is keyed on the whole-project tool id: `cargo-clippy` establishes Rust coverage, while
  `cargo-sort` / `cargo-machete` / `cargo-deny` deliberately do not (they read manifests and the
  dependency graph, never `.rs` source). With the phase off — `--no-workspace`, `[lint]
  workspace = false`, a path-scoped run, or a repo with no `[hooks]` section — nothing lints
  Rust in that run, so the skip is accurate and still appears.

- **The skipped-file note is grouped by reason instead of capped as a list.** A run could emit
  229 consecutive identical lines, which both drowned the findings and pushed every *other*
  reason past the old flat cap of 20 — so the rare skip worth reading was exactly the one
  dropped. Each reason is now bounded on its own: a reason covering more than three files
  collapses to `skipped N file(s): <reason>` plus three example paths, and a reason covering
  three or fewer still names every file. A bulk reason therefore costs two lines however many
  files it covers and can never crowd out a one-off. `--verbose` lists every file individually,
  and `--format json` / `toon` carry the complete set as before.

- **A walked file poly cannot identify is no longer invisible.** `c.xyz` above appeared nowhere
  at all: it is neither a language poly knows nor a path the caller named. The run summary now
  reports `N file(s) of unrecognized type not checked` and names the first few. They are
  deliberately *not* itemised as skips — poly's own tree holds 83 unidentifiable files out of
  446 (snapshots, lock files, images, `LICENSE`), and one skip each would fire `--deny-skips` in
  every repository and bury the skips that mean something. A path named explicitly on the
  command line is unaffected and remains a skip, since naming a path is a request to check it.

- **The cache-permission warning no longer fires forever after an upgrade.** 0.20.0 began
  creating cache directories `0700` but never changed one that already existed, so every user
  upgrading from an older poly — whose slot was created under the usual `022` umask — saw
  `cache directory is readable by other local users` on every single run, with no way to make
  it stop short of a manual `chmod`. A warning that never goes away trains people to ignore
  warnings, which is worse than the low-severity issue it reported.

  The rule now splits on who chose the location. A directory resolved from the **default**
  per-user OS cache home (`~/.cache/poly/…`, `~/Library/Caches/poly/…`) with no
  `POLY_CACHE_HOME` and no `[cache] dir` override is poly's own: poly picked the path, created
  it, and invited nobody to share it, so it is tightened to `0700` in place and silently — there
  is nothing for the user to decide. A location named **explicitly** by `POLY_CACHE_HOME`,
  `[cache] dir`, or `--cache-dir` keeps 0.20.0's behaviour exactly: its mode may encode a
  deliberate choice (a shared CI cache, a team directory), so poly never changes it and warns
  once with the `chmod 700` command.

  Tightening is best-effort and Unix-only: poly checks that it owns the directory (`st_uid`
  matches the effective uid) before changing the mode, and a chmod that fails anyway — read-only
  mount, another account's directory — degrades to the same warning rather than failing the run.
  The warning is now de-duplicated per directory rather than per process, so a run that meets two
  different loose directories reports both.

## [0.20.1] - 2026-08-13

### Fixed

- **A cargo hook is no longer charged for a package-cache lock held outside the run.** Before
  spawning a hook in the `cargo` exclusion set, poly probes `$CARGO_HOME/.package-cache` and,
  if a process outside the run holds it, waits for it to clear **before** starting the hook's
  time budget — so a `cargo deny check` that takes seconds on its own can no longer be killed
  at the whole-project budget for a wait it did not cause. The wait prints
  `⏸ waiting to start: <hook> … the hook has not been spawned and its time budget has not
  started`, is bounded by half the hook's own budget (no new configuration — a hook with
  timeouts disabled never waits), and on expiry the hook is started anyway rather than
  withheld: a check that did not run must never be reported as a pass.

  This mitigates the common case, and says so rather than claiming a fix. The lock can still be
  taken between the probe and the spawn, cargo re-acquires it mid-run, and the artifact-
  directory lock (`<target-dir>/<profile>/.cargo-lock`) is not probed at all. For those, the
  post-spawn notice above remains the report — and it says outright that the budget is still
  counting, because poly cannot pause a queue it does not own.

- **A hook's `run` line is no longer broken, or a source file truncated, by poly's appended
  arguments.** Unix hook lines were built as `{line} "$@"`, so a line whose tail cannot take a
  trailing word failed to parse — a compound command ending in `done`, `fi`, `esac` or `}` — and
  the shell diagnostic named poly's appended text rather than the hook, reading as a poly bug.
  Worse, a line ending in a dangling redirect operator (`cargo build >`) made the appended `"$@"`
  the redirect **target**, so poly truncated one of the matched files. That is now refused.

  A line that already forwards positionals with `"$@"` or `"$*"` no longer gets a second copy
  appended, which is what makes script-shaped lines expressible: they read the arguments
  themselves. The check deliberately does not key on `$1` — `grep -q TODO "$1" && shellcheck`
  guards on the first file while relying on the append for the rest, and that works today.
  `argv[0]` is now the hook id, so a shell diagnostic names the hook that produced it.

- **`cmd_quote` no longer lets a trailing backslash escape its own closing quote** (Windows). It
  doubled embedded quotes and escaped `%` but left backslashes alone, so a value ending in an odd
  run of them stopped the quoting terminating where intended. Only the runs immediately before a
  quote and before the closing quote are doubled; interior backslashes in ordinary paths are
  untouched. Not reachable through tracked filenames, which cannot end in a backslash.

- **A hook's output is shown while it runs, instead of all at once after it exits.** The progress
  preview fed its sink only after the child exited, so a long-running hook showed nothing for its
  whole run — a hook that was working and a hook that was wedged looked identical, which is what
  the preview exists to tell apart. Output is now handed over as it arrives, from the
  supervisor's poll loop rather than the drain threads: feeding from a drain thread would hold a
  lock across a terminal draw, and a stalled drain is a pipe nobody empties, which blocks the
  child forever. The combined stream is now interleaved by arrival rather than all stdout then
  all stderr; captured stdout and stderr are byte-identical to before.

- **`cargo sort` no longer reports a pass for a check it failed.** `cargo sort --check` exits 0
  when it considers a manifest badly *formatted*, printing only a warning, and the cargo group
  gates on the exit code — so the hook rendered a green check while the tool had complained. Both
  the check and fix commands now pass `--no-format`, so the exit code is a faithful signal for
  sort order, the one thing the hook exists to check. Suppressing rather than honouring that
  opinion is deliberate: cargo-sort indents with four spaces and poly formats TOML with taplo at
  two, so gating on its formatting verdict would deadlock poly's own hooks against each other.
  The same flag on the fix command stops `poly lint --fix` churning manifests that `poly fmt`
  then reverts.

## [0.20.0] - 2026-08-13

### Added

- **`poly doctor`** — the command to run before filing a bug. Reports the running executable and its
  build identity, **every** `poly` on `PATH` with each one's version, the config files in effect, and
  the cache directory; `--format json` for pasting into a report. It exits non-zero on a real defect,
  such as one install shadowing another.

  A shadow warning also fires from ordinary commands when a different `poly` earlier on `PATH` would
  answer a bare `poly`. It is emitted before argument parsing, because the incident that motivated it
  was `poly --version` — which prints and exits without reaching any command handler.

- **`--deny-skips` and `--max-skips <N>` on `poly lint` and `poly fmt`.** A skipped file — no
  matching engine, a generated file, an unreadable path — is now always counted and named with its
  reason, so a run that checked nothing cannot look like a clean pass. A run emptied entirely by
  skips reports `Nothing was linted.` rather than `No issues found.` The new flags turn a skip into
  a hard failure for CI, exiting **2** — the class callers treat as an error, rather than the 1 that
  is often treated as success. `--verbose` lists every skipped file instead of the first 20.

- **`poly doctor` warns when an exclude rule prunes more than the directory it names.** `exclude`
  patterns are gitignore rules: a glob with no `/` in it matches a directory of that name **at any
  depth**, so `exclude = ["e2e/**"]` silently skips every nested `e2e` in the tree, not just the one
  at the root. A consumer was checking two files where they expected five.

  The semantics are unchanged and deliberately so — they are gitignore's, and anchoring is already
  expressible with a leading slash (`exclude = ["/e2e/**"]`). What was missing was any way to notice.
  `poly doctor` now reports each rule that pruned directories at two or more distinct depths, names
  the directories, and suggests the anchored form. The behaviour is documented in the `--exclude`
  help, the `[discovery] exclude` schema, the MCP parameter, and ADR 0017.

- **Build identity in `--version`, and in every MCP response.**

  ```text
  poly 0.19.7 (dev build v0.19.7-10-g361844a, debug)
  ```

  Two builds of the same version were previously indistinguishable. One consumer had to diff file
  mtimes to tell a fixed poly from one that was corrupting their templates, and another reported a
  status against the wrong binary. The id comes from `git describe` at compile time — no `--dirty`,
  no timestamp — so it stays a pure function of the committed tree and does not disturb reproducible
  builds. MCP callers get the same identity on every response, since they have no `--version` to
  fall back on.

- **`poly mcp` fails loudly once its own binary is replaced.** It fingerprints its executable at
  startup and re-checks per request. Eight long-lived servers were found still answering from a
  deleted inode with known-destructive semantics; upgrading poly left every running server stale
  forever, with no expiry and no signal. The `version` tool stays exempt so it can explain why the
  others stopped.

- **Discovery now reports what it excluded, per rule**, so a summary can distinguish "everything is
  clean" from "everything I chose to look at is clean". One repo carried formatting drift for weeks
  because a top-level check reported an unqualified pass while an excluded directory held
  unformatted files.

  ```text
  All formatted. (260 file(s) checked, 2 skipped (Go/Helm template syntax), 2 file(s) and 8 director(ies) excluded by config)
    excluded by [discovery] exclude / --exclude: **/identify/tags.rs (1 file(s)), .ai-rulez/** (1 dir(s)), and 5 more rule(s)
    excluded directories were not walked, so the files inside them are not counted
  ```

  Files and directories are counted separately on purpose: an excluded directory is pruned at its
  boundary and never walked, so the files inside it were never measured. Reporting a single total
  would claim a precision nobody paid for.

- **`--force-exclude` / `[discovery] force_exclude`, applied automatically in the hook path.**
  `[discovery] exclude` pruned the directory walk but never applied to a file named explicitly on
  the command line. That is right when a human names a file and wrong in a hook, which is *always*
  handed explicit staged paths — so a repo excluding `**/*.tf` found the exclude silently inert in
  its own pre-commit hook, poly reformatted 519 lines of Terraform, and `terraform fmt` then
  rejected the result, leaving the repo unable to be green under both tools.

  The generated `lint` and `fmt` builtin hooks now pass `--force-exclude`, so a repo's excludes hold
  where they matter without any config change. A direct CLI invocation is unchanged: naming a file
  still checks it.

### Changed

- **Whole-project hooks now actually run in parallel.** A priority group ran under a single
  boolean — if *any* hook in it could not run beside a peer, the *entire* group ran one hook at
  a time — and the stage-level `parallel` key defaults to `false`, so in practice a repo with
  even one inline job (poly's own `poly.toml` among them) ran every builtin hook serially on an
  otherwise idle machine.

  Replaced with named exclusion sets scheduled as chains (ADR 0024): hooks naming the same set
  run sequentially in config order, everything else runs concurrently. New job key
  `serial = true | "<name>" | false`. The cargo builtins (`clippy`/`sort`/`machete`/`deny`) join
  a shared `cargo` set by default, because every cargo subcommand takes the package-cache lock
  and a build takes the build-directory lock; a job whose `run` line invokes `cargo` anywhere
  joins that set automatically. See `adrs/0024-hook-concurrency-exclusion-sets.md` for the model
  and its trade-offs.

- Refreshed the whole dependency tree. All pinned git backends move to their latest upstream
  **release** commits, keeping the existing practice of pinning releases rather than arbitrary
  `HEAD`:

  | backend | from | to |
  | --- | --- | --- |
  | oxc (`oxc_formatter`, `oxc_linter`, …) | `65fe65d` — oxlint 1.76.0 / oxfmt 0.61.0 | `c42d639` — oxlint 1.78.0 / oxfmt 0.63.0 |
  | ruff (`ruff_linter`, `ruff_python_formatter`, …) | `80790b3` — 0.16.1 | `5b48a04` — 0.16.2 |
  | biome (`biome_css_analyze`, `biome_graphql_analyze`, …) | `1139f1c` | `6b8f09c` |
  | rubyfmt | `b63fbaa` | `5185d3b` |

  Registry dependencies were upgraded in the same pass, including the exact-pinned `mago` PHP family
  (`=1.42.0` → `=1.43.0`, bumped as a set so the monorepo crates stay consistent), `rumdl` 0.2.42 →
  0.2.54, `tree-sitter-language-pack` 1.14.0 → 1.14.3, `typos` 0.10.43 → 0.10.44, `uncomment` 3.5.1 →
  3.5.2, `ast-grep-core` 0.44.1 → 0.45.1, and `hcl-rs` / `hcl-edit` 0.19.7 / 0.9.6 → 0.19.8 / 0.9.7.

- Every affected engine's `version()` was bumped alongside its wrapped crate, so cached results
  produced by the previous backend versions are invalidated rather than reused. The `version_audit`
  test enforces this contract and now passes against the refreshed lockfile.

- **A file the formatter cannot process now fails the run (exit 2) instead of reporting success.**
  `poly fmt` logged an engine error at `warn` and dropped the file from the results entirely, so an
  unparseable file was erased from the accounting and reported as `All formatted. (0 file(s)
  checked)` with exit 0 — a gate passing on a file it never read. Each affected path is now named
  with the engine's own message.

  Exit **2** specifically. Consumers automate against these codes and at least one treats `1` as
  success for `--fix`, since `1` means "files were rewritten"; folding an unreadable file into
  either `0` or `1` would be wrong in opposite ways. The full contract is now: `0` clean, `1` files
  changed (or would change), `2` the run failed.

- **`extends`: `exclude` lists accumulate instead of replacing.** Under `extends`, arrays previously
  replaced wholesale, so a repo adding a single exclude had to restate the entire inherited array —
  freezing a copy that later changes to the shared base never reached. The repos that most needed a
  shared base were exactly the ones that silently stopped tracking it.

  `hooks.builtin.{lint,fmt,file_safety}.exclude` now inherit `[discovery] exclude` by the same rule,
  which is why poly's own `poly.toml` drops four duplicated exclude stanzas. Opt out per table with
  `exclude_mode = "replace"`.

  **This changes behaviour for anyone who restated `exclude` in order to *shrink* an inherited
  list** — they now get the union. Non-`exclude` arrays (`select`, `stages`, `clippy_args`) still
  replace, deliberately: those are values, not floors, and unioning them would make shrinking a rule
  set impossible.

- **The result cache now folds in the identity of the poly build that produced an entry**, and
  **existing caches are discarded on first run** as a result of a cache-format bump.

  Invalidation previously depended on an author remembering to bump a hand-written
  `Engine::version()` string, and the audit test only enforced that it embedded the *wrapped crate's*
  version — so a change to poly's own logic invalidated nothing. Worse, two builds reporting the same
  version produced identical keys despite behaving differently, so one build was served the other's
  results. That was reproduced: a binary whose formatter would rewrite a file read a cached entry
  from a different build and reported `All formatted`, exit 0.

  A **release** build's identity is `release/<version>` with nothing machine-local, so two release
  builds of one version still share entries and CI caching is unaffected. Development builds include
  an executable fingerprint, because `git describe` alone cannot see uncommitted work.

  Cache eviction also now runs automatically (at most once per 24h per repo: entries older than 14
  days, then oldest-first to a 1 GiB ceiling). It previously never ran unless invoked by hand.

- **`poly lint --fix` no longer rewrites machine-generated files.** Files whose opening lines carry
  a `DO NOT EDIT` / `@generated` / `auto-generated` / `:hash:` marker are still **reported** on —
  that is how a generator bug gets noticed — but the fix is withheld, because rewriting generated
  output is churn the next generation run reverts, and it can destroy the evidence.

  The reported case: ruff's `F841` fired on an unused binding in a generated test, and that binding
  was the only signal that 39 generated tests across 8 files called an API and asserted nothing.
  Running the obvious `poly lint --fix` rewrote it and turned a correct diagnostic about a real
  upstream defect into a clean lint pass.

  The withheld fix is reported rather than silent:

  ```text
  1 generated file(s) not fixed (pass `--fix-generated` to include them).
  ```

  `poly fmt` skips a generated file **only when its header stamps a content hash** (`alef:hash:`,
  `sourcehash`, `@checksum`), because reformatting invalidates the hash and the remedy is a regen
  that discards the formatting — a loop. A bare `DO NOT EDIT` banner does *not* trigger it: skipping
  in `fmt` removes the file from the format gate entirely, and a generator that stamps a
  hand-written file would otherwise drop it out of enforcement with nothing reporting it.

- **`poly lint <paths>` no longer runs the whole-project phase.** Explicit path arguments now scope
  the run to the per-file tier; `poly lint` with no paths is unchanged and still runs
  `cargo clippy` and the other whole-project tools. Previously a path-scoped lint silently escalated
  to an unbounded whole-workspace `cargo` build — nothing in the argument list distinguished
  `poly lint some/file.py` from a full-repository run, and when another process held the cargo
  package lock it blocked indefinitely with no output. Two agents in one reporting repo concluded
  poly was broken; one was killed at 13 minutes.

  A skipped phase is announced:

  ```text
  note: whole-project phase skipped for path-scoped run (pass --workspace to include it)
  ```

  **Naming the root is not path scoping.** `poly lint .` (or `./`, or an absolute path to the same
  directory) means "lint everything" and still runs the whole-project phase, exactly as `poly lint`
  with no arguments does. Only a genuine narrowing — a file or subdirectory — scopes the run.

  **Action required for commit gates that pass staged paths and rely on clippy running:** add the
  new `--workspace` flag. `poly lint --workspace <paths>` restores the previous behaviour.

- Minor: `cargo deny` no longer carries a stale, unused OpenSSL license allowance; every
  workspace crate now sets `publish = false`; `scripts/release-bump.sh` now asserts
  `.codex-plugin/plugin.json`'s version alongside the Claude plugin manifests it already checked.

### Fixed

- **Security: a hook's fix could be written to a file outside the repository.** A tracked
  symlink (git mode `120000`) whose blob names an absolute path outside the repo was faithfully
  recreated as a real symlink inside the staged snapshot `poly hooks` isolation builds, and
  write-back then copied a hook's rewrite straight through it — the destination open follows
  symlinks. Hook isolation is on by default and every routine `git commit` reaches the
  write-back path, so cloning a hostile repository and committing to it was enough. Reproduced
  end to end, not theorised.

  Closed at both layers. The snapshot now removes, on every refresh, any materialized symlink
  whose target does not lexically resolve inside it (checked right after `git checkout-index`).
  Write-back separately checks the destination with `symlink_metadata` before ever opening it —
  a symlink is refused, never followed — and rejects any index path carrying a `..`, an
  absolute, or a drive/UNC component.

- **Write-back no longer trusts `git diff-files` to decide a worktree file is safe to
  overwrite.** That was its only gate before copying a staged-content fix over live worktree
  bytes, and it is the same stat-based dirtiness probe this repo had already stopped trusting
  elsewhere. Under a stale index stat cache — reproducible with
  `git update-index --assume-unchanged` — it called a genuinely modified file clean, and poly
  then destroyed the unstaged work. Write-back now compares blake3 of the worktree bytes against
  the actual index blob (`git cat-file blob :0:<path>`) before writing. The write is also atomic
  now — a sibling temp file plus rename, preserving the destination's permissions — so an
  interrupt mid-copy can no longer truncate the author's source file.

- **A withheld fix now names the actual reason it was withheld**, instead of always printing
  "these files have unstaged changes" — which was false for the symlink and unreadable-path
  cases above, and actively misleading in the security case: it told the author to stage their
  work when the real cause was a symlink poly refused to follow. Five distinct reasons are now
  reported per path (unstaged changes, worktree is a symlink, worktree is not a regular file,
  the snapshot copy is unreadable, the path escapes the repository); the two security refusals
  are marked as such and never offer the `git add` remedy.

- **A hook queued behind a cargo lock is no longer reported as hung.** A cargo hook blocked on
  the package-cache or build-directory lock held by a process outside the run — a developer's
  own build, `rust-analyzer` — prints nothing while it waits, and was indistinguishable from a
  genuinely wedged hook: both said "still running". The liveness notice now says it is waiting
  on a lock and names the resource.

- **A hook that finished on its own is no longer reported as killed.** At the timeout boundary
  poly entered the kill path, reaped the child's real exit status, and then reported `TimedOut`
  regardless — a hook that passed blocked the commit and told the author it had been killed.
  Measured at 52–117 false reports per 8192 spawns under contention, so this was not theoretical.
  A child's own exit status now wins: poly re-probes before signalling, and on Unix treats the
  absence of a termination signal as proof it was not killed. One consequence worth knowing —
  a hook that traps `SIGTERM`, overruns its budget, and then exits non-zero by itself is now
  reported `Failed` with its exit code rather than `TimedOut`, because that is what happened.

- **Killing a hook on Windows no longer leaves its grandchildren running.** The kill path
  signalled only the direct child, so a hook that spawned its own processes — a shell running a
  build, a test runner with workers — left them alive holding locks after poly reported the hook
  killed. The child is now assigned to a Job Object at spawn and the whole tree is terminated,
  matching the process-group behaviour Unix already had. The job carries no kill-on-close limit,
  so a daemon a hook legitimately started still survives a normal run.

- **Cache directories are created owner-only (`0700`) on Unix.** The per-repo cache slot holds
  the hook staged snapshot — a full mirror of the source being committed — and was previously
  created under the process umask, commonly world-readable. An existing directory is
  deliberately never `chmod`'d, since a shared CI cache mode may be intentional; poly warns once
  with the fix command instead.

- **`poly fmt --format json` omitted a file it failed to process, instead of reporting the
  error.** An errored file was simply absent from the output, indistinguishable from a file
  checked and found clean; the only evidence was the exit code and a `WARN` log.
  `poly lint --format json` never had this bug, so the two commands' JSON output was asymmetric.
  `FormatResult` now carries an `error` field and the format reporter emits a record for every
  errored file — a machine consumer must still check the exit code, not just the payload. Format
  failures now also echo to stderr under `json`/`toon`, matching what lint already did.

- **MCP consumers lost per-file errors.** The poly MCP server now carries a run-level `errors`
  array, a per-file `error` distinct from `skipped`, and sets `isError` on any run with errors.
  An empty findings list was previously not proof of a clean run — a file that failed to lint or
  format could vanish from the result with no signal at all.

- **`poly lint` no longer exits 0 for a file it could not lint.** A per-file engine failure was
  logged as a warning and the file was dropped from the results, so a run that failed to check
  something reported success. `poly fmt` already handled this through `FormatError`; `lint` now
  matches it.

  An engine **error** is deliberately a third category, kept distinct from a **skip**: a skip is poly
  correctly declining to act (no matching engine, a generated file), an error is poly failing.
  Conflating them would have rebuilt the same false pass in a new shape, and would have made
  `--deny-skips` fire on the wrong thing. Errored files do not count toward the skip budget, are
  never counted as checked, and are named individually:

  ```text
  error bad.py: stream did not contain valid UTF-8
  1 file(s) could not be linted and were NOT checked.
  ```

  Exit code is **2**, not 1: an errored file produced no findings, so it is an internal failure
  rather than a lint result. When any file errored, the headline reads `Lint did not complete.` —
  `Fixed N issue(s)` still reports what the run did, but no longer stands in as the verdict.

- **Markdown that merely documents template syntax is no longer skipped.** The Go/Helm template guard
  scanned raw file content, so any markdown containing `{{ … }}` — including inside a fenced code
  block or an inline code span — was excluded from formatting entirely. poly could not format its own
  CHANGELOG.

  Template syntax inside fenced blocks, indented code blocks, and inline code spans is now treated as
  documentation rather than as a template. Syntax outside them still triggers the skip, and YAML
  detection is unchanged, so a genuine Go-templated `Taskfile.yaml` is still skipped. An unterminated
  fence resolves toward skipping — leaving a template unformatted costs nothing, reflowing one
  destroys it.

- **`poly fmt` no longer changes which rows a SQL query returns.** It rewrote `where id = NULL` into
  `where id is NULL`. Those are not equivalent: under three-valued logic `= NULL` evaluates to
  UNKNOWN and matches nothing, while `IS NULL` matches every null — so a command whose job is
  whitespace and casing silently changed a query's result set. Worse, `= NULL` is almost always a
  real bug, and the "fix" destroyed the evidence of it. `AL05` deleting unused table aliases under
  `fmt` is the same shape.

  Same root cause as the Markdown heading fix below: `lint` and `format` did not own disjoint rule
  sets, so `format` applied every rule that carried an autofix. The line is now **presentation
  versus meaning**: `poly fmt` applies only the Layout and Capitalisation rule groups, which cannot
  change what a query does. Aliasing, Ambiguous, Convention, Jinja, References and Structure are
  reported by `poly lint` and never auto-applied. The format-side suppression is derived from
  sqruff's own registry, so a rule group added upstream is excluded from `fmt` until it is
  explicitly vetted in — an unreviewed rule can only make `fmt` do less.

  `CV05` is still **reported**; poly just no longer applies it for you.

- **Per-file hooks validated the worktree while workspace hooks validated staged content**, and
  nothing in the report said which. Both failure directions were reproduced against the shipped
  0.19.7 binary: a violation present only in the index was committed with exit 0, and a violation
  present only in the worktree blocked a commit whose staged content was clean. Every hook now runs
  from one execution root resolved once per run, and the stage banner names the tree it validated.

  Because a hook's rewrite lands in the snapshot, it is carried back to the worktree **only** where
  the worktree copy is byte-identical to the index. Where they differ the author is holding unstaged
  work, so the fix is withheld and a `stage_fixed` hook fails the run naming the files — rather than
  overwriting work the fix never saw, or `git add`ing hunks the author deliberately left out.

  Three accepted trade-offs ship with the wider scope (ADR 0019). A partially staged file (`git
  add -p`) is judged on its staged hunks alone — an unstaged hunk in the same file can no longer
  fail or pass the gate. A gitignored `poly.local.toml` is invisible to the snapshot, so local
  overrides that loosen hooks stop applying under `git commit`; `poly hooks run --all-files` is
  unaffected, and `[hooks] isolate = false` in the committed `poly.toml` restores worktree scoping.
  And a hook's own untracked side effects — logs, generated files outside its matched set — land in
  the snapshot and are discarded on the next refresh, so anything a hook is meant to deliver must be
  a file its own `files`/glob actually matches.

- **A hook that never returns no longer hangs the commit silently.** One wedged a consumer's
  pre-commit for 25 minutes with zero output, blocking four commits, with `--no-verify` the only way
  through. Hooks now run under a budget — 10 minutes per-file, 30 minutes whole-project — chosen as
  hang detectors rather than performance budgets, since killing a cold `cargo clippy` would be a
  worse defect than the hang. On overrun the process group is signalled and the hook is reported as
  **killed by poly**, which is a distinct fact from failing on its own merits. A liveness line names
  the running hook after 15s, and skipped, killed and setup-failed hooks now render with distinct
  markers — previously `-` meant both "skipped" and "still running".

  Set a per-job budget with `timeout` in `poly.toml` — whole seconds (`90`), a suffixed duration
  (`500ms`, `30s`, `10m`, `1h`), or `0` / `off` / `none` to disable. Stage and hook `before` / `after`
  steps and `precondition` probes are bounded too, at 10 minutes and 60 seconds respectively; an
  unbounded stage `before` reproduced the same hang one level up. A malformed value names the hook
  it came from — "invalid timeout" in a repo with thirty hooks is not a diagnosis.

  Resolution is **environment override → `timeout` in config → shape default**. The environment wins
  deliberately: it is the escape hatch of whoever is actually running the hooks, and they are the only
  party who knows how fast that machine is — a config author cannot, and an operator often cannot edit
  the config at all (a CI checkout, a fork). An unparseable environment value is ignored with a
  warning rather than read as "disabled", since failing open on a typo would silently reinstate the
  hang the mechanism exists to bound.

- **`poly lint --fix` no longer deletes single lines out of the middle of a prose comment block.**
  The opt-in `[lint.uncomment]` backend judged each comment *line* independently, and tree-sitter
  reports one comment node per `//` or `#` line — so one line of a paragraph could be classified as
  dead code and removed while its neighbours survived, welding the remainder into a sentence the
  author never wrote. `::` is a code operator, and the prose guard required six word-shaped tokens;
  backticked and parenthesised words did not count, so ordinary technical prose scored just under the
  bar.

  The unit of judgement is now the contiguous comment **block**, never the line. A block is removable
  only when every line in it is covered by a removal — a `TODO` or `~keep` line inside vetoes the
  whole block — **and** every line reads as code. A partial-block edit is no longer merely unlikely,
  it is unrepresentable. A separate guard covers the standalone case, where a one-line English comment
  naming a `::` path has no block to belong to.

  The deliberate trade: a block mixing prose and code is now kept whole, so poly will miss some real
  commented-out code. A missed removal is cosmetic; a wrong removal destroys what the author wrote and
  looks like nothing happened.

- **`poly lint --fix` reported `No issues found.` while rewriting files.** The runner applied fixes,
  re-linted, and summarised only what *remained* — discarding the fact that it had changed the file at
  all. Check mode was always correct; only `--fix` was silent. This is why the comment deletion above
  went unnoticed in a consumer repository: the tool reported doing nothing while doing something. The
  summary now names what was fixed.

- **Remote hook sources no longer duplicate the consumer's cargo builtins.** Each `[[hooks.sources]]`
  entry was lowered through a synthetic config carrying `present = true`, and the cargo builtin group
  is default-on whenever a `[hooks]` section exists — so every source re-emitted
  `cargo-clippy`/`cargo-sort`/`cargo-machete`/`cargo-deny` under its own `{source}:` prefix. Each
  configured source added a full redundant pass over the workspace. In this repository the pre-commit
  stage went from 14 hooks to 10, and from 1m29s to 44s.

  The duplication was `PATH`-probed, so a machine missing `cargo-sort` or `cargo-deny` saw fewer
  duplicates than one with the full toolchain installed.

- **`poly fmt` no longer rewrites a document's heading structure.** `lint` correctly filtered out the
  formatting-category rules that `fmt` owns, but `format` ran the **entire** rule set through the fix
  coordinator — so `poly fmt --fix` applied any rule that happened to carry an autofix, whatever it
  changed. The two paths were never a clean partition.

  Concretely, MD001 (heading increment) demoted `### Go` to `## Go` in a document beginning with an
  `h1`, silently changing which sections nest under which. The violation is real, but the repair is
  ambiguous — demote the `h3`, or insert the missing `h2` above — and those produce different
  documents. Structural rules are now excluded from the format path, and their autofix is dropped from
  `lint` as well, since `poly lint --fix` applies diagnostic edits and would otherwise restructure the
  outline by the other route.

  MD001 is still **reported** by `poly lint`; poly just no longer guesses the repair. Anyone relying on
  `--fix` to normalise heading levels now gets an error to resolve by hand instead.

- **A heading whose text ends in `#` no longer loses that `#`.** rumdl's MD020 treated a trailing run
  of `#` as a closing sequence even with nothing separating it from the heading text, so `### C#`
  became `### C #`; MD003 then normalised that to `### C` in any document whose dominant style is open
  ATX. CommonMark §4.2 requires a closing sequence to be preceded by whitespace, so `### C#` is an
  ordinary heading whose text is `C#` and there was never anything for MD020 to report.

  Wider than the reported C# case: it fired on any ATX heading at any level ending in `#`, through
  both `poly lint --fix` and `poly fmt --fix`. It also **self-reverted** — a docs pass would repair
  the heading and the next `--fix` would undo the repair — which is why it survived unnoticed. Only
  the `### C\#` escape avoided it, because rumdl's pattern excludes a preceding backslash.

  The wrapped rule is now guarded for lines the author wrote verbatim, which keeps the legitimate
  `##Foo##` → `## Foo ##` fix alive; the diagnostic is suppressed too, since the construct is valid
  CommonMark and a permanent unfixable warning on every C# heading would just push people back to the
  escape. An upstream patch is prepared — this guard is what makes consumers safe before it lands.

- **Templates that do not render markup are no longer reformatted.** `poly fmt --fix` on a `.jinja`
  template rendering Go rewrote

  ```go
  data, err := json.Marshal(r)
  if err != nil {
  ```

  into `json.Marshal(r) if err != nil`, concatenated the closing braces, and emitted source that does
  not compile — 30 sites in one consumer's Go binding, which broke a downstream build. markup_fmt
  reflows on the assumption that whitespace is insignificant, which holds for HTML and not for most
  other languages, and the extension `.jinja` names the *template syntax*, never the *rendered*
  language.

  Jinja, Vento and Mustache are now formatted only when the target is actually markup: a double
  extension names it outright (`marshal.go.jinja` is not markup, `page.html.jinja` is), and otherwise
  the body must contain a literal tag once `{{ … }}`, `{% … %}` and `{# … #}` constructs are
  stripped — so a comparison inside a template expression is not mistaken for a tag. Declining is the
  safe default: leaving a template unformatted costs nothing, reflowing one destroys it. Dedicated
  markup languages (`.html`, `.vue`, `.svelte`, `.astro`, `.xml`) are unambiguous and unaffected.

- **A staged-snapshot file rewritten out of band is repaired instead of served forever.** The
  snapshot treated a path as current when its index OID was unchanged and the file existed, so
  anything that rewrote a snapshot file in place without changing an OID — a `workspace = true` hook
  running in fix mode, or a truncating crash — was served on every later run, permanently. That is a
  false pass: the fix never reaches the index, and every subsequent check-only run passes on bytes
  that are not staged. The manifest now records size and mtime alongside the OID.

- **Upgrading poly no longer leaves a window where the binary is absent.** `install.sh` used
  `install -m 0755` with a `cp` fallback; all of those keep the destination inode and rewrite in
  place, so the binary was partial or non-executable for the duration. Because poly's git hooks are
  installed globally and fail closed, that blocked **every commit in every repo on the machine**
  while an upgrade ran. Both installers now stage to a temporary path and rename over the
  destination, which is atomic within a filesystem.

  The git-hook shim also no longer assumes a missing install when poly is being installed *right
  now*: it looks for a staged or partially-written poly beside the destination and says so, and
  separately detects an installed poly whose directory is not on the `PATH` the hook sees. It still
  fails closed — a hook that silently skips when its binary is missing would be the exact failure
  this release exists to remove.

- **`uncomment --fix` no longer deletes prose that continues across lines.** A comment ending
  mid-clause — typically on `;` — was classified as commented-out code, so `poly lint --fix`
  removed it. In a reported Helm values file this deleted

  ```yaml
  # Chromiumoxide engine still gets built because browserEndpoint is set;
  ```

  from the middle of a four-line paragraph, leaving text that still reads as valid English with its
  explanation silently gone — data loss that passes CI and survives review. `code_only` judged prose
  by its closing character alone; a long, overwhelmingly word-shaped line is now treated as prose
  regardless of how it ends. Genuinely commented-out code ending in `;` is still reported.

- **`poly fmt` no longer reports a file it declined to inspect as one it verified.** The summary
  counted every discovered file as "scanned", including files a backend skipped on purpose — YAML
  carrying Go/Helm template actions, and (see below) templates that do not render markup. A skipped
  file and a checked-and-clean file produced byte-identical output:

  ```text
  $ poly fmt --check Taskfile.yaml     # skipped: contains {{.CLI_ARGS}}
  All formatted. (1 file(s) scanned)
  $ poly fmt --check plain.yaml        # actually checked
  All formatted. (1 file(s) scanned)
  ```

  The only signal was an `INFO` log that is invisible at default verbosity *and* absent on a cache
  hit, so a warm re-run had no signal at all. One consumer concluded from this that their templated
  YAML was covered when it was being skipped. The summary now separates the two and names the cause:

  ```text
  All formatted. (249 file(s) checked, 2 skipped (Go/Helm template syntax))
  ```

  Backends declare this through a new defaulted `Engine::skip_reason` hook, so the runner can count
  skips rather than each backend silently returning `Unchanged`.

- **A path argument that does not exist now fails the run instead of reporting success.**
  `poly fmt --check typo.py` printed `All formatted. (0 file(s) scanned)` and exited 0 — a green
  result that verified nothing. A mix of real and missing paths was worse: only the real ones were
  checked, the run still exited 0, and the file count looked plausible, so a hook or CI step feeding
  poly a stale path list was indistinguishable from a passing gate. Every unresolvable path is now
  named on stderr and the run exits 2. Reported independently by `html-to-markdown` (13 paths,
  0 scanned, exit 0) and arbitrated against `crawlberg`'s non-reproduction — the variable was path
  existence, not how many paths were passed.

- **A Rust inner attribute is no longer read as a shebang.** A `.rs` file starting with
  `#![deny(...)]`, `#![allow(...)]` or `#![no_std]` was treated as a script, which flagged every
  generated binding crate in `file_safety`'s shebang checks (~133 files in one reporting repo) and
  mistagged files during hook file-type identification. Both call sites now look one byte past
  `#!`; `[` is not a valid interpreter path in any language, so the check is applied generally
  rather than special-cased to `.rs`.

- **Documentation corrections that were themselves defects.** Every example in `ACTION_USAGE.md`
  referenced `Goldziher/poly@v1`, which does not exist — only `v0` does — so anyone copying them got
  a "ref not found" failure. The README also presented verify-after-the-fact as the general way to
  pin poly, when in fact the installer (`POLY_VERSION`), the GitHub Action (`version:`) and
  `cargo binstall` all support a true, checksum-verified pin; Homebrew is the only channel that
  cannot, and the README now says so plainly. An unpinned-install one-liner had already been copied
  verbatim into eight sibling repositories, so published examples are treated here as a defect
  surface rather than prose.

- **The Homebrew publish step no longer exits 0 when `HOMEBREW_TOKEN` is unset.** It skipped the
  tap update and reported success, so an unset secret would have left `brew install` serving the
  previous release indefinitely with nothing in the run to say so. The tap has in fact tracked
  every release, so no published version was affected — this closes a latent hazard rather than a
  live one. The step now fails the job with `HOMEBREW_TOKEN is not set`, so a missing secret
  blocks the release instead of quietly leaving Homebrew behind.

- Adapted the OXC backend to two upstream API changes in oxc `c42d639`:
  - `oxc_formatter::format` lost its trailing session argument; poly now calls the four-argument
    service-less form, which is the compatibility wrapper for exactly this use.
  - `oxc_linter::Message::rule` was removed. The rule identity now travels on the diagnostic's
    `OxcCode` (`scope` = plugin display name, `number` = rule name), so poly maps that instead.
    Reported codes are unchanged in shape — bare `no-debugger` for `eslint` rules, `oxc/const-comparisons`
    for plugin-scoped ones. Note that plugin-scoped codes now use oxlint's *normalized* plugin display
    name, so the three plugins oxlint renames (`jsx_a11y` → `jsx-a11y`, `react_perf` → `react-perf`,
    `nextjs` → `next`) report under the hyphenated form; adjust `[per-file-ignores]` entries keyed on
    the old spelling.

## [0.19.7] - 2026-08-12

### Fixed

- Tier-one formatters now retain exclusive ownership of their languages when an overlapping catalog
  formatter is enabled. This prevents tools such as `clang-format`, enabled for C-family files, from
  running after OXC and rewriting JavaScript, CJS, MJS, or TypeScript with C/C++ spacing rules.

## [0.19.6] - 2026-08-06

### Fixed

- A configured catalog formatter now displaces poly's generic tree-sitter reindenter instead of
  chaining with it. Formatters run in sequence and the whole chain repeats to a fixed point, so a
  language served by both poly's fallback reindenter *and* an external formatter had the two fighting
  over indentation on every pass. This is what kept Elixir from converging: even with `[tools.mix]`
  declared, poly reindented the file and `mix format` reindented it back. The fallback is dropped only
  when the catalog tool's binary is actually on `PATH`, so a configured-but-missing tool leaves the
  fallback in place rather than silently dropping all formatting for the language.
  (`crates/poly-core/src/runner.rs`, `crates/poly-core/src/engine.rs`,
  `crates/poly-core/src/engines/catalog_tool/mod.rs`)

  Elixir projects should now declare the formatter that owns the language:

  ```toml
  [tools.mix]
  enabled = true
  root = "packages/elixir"
  ```

## [0.19.5] - 2026-08-06

### Fixed

- Elixir formatting no longer strips indentation, and no longer fights `mix format`. Elixir has no
  native backend and `tree-sitter-elixir` ships no `indents.scm`, so it runs on poly's built-in
  `ELIXIR_INDENTS` query — which modelled only `do…end` and `fn…end`. A file containing neither, such
  as a top-level `%{}` map, produced zero captures, and `emit_reindented` still trimmed every line and
  re-emitted it at level 0: a `mix format`-formatted checksum map came back flattened to column 0, and
  the two formatters then oscillated forever. Every Elixir package in the xberg-io polyrepo had drifted
  this way and failed `mix format --check-formatted`. Two changes: `try_reindent_builtin` now returns
  `None` when the query captures nothing at all, so a file poly has no structural model of falls
  through to whitespace normalization instead of being flattened; and `(map)`, `(list)`, `(tuple)` and
  `(bitstring)` are tagged `@indent.auto` so their interiors are emitted verbatim. They are
  deliberately not `@indent` — `mix format` aligns a wrapped `=>` continuation at +4 under a +2 entry,
  which a level-counting model cannot express, so `@indent` would only shrink the oscillation rather
  than end it. Existing Elixir fixtures were all `do…end`/`fn…end`, the only constructs the query
  modelled, so the idempotency tests passed vacuously and never caught this.
  (`crates/poly-core/src/engines/treesitter/indent.rs`)

## [0.19.4] - 2026-08-06

### Fixed

- `poly fmt --fix` and `poly lint --fix` no longer strip a file's permissions. Both write through
  the same `write_atomic` helper, which creates a sibling temp file and renames it over the target.
  The temp file is born with `0666 & !umask` and has no relationship to the file being rewritten, so
  the rename replaced a `0755` script with a `0644` one — formatting an executable script silently
  cleared its exec bit, which is how `publish-npm/scripts/publish.py` in `xberg-io/actions` started
  failing ruff's `EXE001`. The original mode is now applied to the temp file before the rename.

## [0.19.3] - 2026-08-05

### Fixed

- `extends` path validation means the same thing on every host. The guards that keep an
  `extends` source's `file` inside the base checkout were written against `std::path`,
  which parses per-platform: `/etc/passwd` is rooted but *not absolute* on Windows, so
  `is_absolute()` accepted it there, while `C:\Windows\system.ini`, `\\server\share\x`
  and `a\..\b` are an ordinary filename and a single component on Unix and were accepted
  here. `file` is portable config resolved inside a checkout, so it is now inspected as a
  string: POSIX roots, UNC roots and drive qualifiers (including the drive-relative `C:x`)
  are rejected everywhere, as is a `..` segment separated by either slash. This was
  failing `ci` on the windows runner.

## [0.19.2] - 2026-08-05

### Fixed

- `typos` no longer flags lint rule codes. Linter configuration lists them by the hundred —
  ruff `lint.select`/`lint.ignore` and per-file-ignores, `# noqa:` suppressions, rumdl
  `disable` — and the dictionary read the short uppercase ones as misspelled acronyms
  (`CPY` → `COPY`/`CPU`), so every repo carrying such a config had to allowlist rule codes
  by hand. An identifier of at most five uppercase letters followed by at most five digits
  (`CPY`, `CPY001`, `S310`, `PLR0917`, `ASYNC230`, `MD012`) is now treated as valid. The
  shape stays narrow: a longer uppercase run, or any token containing a separator, is an
  ordinary identifier and is still spell-checked.
- The `typos` and `treesitter` engines report the dependency versions they actually build
  against. `typos-dict` 0.13.31 → 0.14.0 and `tree-sitter-language-pack` 1.13.3 → 1.14.0
  were bumped without updating either engine's `version()`, so cached results produced by
  the old dictionary and the old grammars were served as fresh. `version_audit` was failing
  on `main` for both.

## [0.19.1] - 2026-08-04

### Fixed

- `java` and `csharp` are no longer tree-sitter bracket-reindented. Their grammars/external
  scanners produce platform-dependent CSTs, which caused nondeterministic formatting output
  across macOS and Linux. Both languages now use deterministic whitespace normalization instead.

## [0.19.0] - 2026-08-01

### Added

- New `crates/poly-workspace` crate: the whole-project lint orchestration (`cargo clippy` /
  `cargo-sort` / `cargo-machete` / `cargo-deny`) is extracted out of `poly-cli` into a shared
  library (`run_workspace_lint`, `render_workspace_outcome`) with a narrow public API, consumed
  by both `poly-cli` and the new MCP workspace tools with no dependency cycle. `poly lint`'s CLI
  output is byte-identical to before.
- The MCP server (`poly mcp`) gained a broader tool surface: `rules` (list/test the custom
  ast-grep rule packs, read-only) and `config_show` (effective merged config, read-only) join the
  existing read-only `lint` / `format_check` / `cache_stats`, alongside the mutating `lint_fix` /
  `format_write` / `cache_clean`. Two new tools, `workspace_lint` and `workspace_lint_fix`, expose
  the whole-project phase as async Tasks (rmcp's `TaskManager`; poll `tasks/get`, cancel with
  `tasks/cancel`) so a multi-minute `cargo clippy` run doesn't block the call; a client that
  doesn't declare the tasks capability gets a synchronous (blocking) result instead. Every tool
  now returns typed structured content (`CallToolResult.structured_content`, with a derived JSON
  schema) in addition to a JSON or compact TOON text block, selectable per request via a `format`
  parameter. Transport stays stdio-only.
- poly now publishes its own Claude/Codex plugin and ai-rulez marketplace (`Goldziher/poly`),
  registering `poly mcp` as a stdio server plus 5 skills and 2 slash commands that teach an agent
  to use poly as its lint/format orchestrator. Install with `/plugin marketplace add
  Goldziher/poly` then `/plugin install poly@poly` (Claude); the Codex manifest lives at
  `.codex-plugin/plugin.json`. The plugin version is lock-step with the workspace version, bumped
  via `scripts/release-bump.sh`.

### Changed

- Bumped the pinned `ruff` (0.16.1, `80790b3`), `oxc` (`65fe65d`, `oxc_formatter` 0.61 /
  `oxc_parser` 0.142), and `biome` (2.5.6, `1139f1c`) git dependencies to their latest revisions;
  `rubyfmt` was already current. Adjusted for one upstream API rename
  (`Diagnostic::primary_message` → `concise_message`). The affected engine cache keys
  (`version()`) were bumped so upgraded output is re-run.

## [0.18.3] - 2026-07-29

### Fixed

- `poly fmt` is a pure formatter again: it no longer runs the whole-project lint phase. Under `--fix` it had been
  invoking `cargo clippy`/`cargo-sort`/`cargo-machete`/`cargo-deny` — linting, not formatting. That phase now runs
  only under `poly lint --fix` (and the commit gate). The `--no-workspace` flag is removed from `poly fmt`, since it
  only gated the phase that no longer runs.

## [0.18.2] - 2026-07-29

### Fixed

- `poly commit` now resolves remote git `extends` bases, matching `poly lint`/`poly fmt`. Previously the
  commit-message linter loaded `poly.toml` through the network-free resolver and hard-failed with "remote git
  extends source requires the poly CLI resolver" whenever a repo inherited its shared base from a pinned remote
  (rather than a local sibling path), breaking the commit-msg hook. The CLI now passes its remote resolver into
  gitfluff so the repo-local `[commit]` rules load regardless of where the base lives.

## [0.18.1] - 2026-07-29

### Fixed

- The result cache no longer serves stale output after a `poly` upgrade. poly's own version is now folded into
  every cache key, so a new binary can never reuse a predecessor's cached lint/format result — closing a
  non-idempotency where `poly fmt --fix` left a file that a fresh (`--no-cache`) format would still change, because
  an engine's hand-maintained `version()` had not moved even though the binary's output had. The trade-off is one
  re-run of otherwise-cached work after each upgrade.
- A `poly lint`/`fmt`/`hooks` run now self-heals an incompatible on-disk cache layout (wiping the entry tree when
  the `VERSION` sentinel is stale) rather than only under `poly cache gc`. The read-only `poly cache stats`/`size`
  maintenance commands stay non-destructive.

### Changed

- `poly lint --fix` and `poly fmt --fix` now run the whole-project / interop tools in **fix mode** instead of
  check-only: `cargo sort` sorts in place (drops `--check`), `cargo-machete` gains `--fix`, and `cargo clippy` runs
  `--fix --allow-dirty --allow-staged` (preserving `-- -D warnings` and any `clippy_args` override); `cargo deny`
  has no autofix and stays check-only. Previously these always ran in check mode, so `--fix` reported the findings
  but never applied them and they reappeared on every run. `poly fmt` gains a `--no-workspace` flag and only runs
  the whole-project phase under `--fix`, so `poly fmt --check` stays a fast, pure formatter. The git-hook /
  commit-gate path is unchanged and remains check-only.

## [0.18.0] - 2026-07-26

### Added

- `poly.toml` can now declare a top-level `extends` list to inherit any config section (`[discovery]`,
  `[lint.*]`, `[fmt.*]`, `[tools.*]`, `[per-file-ignores]`, `[hooks.*]`, `[defaults]`, …) from local and pinned
  remote base configs, reusing the `path`/`git`/`revision` vocabulary of `[[hooks.sources]]`. Bases are
  deep-merged beneath this `poly.toml`, and `poly.local.toml` still wins on top. A symbolic git ref (branch or
  tag) requires the new `poly config update` subcommand to resolve and pin it into `poly-config.lock`; `poly
  config resolve`/`show` prints the effective merged config. See [ADR 0020](adrs/0020-shared-remote-configuration.md).

### Fixed

- `uncomment` no longer false-positives on comments that are not commented-out code. A new `code_only` option
  (default `true`) keeps a removal only when the comment lexes as code in the file's language, so machine-generated
  headers (`# alef:hash:…`, `Re-generate with:`, `DO NOT EDIT`), multi-line English NOTE blocks, and `key = value`
  directive comments are left alone while genuine commented-out code (`// let x = foo();`, `# print("debug")`) is
  still reported. Set `code_only = false` to restore the previous strip-every-comment behaviour. The engine cache
  key was bumped so upgraded output is re-run.
- `poly fmt --fix` now converges to a fixed point in a single invocation. Each file's format-engine chain is re-run
  (bounded, up to 5 passes) until the content stops changing, so a following `poly fmt --check` is clean even when
  an underlying formatter (clang-format, csharpier, google-java-format) is not idempotent on the first pass.

## [0.17.1] - 2026-07-24

### Changed

- Bumped the pinned `oxc`, `ruff`, and `biome` git dependencies to their latest revisions and refreshed the
  `uncomment` backend to 3.5.1. The affected engine cache keys (`version()`) were bumped so upgraded output is
  re-run, and stale `rumdl`/`tree-sitter-language-pack` version markers left by the previous dependency refresh
  were corrected.
- Homebrew now ships bottles. The `poly` formula is generated as a source build, so the tap's centralized
  bottle pipeline compiles and attaches prebuilt bottles for macOS and Linux; `brew install Goldziher/tap/poly`
  pours a bottle once built. The `curl | sh` and PowerShell installers continue to use the prebuilt release
  binaries.

## [0.17.0] - 2026-07-21

### Added

- The whole-project lint phase now preserves tool colours. When `poly lint`'s own output is a colour-capable terminal,
  captured `cargo clippy` / `cargo-deny` / type-checker diagnostics keep their ANSI colouring instead of being stripped;
  redirected or `--no-color`/`NO_COLOR` output stays plain.

### Changed

- Opinionated default-policy audit across the wrapped tools:
  - MDX (`.mdx`) files no longer report `MD033` (inline HTML), `MD036` (emphasis-as-heading), `MD041` (first-line
    heading), or `MD051` (link fragments) — all noise against JSX/ESM content and toolchain-generated anchors. Plain
    `.md` is unchanged, and any rule is re-enableable via `enable`.
  - `typos` findings are now **warnings** rather than errors, so a single dictionary false positive no longer fails CI
    (`poly lint` exits non-zero only on errors).
  - `ruff`'s flake8-bugbear `B008` is disabled by default — it false-positives on the FastAPI / typer `Depends()` /
    `Query()` argument-default idiom — while the rest of bugbear (`B006`, …) stays on. Re-enable with
    `extend_select = ["B008"]`.
  - TOML now formats with a 2-space indent (matching YAML/JSON and taplo's own default) instead of 4, and the `taplo`
    formatter honours the global `line_length` instead of a hardcoded 120.

### Fixed

- `poly lint` no longer reports a false `parse-error` on valid JSONC (`.jsonc`) files that use trailing commas —
  including the trailing commas poly's own JSONC formatter emits. Strict `.json` still rejects them, and genuine JSONC
  syntax errors are still reported at their correct position.

## [0.16.0] - 2026-07-21

### Added

- MDX (`.mdx`) formatting. `.mdx` files are now discovered and routed through the `rumdl` backend with its MDX flavor
  enabled, preserving imports and JSX while normalizing Markdown.

### Changed

- Bumped the pinned `biome`, `oxc`, `ruff`, and `rubyfmt` git dependencies to their latest revisions and ran
  `cargo upgrade --incompatible`. Affected engine cache keys were bumped so upgraded output is re-run.
- The YAML and Markdown/MDX backends now skip files that contain Go/Helm template syntax — detected by scanning file
  content for template actions (`{{ … }}`, `{{- … }}`, `{{/* … */}}`), not by filename. GitHub Actions `${{ … }}`
  expressions and MDX/JSX object literals are not treated as templates. Each skip emits an info-level message.
- Config resolution now respects a repo-root `poly.toml` when poly is invoked from a subdirectory: it cascades and
  deep-merges every `poly.toml` (and sibling `poly.local.toml`) from the git repo root down to the working directory,
  and re-anchors exclude globs so they still match relative to the walk root.

### Fixed

- `poly hooks install` now installs shims only for configured hook types and prunes stale shims, and no longer prints an
  empty `[stage]` banner for a stage that runs no jobs — so `git commit -n` stays quiet.

## [0.15.5] - 2026-07-20

### Changed

- Bumped the `uncomment` backend to 3.5.0, which extends a `~keep` marker across a whole contiguous block of
  standalone single-line comments instead of preserving only the marked line. A multi-line rationale comment now
  survives with a single `~keep` rather than one per line. The engine cache key was bumped to re-run `uncomment` on
  upgrade.

## [0.15.4] - 2026-07-12

### Fixed

- Prevent `poly hooks` (and `poly fmt`/`poly lint`) from intermittently hanging: the native-tool and catalog-tool
  stdin backends now always feed a child's stdin from a dedicated thread while draining its stdout/stderr, instead of
  writing small inputs inline before the drain. A wrapped tool that emitted output while still reading stdin could fill
  its output pipe and deadlock against the blocked writer.

## [0.15.3] - 2026-07-12

### Changed

- **Dependency refresh.** Bumped `rmcp` (2.1 → 2.2), `saphyr` (0.0.9 → 0.0.11), `uncomment` (3.2 → 3.4), and
  `memchr` (2.8.2 → 2.8.3), along with transitive crates.io updates.

## [0.15.2] - 2026-07-12

### Fixed

- Allow the intentional Windows read-only attribute reset used to rebuild invalid cached hook checkouts.

## [0.15.1] - 2026-07-12

### Fixed

- Resolve simple `command -v <binary>` hook path checks portably on Windows.

## [0.15.0] - 2026-07-12

### Changed

- Share URL-keyed Git hook mirrors and immutable commit checkouts globally under the XDG cache, with per-source interprocess locking.

## [0.14.0] - 2026-07-12

### Changed

- Declare explicitly selected local-path and Git hook catalogs under `[[hooks.sources]]` in `poly.toml`; `poly-hooks.toml` is now producer-only.
- Select guarded producer execution paths using machine-local channel preferences from `poly.local.toml`.

## [0.13.0] - 2026-07-12

### Added

- Load executable local-path and Git hook sources from `poly-hooks.toml`, lock remote revisions, and refresh them with `poly hooks update`.
- Install missing hook toolchains on install or first run using machine-local channel preferences from `poly.local.toml`.

## [0.12.0] - 2026-07-09

### Added

- **Opt-in `uncomment` comment-removal lint backend.** A new cross-cutting lint
  engine (like `typos`) that strips comments across every language it recognizes,
  wrapping the pure-Rust [`uncomment`](https://crates.io/crates/uncomment) crate
  (tree-sitter based). Each removable comment is reported as a **warning** (which
  never fails CI) carrying a delete-edit, so `poly lint` surfaces them and
  `poly lint --fix` removes them. **Off by default**; enable and tune it with a
  language-agnostic `[lint.uncomment]` block plus optional per-language
  `[lint.<lang>.uncomment]` overrides (`enabled`, `remove_todos`, `remove_fixme`,
  `remove_docs`, `use_default_ignores`, `preserve_patterns`). Preservation rules
  keep shebangs, `~keep`, TODO/FIXME, documentation, and user patterns; a language
  `uncomment` does not recognize is left untouched.

## [0.11.0] - 2026-07-08

### Added

- **Per-group `[hooks.builtin.cargo] lint = false`.** Keep the cargo group
  (`clippy`/`sort`/`machete`/`deny`) as a `pre-commit` gate while excluding it
  from the whole-project phase of `poly lint`. Where `[lint] workspace = false`
  disables that phase wholesale, this opts out a single builtin — useful when a
  lightweight `poly lint` (e.g. a CI `validate` job with a plain checkout) cannot
  compile the workspace, but a properly provisioned job still runs clippy. The
  underlying `Hook::skip_in_lint` flag drops a hook from `poly lint`'s workspace
  phase without affecting git-hook runs.

### Fixed

- **Windows-correct staged snapshots.** The staged-content snapshot that isolates
  whole-workspace hooks now normalises CRLF and directory symlinks, so hooks that
  compile or analyse the tree behave the same on Windows as on Unix.

## [0.10.0] - 2026-07-07

### Added

- **`poly lint` runs whole-project tools.** After its per-file tier, `poly lint`
  now runs the same whole-workspace analysis tools a `pre-commit` hook would —
  `cargo clippy` / `cargo-sort` / `cargo-machete` / `cargo-deny` and any
  configured whole-project jobs (e.g. type checkers) — on the live worktree,
  folding their pass/fail into the report and the exit code. It reuses the
  existing `[hooks.builtin.cargo]` + inline `workspace = true` config as the
  single source of truth, so `poly lint` surfaces the same findings a commit
  would. On by default; opt out with `--no-workspace` or `[lint] workspace =
  false`. A repo with no `[hooks]` section runs only the per-file tier. With
  `--format json`/`toon` the whole-project section is written to stderr (stdout
  stays a single valid document), so machine consumers must check the exit code.
- **Animated `poly hooks` progress.** An interactive `poly hooks` run now shows a
  live spinner per concurrently-running hook with a rolling output preview,
  collapsing to a `✓/× id (duration)` line as each finishes. Non-interactive
  runs (CI, pipes) keep the deterministic, quiet report unchanged.

### Fixed

- **Security: bump `crossbeam-epoch` to 0.9.20** (RUSTSEC-2026-0204 — invalid
  pointer dereference in the `fmt::Pointer` impl for `Atomic`/`Shared`). Dropped
  the now-obsolete `quick-xml` advisory ignores (RUSTSEC-2026-0194/0195); the
  pinned ruff rev now resolves `quick-xml 0.41.0`, which is unaffected.

## [0.9.0] - 2026-07-06

Alignment release: `poly` is now the single brand for everything you type or run.
The GitHub repository moved to [`Goldziher/poly`](https://github.com/Goldziher/poly)
(old URLs redirect).

### Changed — breaking

- **Built-in hook keys renamed.** `[hooks.builtin] polylint` / `polyfmt` are now
  `lint` / `fmt`. The old keys are rejected — update `poly.toml` (e.g.
  `[hooks.builtin] lint = true`).
- **`polylint.toml` is no longer read.** Only `poly.toml` (plus the
  `poly.local.toml` override) is discovered. Rename any remaining `polylint.toml`.
- **Cache moved out of the repo.** The result cache and hook staged-snapshot now
  live in the per-user cache directory (`~/.cache/poly/<repo-key>` on Linux,
  `~/Library/Caches/poly/…` on macOS, `%LOCALAPPDATA%\poly\…` on Windows) instead
  of the in-repo `.polylint/` folder — so nothing poly-generated lands in the
  working tree. A legacy `.polylint/` directory is auto-removed on the next run.
  `POLY_CACHE_HOME` overrides the base; `[cache] dir` still pins an explicit root.

### Changed

- **Internal crate `polylint-core` renamed to `poly-core`** (every workspace
  crate now uses the `poly-` prefix). Visible only via `RUST_LOG` targets.
- **Homebrew formula renamed** `polylint` → `poly`: install with
  `brew install Goldziher/tap/poly`.
- Logo and README refreshed to the `poly` wordmark and branding.

### Removed

- **npm and PyPI wrapper packages are discontinued.** poly is now distributed via
  the `curl … | sh` / PowerShell installer, the GitHub Action, and the Homebrew
  tap only. Prebuilt release binaries are unchanged; if you installed the `poly`
  command through `@nhirschfeld/polylint` (npm) or `polylint` (PyPI), switch to
  the installer or `brew install Goldziher/tap/poly`.

## [0.8.0] - 2026-07-06

### Added

- **Custom-rule tier.** Write your own lint rules — and codemods — as
  [ast-grep](https://ast-grep.github.io) YAML, in any of the 300+ languages poly
  can parse. Custom rules run in-process alongside the native backends on every
  `poly lint`, and `poly lint --fix` applies any `fix:` rewrites they declare. No
  plugin, no fork, no extra toolchain: rules run on the same tree-sitter grammars
  poly already bundles. Point `[rules] dirs` at one or more rule directories
  (default `[".poly/rules"]`); each rule is a standard ast-grep document whose
  `language:` field names a grammar.
- **`poly rules test` / `poly rules list`.** Verify rules against companion
  `<name>-test.yml` snippets (`valid` must not match, `invalid` must) and list the
  discovered rules. `poly rules test` exits non-zero on any failed snippet.
- **`fixed:` rule-test assertion.** An `invalid` test case may be a
  `{ code, fixed }` table that asserts the rule's applied autofix output, not just
  that the rule fires.

### Fixed

- **`[rules] dirs` resolve relative to the config file**, not the process working
  directory, so a rule set is found from any subdirectory.

### Changed

- **Dependency refresh.** Bumped `saphyr` (0.0.6 → 0.0.9).

## [0.7.0] - 2026-07-05

### Changed

- **Misspellings are now reported as errors and are never autofixed.** The
  `typos` backend previously emitted `warning`-severity findings with a
  single-correction autofix. Auto-correcting a typo silently rewrites
  identifiers, string keys, and API names that only *look* misspelled — a
  frequent source of regressions — so a typo is now surfaced at `error` severity
  (it fails `poly lint`) with the dictionary suggestion in the message, and
  carries no autofix. Resolve typos by hand (or allow-list the word).
- **Formatting rules no longer leak into `poly lint`.** rumdl's `Whitespace`
  category (line length `MD013`, trailing spaces, hard tabs, blank-line runs,
  final newline) is a `polyfmt` concern, yet every such rule also surfaced as a
  `poly lint` finding — flooding lint with formatting noise the linter cannot
  act on. `poly lint` now suppresses the `Whitespace` category and reports only
  structural / content findings (broken links, heading structure, unused
  references); `poly fmt` still owns and fixes the formatting rules.
- **Whole-project type-checkers are no longer wired into the per-file catalog
  lint tier.** `pyrefly`, `mypy`, `ty`, and the like resolve imports across the
  whole project and infer an import root from the project layout, which the
  per-file, exit-code catalog tier cannot provide — every cross-module import
  became a spurious `missing-import`. They are now refused as catalog linters
  (with a one-time warning); run them as a dedicated whole-project step instead.
- **Dependency refresh.** Bumped the `oxc` / `ruff` / `biome` git dependencies to
  their latest upstream commits and freshened crates.io dependencies (`rumdl`
  0.2.28, `typos-dict` 0.13.31, `tree-sitter` 0.26.10, `tree-sitter-language-pack`
  1.12.4, and others).

### Fixed

- **Catalog linters run against the real file on disk, not a temp copy.** A
  catalog-tier linter (e.g. `shellcheck`, `actionlint`) was fed a temp copy of
  the source, which destroyed project context: a Python type-checker could not
  resolve sibling modules or the project virtualenv, and `actionlint` no longer
  saw a `.github/workflows/` path. Read-only linting now runs against the real
  file whenever its on-disk content matches what is being linted, falling back to
  a temp copy only when they diverge (e.g. a re-lint after an in-memory fix).
- **`poly hooks` whole-workspace snapshot now materializes git submodules.** The
  staged snapshot is built with `git checkout-index`, which writes only blob
  entries — a submodule gitlink left *no* content, so a compile hook that reached
  into a submodule (e.g. a test that `include_bytes!`es a fixture from one) failed
  to build in the sandbox even though the real tree compiles. Each populated
  submodule is now exposed in the snapshot as a symlink into the live worktree, so
  compile-time references resolve.
- **Built-in `typos` allow-list for ubiquitous technical terms.** Common,
  always-correct tokens the dictionary otherwise flags — established
  abbreviations (`ser`, `flate`, `fpr`, `arange`, `unparseable`) and well-known
  OSS names (`certifi`, `onnx`, `wasm`, `tesseract`, `pdfium`, `pymupdf`,
  `surrealdb`, `mkdocs`, `mkdocstrings`, `rumdl`) — are now valid out of the box,
  so every repo no longer re-lists them in `extend_words`.

## [0.6.0] - 2026-07-05

### Added

- **Whole-workspace hook isolation for `poly hooks`.** Hooks that analyze the
  whole project rather than a file list — `cargo clippy`/`sort`/`machete`/`deny`
  and type checkers like `pyrefly` — can now be marked `workspace = true` (the
  `cargo` builtin group sets it automatically). A whole-workspace hook takes no
  appended filenames (`{staged_files}` opts back in) and runs against a
  **non-destructive snapshot of the git index** at `.polylint/staged`, so a
  pre-commit check sees exactly what the commit would capture: unstaged edits and
  untracked files never affect it, and — unlike `git stash`-based approaches — the
  working tree is never touched. The snapshot is a persistent, git-ignored cache
  sourced from the index blob and refreshed incrementally (only files whose staged
  object id changed are re-materialized), so cargo/pyrefly/`tsc` incremental caches
  stay warm; cargo is pointed at the real `target/` and coexists with dev builds.
  On by default for the commit-gating stages (`pre-commit`, `pre-merge-commit`) and
  skipped for `--all-files`; opt out with `[hooks] isolate = false`. See ADR 0019.
- **Default-on result caching for the `cargo` builtin group**, keyed on the Rust
  source/manifest set (`**/*.rs`, `Cargo.toml`, `Cargo.lock`, `deny.toml`,
  toolchain files). A commit touching no Rust skips `clippy`/`sort`/`machete`/`deny`
  entirely; a whole-workspace hook's cache key digests the **staged** snapshot
  content, so reverting an unstaged edit is never a false hit. Opt out with
  `cargo = { cache = false }`.

## [0.5.1] - 2026-07-04

### Fixed

- **Trailing whitespace no longer leaks into `poly lint`.** The tree-sitter
  generic tier (and the format-only native-tool backends that fall back to it —
  `gofmt`, `rustfmt`, `swift-format`, …) previously reported a
  `trailing-whitespace` **lint** diagnostic that `poly lint --fix` could not act
  on: the diagnostic carried no autofix, and the fix lives on the format path.
  Worse, `lint` flagged it even in files that `fmt` deliberately leaves alone
  (e.g. a Swift file marked `// swift-format-ignore-file`), so the warning could
  never be cleared. Trailing whitespace is now purely a **`polyfmt`** concern:
  the generic tier and the format-only native backends declare `lint: false` and
  emit no lint diagnostics; run `poly fmt --fix` to strip trailing whitespace.

## [0.5.0] - 2026-07-04

### Added

- **Live per-hook progress for `poly hooks`** — when stderr is a terminal, each
  hook now prints a `▶ <id> …` line as it starts and a `✓/× <id> (<duration>)`
  line as it finishes, so a long-running hook (`cargo clippy`, `cargo test`, …) is
  visibly running instead of leaving the terminal blank until the whole stage
  completes — which read as a hung commit. Progress goes to stderr and is
  suppressed when stderr is not a terminal (piped / CI), so captured output is
  unchanged.
- **Autofixable count in the lint summary** — `poly lint` now reports how many of
  the findings can be resolved automatically (`N fixable with the `--fix`
  option.`), making the value of a follow-up `--fix` run obvious from a dry run.
  The line is omitted when nothing is fixable.

## [0.4.0] - 2026-07-04

### Added

- **Colored `poly hooks install` / `uninstall` output** — a green ✓ header with
  the hook count and the (relative) hooks directory, then one line per hook name,
  replacing the flat list of absolute paths.

### Changed

- **Installed git-hook shims resolve `poly` from `PATH`** rather than baking in an
  absolute path to the binary, so a hook always runs whatever `poly` is current
  (a recorded absolute path could pin a stale or moved build). When `poly` is not
  on `PATH` the shim now fails with a clear, actionable message and a non-zero exit
  instead of proceeding as though the hook had passed. Re-run `poly hooks install`
  to migrate existing shims.

### Fixed

- Native-toolchain formatter output is normalized to LF line endings; some
  first-party CLIs emit CRLF on Windows, which made output platform-dependent.

## [0.3.0] - 2026-07-04

### Added

- **Native-toolchain formatter backends** — opt-in backends that invoke a
  language's canonical first-party CLI when it is present on the host: Java,
  Kotlin, R, Swift, Dart, and Gleam. Off by default (enabled per-tool in config);
  when the tool is absent the language falls through to the tier-2 tree-sitter
  formatter, so the zero-dependency guarantee is intact for anyone who has not
  opted in.
- **C# tier-2 support** — a `Language::CSharp` variant so `.cs` files route to the
  tree-sitter generic formatter (deterministic, zero system dependency) instead of
  being skipped. Maps the `c#` / `csharp` catalog names and the `.cs` extension.
- **Elixir `do…end` reindent** in the tier-2 formatter. Elixir's blocks are
  keyword-delimited (`do…end`), so they matched neither the brace-counting path nor
  a language-pack indents query (tree-sitter-elixir ships none) and were left at
  column 0. A new built-in-indents-query dispatch slot plus a minimal Elixir query
  produces `mix format`'s 2-space nesting; idempotent, with heredocs/strings
  preserved.

### Changed

- **`poly fmt` honors `// swift-format-ignore-file`** — a Swift file carrying the
  directive is left byte-for-byte untouched (the same whole-file skip marker
  `swift-format` respects), mirroring the generated-lock-file skip. Protects files a
  project opted out of formatting and machine-generated swift-bridge glue.
- Bumped `fs-err`, `serde-saphyr`, and `sqruff` to their latest releases.

### Fixed

- **`poly hooks` now enforces the `commit-msg` stage.** Lowered hooks kept
  `Stage::default()` (pre-commit), so the runner dispatched the `poly commit`
  (Conventional Commits) builtin in file-input mode, matched no files, and silently
  skipped it — the git `commit-msg` hook never enforced anything. Every lowered hook
  is now stamped with the stage it was lowered for; latent for any non-pre-commit
  builtin, only `poly-commit` surfaced it.
- **Rust files named like `dockerfile.rs` are no longer misdetected as
  Dockerfiles.** Language detection now lets a known file extension (`.rs` → Rust)
  win over the Dockerfile filename match, so `engines/dockerfile.rs` and similar no
  longer produce spurious Dockerfile parse errors.

## [0.2.0] - 2026-07-03

### Added

- **Biome CSS + GraphQL linters** — two in-process tier-1 lint backends built on
  the official `biomejs/biome` analyzer crates, filling gaps polylint had no
  native linter for. Both are lint-only and coexist with the existing malva/
  graphql formatters. Configured via `[lint.css.biome]` / `[lint.graphql.biome]`
  with the shared `select`/`extend_select`/`ignore` surface; default rule groups
  are `correctness` + `suspicious`.
- **`poly migrate`** — new subcommand that absorbs a repo's `ruff` / `typos` /
  `taplo` / markdownlint config (including `pyproject.toml` `[tool.ruff]` /
  `[tool.typos]` / `[tool.codespell]`) into `poly.toml`, comment-preserving, then
  deletes or strips only the sources poly can fully honor. Dry-run report by
  default; `--write`, `--recurse`, `--verify`, `--strip-superseded`.
- **Native typos config** — `_typos.toml` / `.typos.toml` / `pyproject
  [tool.typos]` / `[tool.codespell]` are honored, including `extend-ignore-re`
  (region masking), `extend-ignore-words-re` / `-identifiers-re`, and full
  ancestor-chain merging.
- **Dockerfile rule selection** — the Dockerfile backend now honors
  `[lint.dockerfile]` `select` / `extend_select` / `ignore`.

### Changed

- Dry-run `poly fmt` (no `--fix`) now reports "N file(s) will change" instead of
  the past-tense "N changed", which implied files were rewritten.
- Bumped the pinned oxc (`c0c69dc`) and ruff (`1cb2012`) revisions and ran
  `cargo upgrade --incompatible` (clap_complete, rand, rmcp, rustc-hash).

### Removed

- **The R (air/jarl) tier-1 backend.** Migrating air+jarl onto official
  `biomejs/biome` was disproportionately costly (a large fork rebase across biome
  API drift plus a non-upstream patch), and air/jarl were the sole consumers of
  the `lionel-/biome` fork. Dropping them removes that fork from the dependency
  graph and unblocks the official biome CSS/GraphQL analyzers with no crate
  collision. R now falls through to the tier-2 tree-sitter formatter (best-effort
  format, no lint).

## [0.1.15] - 2026-07-02

### Added

- **Hierarchical (monorepo-aware) config resolution** (ADR 0018). Running `poly`
  from a monorepo root now discovers nested `poly.toml` files and cascades them
  the way ruff/eslint resolve config: a file is governed by the deep-merge of its
  ancestor config chain (workspace root as base, nearest config wins), so a
  sub-project's `poly.toml` declares only its diff and inherits `[defaults]`, the
  `[lint.*]`/`[fmt.*]` rule tables, and `[per-file-ignores]` from above.
  - New `[workspace] root = true` marker bounds the upward cascade; a `.git`
    directory is an implicit boundary, so single repos need no annotation.
  - `[discovery] exclude` globs are unioned tree-wide, each rooted at its own
    config directory (a nested config prunes only its subtree); `[per-file-ignores]`
    globs resolve relative to their owning config's directory.
  - `--config <path>` pins a single config and bypasses nested resolution.
  - Fully back-compatible: a repo with one root `poly.toml` and no nested configs
    resolves every file to the root config, identical to before.

## [0.1.12] - 2026-07-02

### Fixed

- **ruff cache-key** now folds the E501/`line_length` engine change (0.1.11).
  The `line_length`-honoring fix altered lint output for the same input without
  bumping the ruff engine `version()` suffix, so warm `.polylint` caches kept
  serving stale 88-column E501 diagnostics. Bumped the suffix (`+e501`) to
  invalidate them. (CI is unaffected — fresh runners have no cache.)

## [0.1.11] - 2026-07-02

### Fixed

- **ruff E501 now honors `line_length`.** The line-too-long rule read ruff's
  `pycodestyle.max_line_length`, which poly never set — so it stayed pinned at
  ruff's hardcoded 88 regardless of the configured `line_length` (while the
  formatter correctly used 120). poly now mirrors the resolved `line_length`
  onto `pycodestyle.max_line_length` in both the default and per-config settings,
  so `select = ["ALL"]` projects with a 120 limit no longer see false-positive
  E501 on 89–120 char lines.

## [0.1.10] - 2026-07-02

### Fixed

- **actionlint**: restrict linting to GitHub Actions workflow files
  (`.github/workflows/**/*.yml|yaml`). Previously `poly lint .` ran `actionlint`
  on every YAML file (including `Taskfile.yml`, `docker-compose.yaml`, etc.),
  emitting spurious "jobs section is missing" errors. The tool now silently skips
  non-workflow YAML. A new `path_globs` field in the catalog model provides a
  general mechanism for future path-scoped tools.

### Added

- **ruff / isort**: `known_first_party` and `known_third_party` options for the
  ruff engine, settable in `poly.toml` under `[lint.python.ruff]`. Resolves
  false `I001` (import-block un-sorted) errors when a first-party package lives
  in a `src/`-layout that the package-root walk cannot reach from a sibling
  `tests/` directory.

  ```toml
  [lint.python.ruff]
  known_first_party = ["kreuzberg_cloud"]
  ```

## [0.1.9] - 2026-07-02

### Fixed

- **rustfmt**: `poly fmt` now honours the project's `rustfmt.toml` /
  `.rustfmt.toml` when formatting Rust source. Previously poly always injected
  `--config max_width=120`, which silently overrode every option in the
  project's rustfmt config (not just `max_width`). Now poly walks up from each
  source file to find the nearest `rustfmt.toml` and passes its directory via
  `--config-path`, letting rustfmt load the full project configuration. When no
  config file is found the existing 120-column default is preserved.

## [0.1.8] - 2026-07-01

### Fixed

- **cli**: `poly lint` exits non-zero only when a diagnostic is error-severity.
  Warning/info/hint findings are still reported but no longer fail the run, git
  hooks, or CI — matching the ruff/eslint/clippy convention. Previously any finding
  (including warnings) exited non-zero.

### Changed

- **deps**: upgrade dependencies to their latest versions (`cargo upgrade --incompatible`):
  quick-xml 0.40 → 0.41, plus clap_complete and indicatif.

## [0.1.7] - 2026-07-01

### Added

- **Uniform rule selection across `ruff`, `sqruff`, and `rumdl`** — all three now
  accept the canonical `select` / `extend_select` / `ignore` vocabulary through the
  shared parser, with each tool's native keys (`rules`/`exclude_rules`,
  `enable`/`disable`) kept as back-compat aliases and unioned. Unknown or blank rule
  codes are surfaced with a warning and skipped instead of dropped silently.
- **Uniform per-rule severity remap** — a configured `[lint.<lang>.<tool>.rules.<code>]
  level` is now honored for every engine as a post-lint remap on the normalized
  diagnostic code, including engines with no native severity configuration.
- ADR 0016 (uniform rule-selection model) and ADR 0017 (path exclusions and
  per-file rule ignores) documenting the configuration design.

### Fixed

- **Tier-2 generic formatter** no longer rewrites the interior of multi-line
  strings, heredocs, raw strings, or block comments on the query-driven reindent
  path (the brace-counting path was already guarded); their significant leading
  whitespace is preserved byte-for-byte.
- **`rubyfmt` cache key** now folds the pinned git rev instead of a stale version
  string, so a rev bump invalidates cached Ruby output.

### Changed

- **Homebrew distribution now ships bottles.** The tap formula builds `poly` from
  source and the release dispatches the tap's bottle workflow, so `brew install`
  pours a prebuilt bottle on supported platforms (macOS ARM64, Linux x86_64/ARM64)
  and builds from source elsewhere.

### Internal

- Extracted a shared `deserialize_options` helper for the format-only backends
  (malva, markup_fmt).
- Added a `Cargo.lock` drift-guard test asserting every backend's `version()`
  embeds the resolved crate version or pinned git rev — enforcing cache-key
  discipline across all 17 backends.

## [0.1.6] - 2026-06-30

### Added

- **Per-tool rule configuration** — a uniform `select` / `extend_select` / `ignore`
  surface plus per-rule `[rules.<id>]` overrides (`level` + tool-specific params)
  for `mago`, `ruff`, `oxc`, `sqruff`, `rumdl`, and R/`jarl`. `select` by category
  replaces the default set; unknown rule/category names error loudly.
- **Formatter options** via `[fmt.<lang>.<tool>]` for `yaml`, CSS/SCSS/Less
  (`malva`), HTML/Vue/… (`markup_fmt`), GraphQL, and TOML (`taplo`).
- **ruff per-plugin parameters**: `pydocstyle_convention`, `mccabe_max_complexity`,
  `pylint_max_args`, `pylint_max_branches`, `pylint_max_returns`.
- **Path exclusions** across config, CLI, and MCP: `[discovery] exclude`, a
  repeatable `--exclude <glob>` flag, and an MCP `exclude` parameter.
- **`[per-file-ignores]`** — gitignore-style glob → rule-code suppression, applied
  as a cross-engine post-lint filter.
- **`[tools.*]` `env` and `root`** — environment variables and a working directory
  for catalog tools (e.g. running `golangci-lint` per Go module).
- **`[hooks.builtin.cargo] clippy_args`** to override the clippy invocation.
- **oxc** per-rule `Deny` severity and JS/JSON formatter options.
- Per-engine `indent_width` override (honored uniformly by every formatter).
- A `Taskfile.yaml` with the standard dev tasks.

### Fixed

- **ruff INP001 false positives** and **isort first-party misclassification** —
  the package root is now resolved from the file's directory, so per-file linting
  matches ruff's whole-tree behavior. (isort `I001`/`I002` are in the default set,
  so this affected every run.)
- **Single-file invocations** now apply `[per-file-ignores]` and report the
  correct path (a file passed as its own root collapsed to an empty match path).
- **Lint cache correctness** — the cache key now folds the file path (byte-identical
  files such as empty `__init__.py` no longer collide and serve each other's
  path-dependent diagnostics) and the effective `[defaults]` globals.
- **HCL** inline trailing comments are no longer lost on format (files with
  comments route to the structural tier instead of the comment-stripping path).
- **Dockerfile** parse failures now surface as `Error` diagnostics instead of
  being silently swallowed.
- **sqruff** parse/lex errors are reported as `Error` (not `Warning`).
- **R** `--fix` applies only fixes whose status is `Safe`.
- **rustfmt** (native-toolchain backend) honors the 120-column line width.
- **`php_version`** rejects a non-numeric component (e.g. `"8.x"`) instead of
  silently defaulting it to `0`.

### Performance

- **mago** caches its rule registry per run instead of rebuilding it for every file.

### Changed

- `oxc` and `sqruff` no longer advertise a `fix` capability they did not implement.

### Documentation

- Corrected ADR drift (configuration, backend selections, distribution, catalog,
  caching) and README inaccuracies (version-pin examples, MCP tool names and
  parameters, version badges).

[0.1.6]: https://github.com/Goldziher/poly/releases/tag/v0.1.6
