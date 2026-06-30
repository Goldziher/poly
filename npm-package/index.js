const path = require("node:path");
const os = require("node:os");

const ext = os.type() === "Windows_NT" ? ".exe" : "";

function binaryPath(base) {
  return path.join(__dirname, "bin", `${base}${ext}`);
}

module.exports = {
  binaryPaths: {
    poly: binaryPath("poly"),
    polylint: binaryPath("polylint"),
    polyfmt: binaryPath("polyfmt"),
  },
};
