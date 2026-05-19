#!/usr/bin/env node
const { execFileSync } = require("child_process");
const path = require("path");
const binPath = path.join(__dirname, "bin", "cc-email");
try {
  execFileSync(binPath, process.argv.slice(2), { stdio: "inherit" });
} catch (e) {
  process.exit(e.status || 1);
}
