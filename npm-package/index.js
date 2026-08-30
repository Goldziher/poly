"use strict";

// Resolves the prebuilt `poly` binary that npm installed for this host.
//
// Distribution model: this package ships no binary at all. The six platform
// packages listed under `optionalDependencies` each ship exactly one, and npm
// installs only the ones whose `os`/`cpu`/`libc` match the host — so there is no
// postinstall download, the install works offline, and the registry's own
// integrity hashes cover the binary.

const fs = require("node:fs");
const path = require("node:path");

// One entry per release target. `libc` is only meaningful on Linux; the two
// x64 Linux binaries are NOT interchangeable — the musl build is dynamically
// linked against musl and the gnu build against glibc, so each fails to start
// on the other's userland. npm's own `libc` field keeps the wrong one from
// being installed on a modern npm, and `detectLibc()` below picks correctly
// even on a package manager that ignores the field and installs both.
const PLATFORM_PACKAGES = [
  { pkg: "@goldziher/polylint-darwin-arm64", platform: "darwin", arch: "arm64", libc: null },
  { pkg: "@goldziher/polylint-darwin-x64", platform: "darwin", arch: "x64", libc: null },
  { pkg: "@goldziher/polylint-linux-arm64-gnu", platform: "linux", arch: "arm64", libc: "glibc" },
  { pkg: "@goldziher/polylint-linux-x64-gnu", platform: "linux", arch: "x64", libc: "glibc" },
  { pkg: "@goldziher/polylint-linux-x64-musl", platform: "linux", arch: "x64", libc: "musl" },
  { pkg: "@goldziher/polylint-win32-x64", platform: "win32", arch: "x64", libc: null },
];

const BINARY_NAME = process.platform === "win32" ? "poly.exe" : "poly";

/**
 * Report whether this Linux host runs glibc or musl.
 *
 * `process.report.getReport().header.glibcVersionRuntime` is present only when
 * the running Node was linked against glibc, which is the check `detect-libc`
 * makes; doing it inline keeps this package dependency-free.
 */
function detectLibc() {
  if (process.platform !== "linux") return null;
  try {
    const report = typeof process.report?.getReport === "function" ? process.report.getReport() : null;
    if (report && report.header && report.header.glibcVersionRuntime) return "glibc";
    if (report) return "musl";
  } catch {
    // Fall through to the conservative default below.
  }
  // A Node build without process.report is old enough that glibc is the safer
  // guess: musl hosts are overwhelmingly Alpine, which ships current Node.
  return "glibc";
}

/** The platform packages that could serve this host, best match first. */
function candidatePackages() {
  const libc = detectLibc();
  return PLATFORM_PACKAGES.filter(
    (entry) =>
      entry.platform === process.platform &&
      entry.arch === process.arch &&
      (entry.libc === null || entry.libc === libc),
  ).map((entry) => entry.pkg);
}

/**
 * Absolute path of the installed `poly` binary, or `null` when no platform
 * package for this host is present.
 */
function resolveBinaryPath() {
  for (const pkg of candidatePackages()) {
    let resolved;
    try {
      resolved = require.resolve(`${pkg}/bin/${BINARY_NAME}`);
    } catch {
      continue;
    }
    if (fs.existsSync(resolved)) return resolved;
  }

  // Yarn PnP and some hoisting layouts do not expose the file through
  // `require.resolve`; fall back to the package directory itself.
  for (const pkg of candidatePackages()) {
    let packageJson;
    try {
      packageJson = require.resolve(`${pkg}/package.json`);
    } catch {
      continue;
    }
    const candidate = path.join(path.dirname(packageJson), "bin", BINARY_NAME);
    if (fs.existsSync(candidate)) return candidate;
  }

  return null;
}

/** A human-readable explanation of why no binary was found. */
function unsupportedPlatformMessage() {
  const supported = PLATFORM_PACKAGES.map(
    (entry) => `  ${entry.platform}/${entry.arch}${entry.libc ? ` (${entry.libc})` : ""}`,
  ).join("\n");
  const host = `${process.platform}/${process.arch}${detectLibc() ? ` (${detectLibc()})` : ""}`;
  const expected = candidatePackages();

  return expected.length === 0
    ? `poly: no prebuilt binary is published for ${host}.\n\n` +
        `Supported platforms:\n${supported}\n\n` +
        `Build from source instead: https://github.com/Goldziher/poly#installation`
    : `poly: the platform package for ${host} is not installed.\n\n` +
        `Expected one of: ${expected.join(", ")}\n\n` +
        `This usually means the install ran with optional dependencies disabled\n` +
        `(npm install --no-optional, --omit=optional, or an offline cache that\n` +
        `predates this version). Reinstall with optional dependencies enabled:\n\n` +
        `  npm install --include=optional @goldziher/polylint\n\n` +
        `Or install poly directly: https://github.com/Goldziher/poly#installation`;
}

module.exports = { resolveBinaryPath, unsupportedPlatformMessage, PLATFORM_PACKAGES };
