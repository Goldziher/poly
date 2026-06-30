# poly

Universal zero-dependency linter and formatter. This package installs the `poly` CLI.

<!-- markdownlint-disable-next-line MD013 -->
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](https://github.com/Goldziher/polylint/blob/main/LICENSE)
[![npm](https://img.shields.io/npm/v/@nhirschfeld/polylint.svg)](https://www.npmjs.com/package/@nhirschfeld/polylint)

## Install

```bash
npm install -g @nhirschfeld/polylint
```

The postinstall script downloads the matching prebuilt Rust binary bundle from
[GitHub Releases](https://github.com/Goldziher/polylint/releases), verifies it against the release
checksums, and exposes the `poly` command.

## Quickstart

```bash
cd /path/to/your/repo
poly fmt --check
poly lint
poly hooks run pre-commit --all-files
```

## Full Documentation

See the [main README](https://github.com/Goldziher/polylint#readme) for installation options,
configuration, backend coverage, architecture, and CLI reference.

## License

MIT - see [LICENSE](https://github.com/Goldziher/polylint/blob/main/LICENSE).
