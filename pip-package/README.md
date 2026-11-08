# polylint

[polylint](https://github.com/Goldziher/polylint) is one linter and formatter for every
language in your repository, driven by one `poly.toml`. It runs ruff, oxc, biome, mago,
sqruff and more as signed engine components, uses host tools such as `rustfmt` and `gofmt`
when they are installed, and formats the long tail with tree-sitter.

> **The command is `poly`.** The distribution is named `polylint` because the `poly` name
> on PyPI is not ours; `polylint` also works as an alias for the same executable, so
> either name gets you the tool.

## Install

```sh
pip install polylint
# or
uv tool install polylint
```

Then:

```sh
poly lint .
poly fmt .
```

## How the binary gets here

Nothing is downloaded at install time and there is no build step. Each release publishes
platform-specific wheels carrying one prebuilt binary:

| Wheel platform tag             | Binary                          |
| ------------------------------ | ------------------------------- |
| `macosx_11_0_arm64`            | `aarch64-apple-darwin`          |
| `manylinux_2_*_aarch64`        | `aarch64-unknown-linux-gnu`     |
| `manylinux_2_*_x86_64`         | `x86_64-unknown-linux-gnu`      |
| `musllinux_1_2_x86_64`         | `x86_64-unknown-linux-musl`     |
| `win_amd64`                    | `x86_64-pc-windows-msvc`        |

A `py3-none-any` wheel is published alongside them. pip ranks platform wheels above it, so
it is only ever installed where none of the above match — and then `poly` prints which
platforms are published and where to get a binary, instead of failing with pip's bare
"could not find a version that satisfies the requirement".

## Engines are downloaded at run time

Nothing is fetched at install time, but every wheel except `musllinux_1_2_x86_64` carries a
**core** build: the first `poly lint` or `poly fmt` downloads the engine components the
repository needs (`python`, `web`, `sql`, …), verifies each against the signed lock
embedded in the binary, and caches it. Later runs use the cache.

- The `musllinux_1_2_x86_64` wheel is the **bundled** build: every engine is linked in and
  nothing is downloaded.
- `POLY_COMPONENT_MIRROR` points component downloads at an internal mirror.
- `poly components sync .` installs ahead of time; `poly components import <archive>`
  seeds a host with no network; `--offline` forbids every download.

See [Components and offline use](https://goldziher.github.io/polylint/guides/components/).

## Links

- [Documentation](https://goldziher.github.io/polylint/)
- [Changelog](https://github.com/Goldziher/polylint/blob/main/CHANGELOG.md)
- [Issues](https://github.com/Goldziher/polylint/issues)

MIT licensed.
