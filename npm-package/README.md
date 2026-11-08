# @goldziher/polylint

[polylint](https://github.com/Goldziher/polylint) is one linter and formatter for every
language in your repository, driven by one `poly.toml`. It runs ruff, oxc, biome, mago,
sqruff and more as signed engine components, uses host tools such as `rustfmt` and `gofmt`
when they are installed, and formats the long tail with tree-sitter.

> **The command is `poly`, not `polylint`.** The pnpm package is named
> `@goldziher/polylint` because the unscoped `poly` name is taken; the executable it puts
> on your `PATH` is `poly`.

## Install

```sh
pnpm add --global @goldziher/polylint
# or, per project
pnpm add --save-dev @goldziher/polylint
```

Then:

```sh
poly lint .
poly fmt .
```

## How the binary gets here

There is no `postinstall` script and nothing is downloaded at install time. This package
declares five platform packages as `optionalDependencies`, each carrying one prebuilt
binary and marked with the `os`, `cpu` and `libc` it is for. npm installs only the one
matching your machine, so installs work offline, behind a proxy, and are covered by the
registry's own integrity hashes.

| Platform                | Package                                 |
| ----------------------- | --------------------------------------- |
| macOS (Apple silicon)   | `@goldziher/polylint-darwin-arm64`      |
| Linux arm64 (glibc)     | `@goldziher/polylint-linux-arm64-gnu`   |
| Linux x64 (glibc)       | `@goldziher/polylint-linux-x64-gnu`     |
| Linux x64 (musl/Alpine) | `@goldziher/polylint-linux-x64-musl`    |
| Windows x64             | `@goldziher/polylint-win32-x64`         |

On any other platform, `poly` prints which platforms are supported and points at the
source build instead of failing with a stack trace.

## Engines are downloaded at run time

Nothing is fetched at install time, but every platform package except
`linux-x64-musl` carries a **core** build: the first `poly lint` or `poly fmt` downloads the
engine components the repository needs (`python`, `web`, `sql`, …), verifies each against
the signed lock embedded in the binary, and caches it. Later runs use the cache.

- `@goldziher/polylint-linux-x64-musl` is the **bundled** build: every engine is linked in
  and nothing is downloaded.
- `POLY_COMPONENT_MIRROR` points component downloads at an internal mirror.
- `poly components sync .` installs ahead of time; `poly components import <archive>`
  seeds a host with no network; `--offline` forbids every download.

See [Components and offline use](https://goldziher.github.io/polylint/guides/components/).

## Links

- [Documentation](https://goldziher.github.io/polylint/)
- [Changelog](https://github.com/Goldziher/polylint/blob/main/CHANGELOG.md)
- [Issues](https://github.com/Goldziher/polylint/issues)

MIT licensed.
