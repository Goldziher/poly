# Package-manager installs

poly ships one binary. Every channel below installs that same prebuilt binary — none of
them compiles anything, and none needs a Rust, Node, or Python toolchain at run time.

> See the README's [Installation](../README.md#installation) section for the quickstart
> version of this. This file is the deeper reference — per-platform package names, wheel
> layout — plus the maintainer-only publishing notes below, which do not belong on a public
> landing page.

## The command is `poly`

The package is called `polylint` on PyPI and `@goldziher/polylint` on npm, because the
unscoped `poly` name belongs to unrelated projects on both registries. **The executable it
installs is `poly`.** `polylint` also works everywhere as an alias for the same binary, so
whichever name you install under, both commands run the same tool.

## npm

```sh
npm install --global @goldziher/polylint   # or: npm install --save-dev @goldziher/polylint
poly lint .
```

The package pulls exactly one prebuilt binary through `optionalDependencies`, matched to
your platform by npm's own `os`, `cpu` and `libc` fields. There is no `postinstall` script
and nothing is downloaded at install time, so installs work offline and behind a proxy,
and the binary is covered by the registry's integrity hashes like any other package
content.

| Platform                | Platform package                      |
| ----------------------- | ------------------------------------- |
| macOS (Apple silicon)   | `@goldziher/polylint-darwin-arm64`    |
| macOS (Intel)           | `@goldziher/polylint-darwin-x64`      |
| Linux arm64 (glibc)     | `@goldziher/polylint-linux-arm64-gnu` |
| Linux x64 (glibc)       | `@goldziher/polylint-linux-x64-gnu`   |
| Linux x64 (musl/Alpine) | `@goldziher/polylint-linux-x64-musl`  |
| Windows x64             | `@goldziher/polylint-win32-x64`       |

Installing with optional dependencies disabled (`--omit=optional`) is the one way to end
up with the wrapper and no binary; `poly` then says so and tells you how to fix it rather
than failing with a stack trace.

## PyPI

```sh
pip install polylint     # or: uv tool install polylint
poly lint .
```

Each release publishes platform-specific wheels, each carrying one binary as package data
plus the `poly` and `polylint` console scripts. Again: no download at install time, no
build step.

A `py3-none-any` wheel is published alongside the platform wheels. pip ranks platform
wheels above it, so a supported platform never resolves to it; an unsupported one gets a
`poly` command that explains which platforms are published instead of pip's bare "could
not find a version that satisfies the requirement".

## Homebrew

```sh
brew install Goldziher/tap/poly
```

## Scoop (Windows)

```powershell
scoop bucket add goldziher https://github.com/Goldziher/scoop-bucket
scoop install poly
```

## Installer scripts

```sh
curl -fsSL https://raw.githubusercontent.com/Goldziher/poly/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/Goldziher/poly/main/install.ps1 | iex
```

---

## Maintainer notes

### How a release reaches each registry

`.github/workflows/publish.yaml` builds the six platform archives, uploads them to the
GitHub release, generates `sha256sums.txt`, and only then un-drafts the release. The
`publish_npm`, `publish_pypi` and `publish_scoop` jobs run after that, and each one
**downloads the archives back off the release** and verifies them against
`sha256sums.txt` before packaging. Nothing is rebuilt: the bytes inside a wheel or an npm
platform package are the same bytes the checksums file describes.

All three jobs are `continue-on-error: true` and nothing depends on them. A registry
misconfiguration therefore shows up as a red job on a green release run — the GitHub
release is already published before they start, and no failure of theirs can put it back
into draft. That is deliberate: a broken publish channel must be visible, but it must not
take the release with it.

### Registry configuration

Both registries authenticate by OIDC trusted publishing. There is no `NPM_TOKEN` and no
PyPI API token anywhere in the workflow.

**npm** — seven packages (`@goldziher/polylint` plus the six platform packages) each need
a trusted publisher pointing at:

- Repository: `Goldziher/poly`
- Workflow: `publish.yaml`
- Environment: _(leave blank)_

npm can only configure a trusted publisher for a package that already exists, so each
package needs one manual `npm publish` before CI can take over. Publish that first version
under a non-`latest` dist-tag (`npm publish --access public --tag placeholder`) so it does
not become what `npm install @goldziher/polylint` resolves to — note that npm sets `latest`
on a package's very first publish regardless, so the umbrella's `latest` has to be moved by
the first real release. **This applies to every platform package too**, not just the
umbrella. All seven have had their bootstrap publish and exist on the registry; a new
platform target added later needs the same manual first publish before CI can take it over.
`publish_npm` publishes the platform packages before the umbrella either way, because an
umbrella on `latest` whose pinned binaries do not exist installs cleanly and then has no
`poly` to run.

**PyPI** — one project, `polylint`, with a trusted publisher pointing at:

- Owner: `Goldziher`, repository: `poly`
- Workflow: `publish.yaml`
- Environment: `pypi` (the workflow declares `environment: pypi`; PyPI's environment field
  may be left blank, but if it is set it must be exactly `pypi`)

**Scoop** — the bucket is `Goldziher/scoop-bucket` (branch `main`) and the manifest is
`bucket/poly.json`. The push reuses the existing `HOMEBREW_TOKEN` secret, whose access
covers that repository as well as the tap; there is no separate Scoop credential.

`scripts/update-scoop-manifest.sh` rewrites only `version`, the 64-bit `url`, and the
64-bit `hash` (read out of the release's own `sha256sums.txt`), and asserts that `bin`,
`checkver`, `autoupdate`, `description`, `license` and `homepage` came through untouched.
The `bin` array is what creates both the `poly` and `polylint` shims, so losing it would
be silent — `scoop install` would still succeed and produce no command.

### Version lock-step

`scripts/release-bump.sh <version>` moves every surface at once: `Cargo.toml`, the npm
umbrella package **and each of its six exact `optionalDependencies` pins**, the six
platform `package.json` files, `pip-package/pyproject.toml`,
`pip-package/polylint/__init__.py`, and `.ai-rulez/config.toml`. It then asserts each
substitution actually landed, and that the pinned package names and the `platforms/`
directories describe the same set — a pin left behind would resolve to the previous
release's binary under the new version, which installs cleanly and is wrong.
`publish.yaml`'s `meta` job re-checks all of it against the tag before anything is built.

### Building the packages locally

```sh
task package:npm       # stage target/debug/poly into the host platform package, then npm pack
task package:wheels    # build a platform wheel from target/debug/poly
```
