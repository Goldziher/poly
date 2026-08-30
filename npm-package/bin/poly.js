#!/usr/bin/env node
"use strict";

// poly: allow-file[no-console] this launcher is a CLI entry point; stderr is its only output channel.

// Thin launcher: resolve the platform binary npm installed, run it with this
// process's arguments, and propagate its exit status faithfully — poly's exit
// codes (0 clean, 1 findings, 2 the run verified less than it claims) are part
// of its contract, so swallowing or remapping them would break CI gates.

const { spawnSync } = require("node:child_process");
const { resolveBinaryPath, unsupportedPlatformMessage } = require("../index.js");

const binary = resolveBinaryPath();

if (binary === null) {
  console.error(unsupportedPlatformMessage());
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  console.error(`poly: failed to run ${binary}: ${result.error.message}`);
  process.exit(1);
}

if (result.signal) {
  // Re-raise so the parent shell sees a signal death rather than a plain exit.
  process.kill(process.pid, result.signal);
}

process.exit(result.status ?? 1);
