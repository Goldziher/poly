# polylint

[poly](https://github.com/Goldziher/poly) is a universal, zero-dependency linter and
formatter: one pure-Rust binary that wraps best-in-class tools as in-process backends,
with a tree-sitter generic tier for everything else.

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
poly fmt --check .
```

## How the binary gets here

Nothing is downloaded at install time and there is no build step. Each release publishes
platform-specific wheels carrying one prebuilt binary:

| Wheel platform tag             | Binary                          |
| ------------------------------ | ------------------------------- |
| `macosx_11_0_arm64`            | `aarch64-apple-darwin`          |
| `macosx_10_12_x86_64`          | `x86_64-apple-darwin`           |
| `manylinux_2_*_aarch64`        | `aarch64-unknown-linux-gnu`     |
| `manylinux_2_*_x86_64`         | `x86_64-unknown-linux-gnu`      |
| `musllinux_1_2_x86_64`         | `x86_64-unknown-linux-musl`     |
| `win_amd64`                    | `x86_64-pc-windows-msvc`        |

A `py3-none-any` wheel is published alongside them. pip ranks platform wheels above it, so
it is only ever installed where none of the above match — and then `poly` prints which
platforms are published and where to get a binary, instead of failing with pip's bare
"could not find a version that satisfies the requirement".

## Links

- [Documentation](https://github.com/Goldziher/poly#readme)
- [Changelog](https://github.com/Goldziher/poly/blob/main/CHANGELOG.md)
- [Issues](https://github.com/Goldziher/poly/issues)

MIT licensed.
