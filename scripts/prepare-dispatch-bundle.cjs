#!/usr/bin/env node

const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const cwd = process.cwd();
const repoRoot = fs.existsSync(path.join(cwd, "src-tauri", "Cargo.toml"))
  ? cwd
  : path.resolve(cwd, "..");
const srcTauri = path.join(repoRoot, "src-tauri");
const releaseDir = path.join(srcTauri, "target", "release");
const stageDir = path.join(releaseDir, "aeroftp-dispatch-bundle");
const platform = process.env.TAURI_ENV_PLATFORM || process.platform;

if (platform !== "linux") {
  console.log(`prepare-dispatch-bundle: skipping on ${platform}`);
  process.exit(0);
}

function run(command, args) {
  // Run cargo from src-tauri (the Tauri manifest dir), exactly like Tauri's
  // own build invocation. Tauri builds the GUI binary with cwd=src-tauri, so
  // it loads src-tauri/.cargo/config.toml. If this hook ran cargo from the
  // repo root instead, Cargo would load a different config set and compute a
  // different fingerprint, clobbering the shared target dir and forcing a
  // full ~7 min recompile of the whole crate on every bundle invocation.
  // Matching the cwd makes the extra bins a cache hit off the GUI build:
  // only the two small bin targets compile.
  const result = spawnSync(command, args, {
    cwd: srcTauri,
    stdio: "inherit",
  });
  if (result.status !== 0) {
    process.exit(result.status || 1);
  }
}

run("cargo", [
  "build",
  "--release",
  "--bin",
  "aeroftp-cli",
  "--bin",
  "aeroftp-dispatch",
]);

fs.mkdirSync(stageDir, { recursive: true });
for (const name of ["aeroftp-cli", "aeroftp-dispatch"]) {
  const src = path.join(releaseDir, name);
  const dst = path.join(stageDir, name);
  fs.copyFileSync(src, dst);
  fs.chmodSync(dst, 0o755);
}

console.log(`prepare-dispatch-bundle: staged payloads in ${stageDir}`);
