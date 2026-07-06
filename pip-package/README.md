# polylint

Universal zero-dependency linter and formatter. This package installs the `poly` CLI.

<!-- markdownlint-disable-next-line MD013 -->
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](https://github.com/Goldziher/poly/blob/main/LICENSE)
[![PyPI](https://img.shields.io/pypi/v/polylint.svg)](https://pypi.org/project/polylint/)

## Install

```bash
pip install polylint
```

On first invocation, the wrapper downloads the matching prebuilt Rust binary bundle from
[GitHub Releases](https://github.com/Goldziher/poly/releases) and caches it under
`~/.cache/polylint/<version>/`.

Override the cache directory with `POLYLINT_BINARY=/path/to/dir`, or point at an explicit binary
with `POLYLINT_BINARY_POLY`.

## Quickstart

```bash
cd /path/to/your/repo
poly fmt --check
poly lint
poly hooks run pre-commit --all-files
```

## Full Documentation

See the [main README](https://github.com/Goldziher/poly#readme) for installation options,
configuration, backend coverage, architecture, and CLI reference.

## License

MIT - see [LICENSE](https://github.com/Goldziher/poly/blob/main/LICENSE).
