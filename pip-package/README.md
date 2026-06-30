# polylint

Universal zero-dependency linter & formatter — two pure-Rust binaries (`polylint` + `polyfmt`),
driven by one config, wrapping best-in-class tools as in-process backends with a tree-sitter
generic tier for everything else. Plus the `poly` umbrella CLI (lint, format, hooks, and more).

<!-- markdownlint-disable-next-line MD013 -->
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](https://github.com/Goldziher/polylint)
[![PyPI](https://img.shields.io/pypi/v/polylint.svg)](https://pypi.org/project/polylint/)

## Install

```bash
pip install polylint
```

This installs three console scripts — `poly`, `polylint`, and `polyfmt`. On first invocation, the
pre-compiled Rust binaries for your platform (macOS, Linux, Windows; x86_64 + arm64) are downloaded
from [GitHub Releases](https://github.com/Goldziher/polylint/releases) and cached under
`~/.cache/polylint/<version>/`.

Override the cache directory holding the binaries with `POLYLINT_BINARY=/path/to/dir`, or point a
single binary at an explicit path with `POLYLINT_BINARY_POLY` / `POLYLINT_BINARY_POLYLINT` /
`POLYLINT_BINARY_POLYFMT`.

## Quickstart

```bash
cd /path/to/your/repo
polylint .           # lint the working tree
polyfmt .            # format (dry-run); add --fix to write changes
poly hooks run pre-commit --all-files
```

## Full documentation

See the [main README](https://github.com/Goldziher/polylint#readme) for complete docs,
architecture, backend coverage, and configuration reference.

## License

Licensed under either of [MIT](https://github.com/Goldziher/polylint) or
[Apache-2.0](https://github.com/Goldziher/polylint) at your option.
