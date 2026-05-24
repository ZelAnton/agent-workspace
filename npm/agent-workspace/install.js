// ============================================================
// Postinstall: Verify Platform Package & Setup Shell Integration
// ============================================================
//
// Runs after `npm install -g @zelanton/agent-workspace`. Responsibilities:
//   1. Resolve the platform-specific binary that npm installed as an
//      optional dependency.
//   2. Invoke `wt setup` to install shell-wrapper functions in the user's
//      rc files (the wrapper is required for `wt cd`, `wt new`, etc. to
//      actually change shell cwd).
//   3. Stamp the install-channel marker file so `wt update` knows to
//      re-invoke npm rather than self-replace from GitHub Releases.

const { execFileSync } = require("child_process");
const { join } = require("path");
const fs = require("fs");
const os = require("os");

const PLATFORMS = {
  "darwin-arm64": "@zelanton/agent-workspace-darwin-arm64",
  "linux-x64": "@zelanton/agent-workspace-linux-x64",
  "win32-x64": "@zelanton/agent-workspace-win32-x64",
};

const key = `${process.platform}-${process.arch}`;
const pkg = PLATFORMS[key];

if (!pkg) {
  console.warn(`[agent-workspace] Warning: Unsupported platform ${key}`);
  console.warn(`[agent-workspace] Supported: ${Object.keys(PLATFORMS).join(", ")}`);
  process.exit(0);
}

let pkgJsonPath;
try {
  pkgJsonPath = require.resolve(`${pkg}/package.json`);
} catch {
  console.warn(`[agent-workspace] Warning: Platform package ${pkg} not installed`);
  console.warn(`[agent-workspace] This may happen if npm failed to install optional dependencies`);
  process.exit(0);
}

// Run 'wt setup' to install shell integration
const exe = process.platform === "win32" ? "wt.exe" : "wt";
const binaryPath = join(pkgJsonPath, "..", "bin", exe);
try {
  execFileSync(binaryPath, ["setup"], { stdio: "inherit" });
} catch {
  console.warn("[agent-workspace] Auto-setup failed. Run 'wt setup' manually.");
}

// Stamp the install-channel marker so `wt update` re-invokes npm rather than
// self-replacing from GitHub Releases. Honors AGENT_WORKSPACE_DIR to match the
// Rust side's Config::base_dir() resolution.
try {
  const baseDir = process.env.AGENT_WORKSPACE_DIR || join(os.homedir(), ".agent-workspace");
  fs.mkdirSync(baseDir, { recursive: true });
  fs.writeFileSync(join(baseDir, "install_channel"), "npm");
} catch (err) {
  console.warn(`[agent-workspace] Could not write install_channel marker: ${err.message}`);
}
