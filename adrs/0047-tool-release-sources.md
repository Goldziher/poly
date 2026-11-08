# 0047 — Tool Release Sources: Download Any Upstream Version, Default to Latest

- Status: Accepted
- Date: 2026-11-08
- Amends: ADR 0035
- Related: ADR 0038

## Context

ADR 0035 let `install = "auto"` download a recipe tool, but only a release the overlay pinned: each
`install.releases` entry held fixed URLs and SHA-256 digests. A tool stayed on poly's pin until poly
shipped, a `version` the overlay did not list was refused ("has no pinned release; give `url` and
`sha256`"), and checkstyle's pin was two majors behind upstream. The fix has to keep what made the pins
safe: the binary is verified before it can run, runs are reproducible, and a cached tool works offline.

## Decision

- **The overlay describes a source, not a table.** `install.source` names where releases are published
  and how assets are named: either a GitHub repository (`github`, a `tag` template such as
  `v{version}`) or a plain `base_url`, with one asset per `<os>-<arch>` as a name template (and archive
  `member`). `install.releases` stays as an offline seed: an exact version it lists needs no lookup,
  and `latest` falls back to its newest entry when poly may not use the network and nothing was ever
  resolved. All eleven downloadable tools declare a source; a catalog test checks each source
  reproduces every seeded URL and member byte for byte.
- **`version` defaults to `"latest"`** for a tool poly can download, when `install = "auto"`. Any
  upstream version can be pinned (`version = "2.14.1"`). `url` plus `sha256` remain the manual override.
- **Where `latest` and digests come from.** A GitHub source asks `/releases/latest` (or
  `/releases/tags/<tag>`), honouring `GITHUB_TOKEN`, and takes each asset's digest from the API's
  `digest` field. Hosts without an API (`get.helm.sh`, Maven Central) declare how to find the newest
  version (`latest`: a GitHub repository's latest tag, or a `maven-metadata.xml`) and where digests are
  published (`checksum`: a per-asset sidecar such as `{url}.sha256sum`, or a `<digest>  <name>` list).
  A GitHub release whose API entry has no digest falls back to the source's checksum file. A digest from
  the publisher proves the file is what the publisher recorded, not more; that matches installing the
  tool by hand.
- **`poly.lock` records the answer.** `[[tools]]` entries carry the version and every platform's URL
  and SHA-256, so a lock made on a laptop serves CI on another platform. `poly lock` resolves `latest`
  into the lock and keeps an entry that still satisfies the config; `--upgrade` moves to the newest
  release (an exact `version` is already fixed, so `--upgrade` keeps it); `--check` compares offline
  (the recorded names are the lockable ones, an exact `version` matches), and with `--upgrade` re-resolves
  and compares. A plain `poly lock` also re-resolves an entry that lacks a platform the source now
  publishes. With an entry, a run does no resolution I/O. Re-locking refuses an asset of the locked
  version whose digest changed upstream.
- **Without a lock entry, `latest` is cached** under the poly cache for 24 hours (`resolve_ttl`, per
  tool), with the response's `ETag`/`Last-Modified`; an expired entry is revalidated with
  `If-None-Match`, so a 304 costs no API quota. A refresh that fails keeps the last answer and warns
  once. When `version` is not set (it is implicitly `latest`) and the lookup fails with nothing cached
  to fall back on (a rate limit, an outage), poly uses the newest seeded release, else the tool on
  `PATH`, with one warning; an explicit `version` never falls back and the tool is reported missing. An exact version's digests are immutable and cached per version. Installed binaries are stored
  under `tools/<tool>/<version>/` with a digest stamp and never downloaded again.
- **Resilience.** Every fetch retries connection errors, timeouts, 429, 500, 502, 503 and 504 up to three
  attempts with the exponential backoff already used for `helm dependency build` (`Retry`), honouring
  `Retry-After` up to 30 seconds; other 4xx and a GitHub primary rate limit are not retried. Connect,
  stall and overall deadlines bound each request and each download. A per-tool file lock serialises
  resolution and install across processes; within one run a tool resolves once. `--offline` / `POLY_OFFLINE`
  uses only the lock, the cache and installed binaries, and anything missing is a visible skip.
  Without `install = "auto"` poly never asks the network, but still reads the lock and the cache.
- **Errors name the tool, the URL and the remedy**: a rate limit with its reset time and `GITHUB_TOKEN`,
  a platform upstream does not publish with the platforms it does, a digest mismatch with both digests
  (the bad file is discarded), an unknown version with the closest published ones, and no network with
  nothing cached.

## Consequences

A tool tracks upstream without a poly release, and a user can pin any version. The cost is a new class
of failure at run time (GitHub rate limits, an upstream renaming its assets), which the lock removes for
anyone who commits `poly.lock` and which the cache and seed soften for everyone else. An unlocked
`install = "auto"` tool now follows upstream within a day, so a repository that needs reproducible
output should lock it. Asset name templates are hand-maintained data and break when upstream renames
assets; the error names the template and the platforms poly knows. `GITHUB_TOKEN` is sent only to the
GitHub API, never to a download host.

## Alternatives considered

- **Keep pinning in the overlay and bump it per poly release.** Safe, but a tool is never newer than
  poly, and every version a user wants needs a poly change.
- **Resolve through `gh` or `go install`.** Needs a tool the user may not have, and loses the digest.
- **Always resolve at run time, no lock.** A tool upgrade could change a CI verdict between two runs of
  the same commit.
- **Verify cosign/sigstore signatures.** Worthwhile for tools that publish them (betterleaks,
  golangci-lint) and left for a later ADR.
