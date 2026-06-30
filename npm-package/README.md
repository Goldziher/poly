# poly

A universal zero-dependency linter & formatter — two pure-Rust binaries (`polylint` + `polyfmt`)
wrapping best-in-class tools as in-process backends, with a tree-sitter generic tier for
everything else. Installs three commands: `poly`, `polylint`, `polyfmt`.

<!-- markdownlint-disable-next-line MD013 -->
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](https://github.com/Goldziher/polylint#readme)
[![npm](https://img.shields.io/npm/v/@nhirschfeld/polylint.svg)](https://www.npmjs.com/package/@nhirschfeld/polylint)

## Install

```bash
npm install -g @nhirschfeld/polylint
```

This installs three binaries on your `PATH`: `poly` (the umbrella CLI), `polylint` (lint), and
`polyfmt` (format).

The installer downloads the appropriate pre-compiled Rust binaries for your platform (macOS,
Linux, Windows; x86_64 + arm64) from
[GitHub Releases](https://github.com/Goldziher/polylint/releases) on first install.

## Quickstart

```bash
cd /path/to/your/repo
polylint .           # lint the working tree
polyfmt . --fix      # format the working tree in place
poly hooks run       # run the configured git hooks
```

## Full documentation

See the [main README](https://github.com/Goldziher/polylint#readme) for complete docs,
architecture, the backend/tier reference, and configuration.

## License

[MIT](https://github.com/Goldziher/polylint/blob/main/LICENSE-MIT) OR
[Apache-2.0](https://github.com/Goldziher/polylint/blob/main/LICENSE-APACHE).
