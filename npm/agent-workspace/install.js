// ============================================================
// Postinstall: Verify Platform Package & Setup Shell Integration
// ============================================================

const { execFileSync } = require("child_process");
const { join } = require("path");

const PLATFORMS = {
  "darwin-arm64": "@ZelAnton/agent-workspace-darwin-arm64",
  "darwin-x64": "@ZelAnton/agent-workspace-darwin-x64",
  "linux-x64": "@ZelAnton/agent-workspace-linux-x64",
  "win32-x64": "@ZelAnton/agent-workspace-win32-x64",
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
