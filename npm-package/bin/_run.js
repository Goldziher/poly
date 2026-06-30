const { spawnSync } = require("node:child_process");
const path = require("node:path");
const os = require("node:os");
const fs = require("node:fs");

// Shared launcher for the three thin bin/ shims (poly, polylint, polyfmt). Each
// shim calls run("<base>"); this resolves the platform binary, self-heals once
// by running install.js if it is missing, then execs it with the user's args.
module.exports = function run(base) {
  const binaryName = os.type() === "Windows_NT" ? `${base}.exe` : base;
  const binaryPath = path.join(__dirname, binaryName);
  const packageJsonPath = path.join(__dirname, "..", "package.json");

  function ensureBinaryExists() {
    if (fs.existsSync(binaryPath)) {
      return true;
    }

    // Binary is missing. Attempt a one-time self-heal by running the postinstall script.
    console.error(`${base}: binary not found at ${binaryPath}. Running install...`);

    const installScriptPath = path.join(__dirname, "..", "install.js");
    if (!fs.existsSync(installScriptPath)) {
      console.error(`${base}: install script not found at ${installScriptPath}`);
      return false;
    }

    // Run the install script with stdio inherit so the user sees progress/errors
    const installResult = spawnSync(process.execPath, [installScriptPath], {
      stdio: "inherit",
      cwd: path.dirname(packageJsonPath),
    });

    // Check if the install succeeded and the binary now exists
    if (installResult.status === 0 && fs.existsSync(binaryPath)) {
      return true;
    }

    return false;
  }

  if (!ensureBinaryExists()) {
    console.error(
      `${base}: native binary not found at ${binaryPath}.\n` +
        `The postinstall step that downloads the binary from GitHub releases may have failed.\n` +
        `You can try:\n` +
        `  1. Reinstall: npm install -g poly-lint\n` +
        `  2. Download a release binary from https://github.com/Goldziher/polylint/releases`,
    );
    process.exit(1);
  }

  const result = spawnSync(binaryPath, process.argv.slice(2), { stdio: "inherit" });
  if (result.error) {
    console.error(`${base}: failed to spawn binary: ${result.error.message}`);
    process.exit(1);
  }
  process.exit(result.status ?? 0);
};
