# conformance

A dev-only differential harness that measures how close `polyfmt` is to each
language's **idiomatic reference formatter**. It is the mechanism by which we
derive and validate the conventions our pure-Rust formatters should follow, and
track per-language progress toward being a drop-in replacement for the reference
tool.

This crate is **not published** and the shipped `poly` / `polylint` / `polyfmt`
binaries never depend on it or on any reference tool.

## How it works

- `corpus/<lang>/` — deliberately unformatted sample files per language.
- `golden/<lang>/` — the reference tool's output for each corpus file
  (committed). This is the convention target.
- `docker/<lang>.Dockerfile` — a pinned image that runs the reference formatter
  as a `stdin → stdout` filter.
- `tools.toml` — per-language reference tool + file extensions.

`generate` builds each image (`conformance-<lang>`) and pipes every corpus file
through it to (re)produce the golden output. `check` runs `polyfmt` over the
same corpus and scores its output against the golden — exact byte match plus a
line-similarity ratio — so we can see, per language, how far we are and which
conventions we're missing.

## Reference tools (opinionated)

| Language | Reference | Notes |
|---|---|---|
| Elixir | `mix format` | canonical, opinionated |
| Shell | `shfmt -i 2` | matches the repo's shfmt hook |
| Ruby | `standardrb --fix` | zero-config opinionated |
| PHP | Laravel Pint | zero-config opinionated |
| Kotlin | ktfmt | opinionated |
| Java | palantir-java-format | opinionated (golden pending: needs an all-deps CLI jar) |

Languages whose toolchain ships a canonical formatter (Go/gofmt, Rust/rustfmt)
are intentionally absent — we simply match those conventions.

## Usage

```sh
# Regenerate golden (requires Docker; one image per language, cached):
cargo run -p conformance -- generate                 # all languages
cargo run -p conformance -- generate --lang elixir   # one language

# Score polyfmt against the committed golden (hermetic; no Docker):
cargo run -p conformance -- check
cargo run -p conformance -- check --lang shell --min 0.9   # fail under 90%
```

`check` is intentionally a binary, not a `cargo test`, so the normal test suite
stays hermetic — `polyfmt` fetches tree-sitter grammars on demand, which `check`
exercises but unit tests should not.

## Roadmap

Baseline at introduction (generic tier vs reference): elixir ~29%, kotlin ~29%,
php ~35%, shell ~43%. Each language gets an increasingly idiomatic pure-Rust
formatter, iterated against `check` until it converges on the reference output.
