# Setup poly CLI — GitHub Action

A composite GitHub Action that installs the `poly` binary in a CI job,
with optional local caching. It delegates the download/target-detection/checksum/extraction to the
repository's [`install.sh`](install.sh), so there is one source of truth for install logic.

## Inputs

| Input | Description | Default |
|-------|-------------|---------|
| `version` | Release version to install (e.g. `v0.1.0` or `0.1.0`). Omit or pass `latest` to resolve the latest release dynamically. | `` (latest) |
| `cache` | Cache the installed binaries with `actions/cache`. | `true` |
| `github-token` | Token used to query the GitHub API when resolving the latest release. | `${{ github.token }}` |

## Outputs

| Output | Description |
|--------|-------------|
| `version` | The resolved version that was installed (without the `v` prefix). |
| `install-dir` | Directory the binaries were installed into (added to `PATH`). |

## Usage

```yaml
# Latest release, cached (default):
- uses: Goldziher/poly@v0

# Pin a version:
- uses: Goldziher/poly@v0
  with:
    version: v0.5.0

# Disable caching:
- uses: Goldziher/poly@v0
  with:
    cache: false
```

Full example:

```yaml
name: Lint and Format
on: [push, pull_request]
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: Goldziher/poly@v0
      - run: poly lint .
      - run: poly fmt --check .
```

## Pinning is a real pin, not verify-after-the-fact

`version:` is a genuine pin, not just a presence check: the action forwards it verbatim to
`install.sh --no-modify-path <version>`, which downloads that exact tag's release archive
(`poly-<version>-<triple>.tar.gz`/`.zip`) and refuses to install it unless it matches the
checksum published in that release's `sha256sums.txt`. There is no "already installed, skip" path
— every run installs (or restores from cache) precisely the requested version.

Two independent things can be pinned, and both matter for reproducibility:

- **`version:`** — which `poly` release gets installed. Pin this to an exact version
  (`v0.20.0`, not `latest`) so lint/format output can't drift between runs.
- **The action reference itself** (`Goldziher/poly@v0`) — which cut of *this action's* code
  runs. `@v0` is a moving major-version tag (`update_major_tag` in `publish.yaml` re-points it
  at every non-prerelease release), so it tracks the newest `v0.x.y` action logic, not a fixed
  commit. For supply-chain-grade pinning of the action itself, reference a full commit SHA
  instead of `@v0` (`uses: Goldziher/poly@<40-char-sha>`) — standard GitHub Actions pinning
  practice, independent of the `version:` input above.

## Caching & version invalidation

When `cache: true` (default), the cache key is `poly-v<version>-<os>-<arch>` with the **resolved**
version embedded. A version bump therefore produces a new key — the previous version's cache is not
restored (invalidated), and the new version runs the full download → checksum → install flow once,
then populates the cache for subsequent runs.

## Cross-platform

Targets follow the release matrix and are resolved inside `install.sh`: Linux
(`x86_64`/`aarch64`, glibc & musl auto-detected), macOS (`x86_64`/`aarch64`), Windows (`x86_64`).
