# @goldziher/polylint

[poly](https://github.com/Goldziher/poly) is a universal, zero-dependency linter and
formatter: one pure-Rust binary that wraps best-in-class tools as in-process backends,
with a tree-sitter generic tier for everything else.

> **The command is `poly`, not `polylint`.** The npm package is named
> `@goldziher/polylint` because the unscoped `poly` name is taken; the executable it puts
> on your `PATH` is `poly`.

## Install

```sh
npm install --global @goldziher/polylint
# or, per project
npm install --save-dev @goldziher/polylint
```

Then:

```sh
poly lint .
poly fmt --check .
```

## How the binary gets here

There is no `postinstall` script and nothing is downloaded at install time. This package
declares six platform packages as `optionalDependencies`, each carrying one prebuilt
binary and marked with the `os`, `cpu` and `libc` it is for. npm installs only the one
matching your machine, so installs work offline, behind a proxy, and are covered by the
registry's own integrity hashes.

| Platform                | Package                                 |
| ----------------------- | --------------------------------------- |
| macOS (Apple silicon)   | `@goldziher/polylint-darwin-arm64`      |
| macOS (Intel)           | `@goldziher/polylint-darwin-x64`        |
| Linux arm64 (glibc)     | `@goldziher/polylint-linux-arm64-gnu`   |
| Linux x64 (glibc)       | `@goldziher/polylint-linux-x64-gnu`     |
| Linux x64 (musl/Alpine) | `@goldziher/polylint-linux-x64-musl`    |
| Windows x64             | `@goldziher/polylint-win32-x64`         |

On any other platform, `poly` prints which platforms are supported and points at the
source build instead of failing with a stack trace.

## Links

- [Documentation](https://github.com/Goldziher/poly#readme)
- [Changelog](https://github.com/Goldziher/poly/blob/main/CHANGELOG.md)
- [Issues](https://github.com/Goldziher/poly/issues)

MIT licensed.
