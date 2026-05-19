const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const os = require("os");

const BIN_NAME = "cc-email";
const REPO = "meloalright/cc-email";

function getTarget() {
  const platform = os.platform();
  const arch = os.arch();

  if (platform === "darwin" && arch === "arm64")
    return "aarch64-apple-darwin";
  if (platform === "darwin" && arch === "x64")
    return "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64")
    return "x86_64-unknown-linux-gnu";
  if (platform === "linux" && arch === "arm64")
    return "aarch64-unknown-linux-gnu";

  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

function install() {
  const target = getTarget();
  const tarball = `${BIN_NAME}-${target}.tar.gz`;
  const url = `https://github.com/${REPO}/releases/latest/download/${tarball}`;
  const binDir = path.join(__dirname, "bin");

  fs.mkdirSync(binDir, { recursive: true });

  const tmp = path.join(os.tmpdir(), `${BIN_NAME}-${Date.now()}.tar.gz`);

  try {
    execSync(`curl -fsSL "${url}" -o "${tmp}"`, { stdio: "pipe" });
    execSync(`tar xzf "${tmp}" -C "${binDir}" ${BIN_NAME}`, { stdio: "pipe" });
    fs.chmodSync(path.join(binDir, BIN_NAME), 0o755);
  } finally {
    try { fs.unlinkSync(tmp); } catch {}
  }

  console.log(`${BIN_NAME} installed successfully`);
}

install();
