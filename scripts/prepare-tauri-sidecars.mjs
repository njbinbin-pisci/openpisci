import { copyFileSync, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const srcTauriDir = join(repoRoot, "src-tauri");
const binariesDir = join(srcTauriDir, "binaries");

function defaultTargetTriple() {
  const { platform, arch } = process;
  if (platform === "win32" && arch === "x64") return "x86_64-pc-windows-msvc";
  if (platform === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (platform === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-gnu";
  if (platform === "linux" && arch === "arm64") return "aarch64-unknown-linux-gnu";
  throw new Error(`Unsupported platform/arch for sidecar staging: ${platform}/${arch}`);
}

function exeSuffix(targetTriple) {
  return targetTriple.includes("windows") ? ".exe" : "";
}

const targetTriple = process.env.TAURI_ENV_TARGET_TRIPLE || defaultTargetTriple();
const suffix = exeSuffix(targetTriple);
const sourceBinary = join(repoRoot, "target", "release", `openpisci-headless${suffix}`);
const stagedBinary = join(
  binariesDir,
  `openpisci-headless-${targetTriple}${suffix}`
);

console.log(`[sidecar] target triple: ${targetTriple}`);
console.log("[sidecar] building openpisci-headless...");
execFileSync(
  "cargo",
  ["build", "-p", "pisci-cli", "--release", "--bin", "openpisci-headless"],
  {
    cwd: srcTauriDir,
    stdio: "inherit",
  }
);

if (!existsSync(sourceBinary)) {
  throw new Error(`[sidecar] built binary missing: ${sourceBinary}`);
}

mkdirSync(binariesDir, { recursive: true });
for (const entry of readdirSync(binariesDir)) {
  if (entry.startsWith("openpisci-headless-")) {
    rmSync(join(binariesDir, entry), { force: true });
  }
}

copyFileSync(sourceBinary, stagedBinary);
console.log(`[sidecar] staged ${stagedBinary}`);
