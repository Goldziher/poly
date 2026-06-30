const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const https = require("node:https");
const http = require("node:http");
const crypto = require("node:crypto");
const { execFileSync } = require("node:child_process");
const AdmZip = require("adm-zip");

// tar v7 is ESM-only, so it is pulled in via dynamic import() at the extract
// site rather than a top-level require (this file is CommonJS).

const { version } = require("./package.json");

// poly ships one standalone binary in the archive root.
const BINARY_BASENAMES = ["poly"];

// `os.arch()` reflects the Node process arch, so an x64 Node under Rosetta reports
// "x64" even on Apple Silicon hardware. Probe a hardware-level signal the translation
// layer cannot spoof: `sysctl -n hw.optional.arm64` is "1" on Apple Silicon.
function isAppleSilicon() {
  if (os.type() !== "Darwin") return false;
  if (os.arch() === "arm64") return true;
  try {
    return (
      execFileSync("sysctl", ["-n", "hw.optional.arm64"], {
        encoding: "utf8",
      }).trim() === "1"
    );
  } catch {
    return false;
  }
}

function getPlatformTriple() {
  const type = os.type();
  const arch = os.arch();

  if (type === "Windows_NT") {
    if (arch === "x64") return "x86_64-pc-windows-msvc";
    if (arch === "ia32") throw new Error("32-bit Windows is not supported");
  }

  if (type === "Linux") {
    if (arch === "x64") return "x86_64-unknown-linux-gnu";
    if (arch === "arm64") return "aarch64-unknown-linux-gnu";
    return "x86_64-unknown-linux-gnu";
  }

  if (type === "Darwin") {
    if (isAppleSilicon()) return "aarch64-apple-darwin";
    return "x86_64-apple-darwin";
  }

  throw new Error(`Unsupported platform: ${type} ${arch}`);
}

function getReleaseAssets() {
  const platform = getPlatformTriple();
  const baseUrl = `https://github.com/Goldziher/polylint/releases/download/v${version}`;
  const ext = platform.includes("windows") ? "zip" : "tar.gz";
  const assetName = `poly-${version}-${platform}.${ext}`;
  return {
    assetName,
    archiveUrl: `${baseUrl}/${assetName}`,
    checksumsUrl: `${baseUrl}/sha256sums.txt`,
  };
}

function downloadWithRedirects(url, dest, maxRedirects = 5) {
  return new Promise((resolve, reject) => {
    if (maxRedirects <= 0) {
      return reject(new Error("Too many redirects"));
    }

    const urlObj = new URL(url);
    const client = urlObj.protocol === "https:" ? https : http;

    const req = client.get(
      url,
      {
        headers: {
          "User-Agent": "poly-npm-wrapper",
        },
      },
      (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return downloadWithRedirects(res.headers.location, dest, maxRedirects - 1)
            .then(resolve)
            .catch(reject);
        }

        if (res.statusCode !== 200) {
          return reject(new Error(`HTTP ${res.statusCode}: ${res.statusMessage}`));
        }

        const file = fs.createWriteStream(dest);
        res.pipe(file);

        file.on("finish", () => {
          file.close();
          resolve();
        });

        file.on("error", (err) => {
          fs.unlink(dest, () => {});
          reject(err);
        });
      },
    );

    req.on("error", reject);
    req.setTimeout(30000, () => {
      req.destroy();
      reject(new Error("Download timeout"));
    });
  });
}

// Retry-with-exponential-backoff wrapper. Retries on network errors, 5xx, and timeout.
// Does NOT retry on 404 (deterministic failure). Returns error on 4xx (except retryable timeout).
function retryWithBackoff(fn, maxAttempts = 3) {
  const delays = [1000, 2000, 4000]; // exponential: 1s, 2s, 4s
  return (async function attempt(index = 0) {
    try {
      return await fn();
    } catch (err) {
      const isRetryable =
        err.message.includes("Download timeout") ||
        err.message.includes("ECONNREFUSED") ||
        err.message.includes("ECONNRESET") ||
        err.message.includes("ETIMEDOUT") ||
        err.message.includes("EHOSTUNREACH") ||
        (err.message.match(/HTTP ([0-9]+)/) && parseInt(RegExp.$1) >= 500);

      if (!isRetryable || index >= maxAttempts - 1) {
        throw err;
      }

      const delay = delays[index];
      console.log(`Transient error (attempt ${index + 1}/${maxAttempts}): ${err.message}; retrying in ${delay}ms...`);
      await new Promise((resolve) => setTimeout(resolve, delay));
      return attempt(index + 1);
    }
  })();
}

// Download a (small) text resource into memory, following redirects.
function fetchTextWithRedirects(url, maxRedirects = 5) {
  return new Promise((resolve, reject) => {
    if (maxRedirects <= 0) {
      return reject(new Error("Too many redirects"));
    }

    const urlObj = new URL(url);
    const client = urlObj.protocol === "https:" ? https : http;

    const req = client.get(
      url,
      {
        headers: {
          "User-Agent": "poly-npm-wrapper",
        },
      },
      (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return fetchTextWithRedirects(res.headers.location, maxRedirects - 1)
            .then(resolve)
            .catch(reject);
        }

        if (res.statusCode !== 200) {
          return reject(new Error(`HTTP ${res.statusCode}: ${res.statusMessage}`));
        }

        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
        res.on("error", reject);
      },
    );

    req.on("error", reject);
    req.setTimeout(30000, () => {
      req.destroy();
      reject(new Error("Download timeout"));
    });
  });
}

// Retry wrapper for text fetches (same retry policy as binary downloads).
function retryFetchText(url) {
  return retryWithBackoff(() => fetchTextWithRedirects(url));
}

function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

// Parse a `sha256<space>filename` checksums file and return the digest for
// `assetName`, or null if absent. Lines may use one or two spaces (GNU coreutils
// uses two: binary-mode marker "* "), and entries may carry a leading "./", so
// strip an optional "./" prefix and "*" binary marker before comparing.
function expectedDigest(checksumsText, assetName) {
  for (const line of checksumsText.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const parts = trimmed.split(/\s+/);
    if (parts.length < 2) continue;
    const digest = parts[0];
    const name = parts[parts.length - 1].replace(/^\*/, "").replace(/^\.\//, "");
    if (name === assetName) return digest.toLowerCase();
  }
  return null;
}

// Verify the downloaded archive against the release checksums file. Fails CLOSED:
// any failure to fetch the checksums, locate the entry, or match the digest
// aborts the install. Uses retry-with-backoff for transient failures.
async function verifyChecksum(archivePath, assetName, checksumsUrl) {
  let checksumsText;
  try {
    checksumsText = await retryFetchText(checksumsUrl);
  } catch (error) {
    throw new Error(
      `could not fetch checksums (${checksumsUrl}): ${error.message} — refusing to install unverified binary`,
    );
  }

  const expected = expectedDigest(checksumsText, assetName);
  if (!expected) {
    throw new Error(`no checksum entry for ${assetName} in ${checksumsUrl} — refusing to install unverified binary`);
  }

  const actual = sha256File(archivePath).toLowerCase();
  if (actual !== expected) {
    throw new Error(`checksum mismatch for ${assetName} (expected ${expected}, got ${actual})`);
  }

  console.log("Checksum verified.");
}

async function installBinary() {
  try {
    const { assetName, archiveUrl, checksumsUrl } = getReleaseAssets();
    const isZip = archiveUrl.endsWith(".zip");
    const isWindows = os.type() === "Windows_NT";
    const binDir = path.join(__dirname, "bin");
    const archivePath = path.join(binDir, assetName);

    // The standalone binary the archive root is expected to contain.
    const binaryPaths = BINARY_BASENAMES.map((base) => path.join(binDir, isWindows ? `${base}.exe` : base));

    if (!fs.existsSync(binDir)) {
      fs.mkdirSync(binDir, { recursive: true });
    }

    // Skip the download only when the binary is already present.
    if (binaryPaths.every((p) => fs.existsSync(p))) {
      return;
    }

    console.log(`Downloading poly binary from ${archiveUrl}...`);

    await retryWithBackoff(() => downloadWithRedirects(archiveUrl, archivePath));

    // Fail CLOSED: verify the archive against the release checksums before
    // extracting anything. Any fetch/parse/mismatch failure aborts the install.
    await verifyChecksum(archivePath, assetName, checksumsUrl);

    console.log("Extracting archive (poly)...");

    // The archive root contains the standalone binary; there is no bundled lib/
    // tree. Extract the whole archive into bin/.
    if (isZip) {
      const zip = new AdmZip(archivePath);
      zip.extractAllTo(binDir, true);
    } else {
      const { extract } = await import("tar");
      await extract({
        file: archivePath,
        cwd: binDir,
      });
    }

    fs.unlinkSync(archivePath);

    // Verify each expected binary exists, and make them executable on Unix.
    for (const binaryPath of binaryPaths) {
      if (!fs.existsSync(binaryPath)) {
        throw new Error(`binary ${path.basename(binaryPath)} not found after extracting ${assetName}`);
      }
      if (!isWindows) {
        fs.chmodSync(binaryPath, 0o755);
      }
    }

    console.log("poly binary installed successfully!");
  } catch (error) {
    console.error("Error installing poly binary:", error.message);
    process.exit(1);
  }
}

installBinary();
