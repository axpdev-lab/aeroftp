#!/usr/bin/env node
// Tauri 2.11.2 prepare_tools trusts existence, not content. Seed its *local*
// cache and verify the entire set before it can launch linuxdeploy.
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const pins = require("./linuxdeploy-pins.json");

function digest(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function verifyHelper(file, pin, downloaded = false) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.nlink !== 1) {
    throw new Error(`Helper must be a regular, unlinked file: ${file}`);
  }
  const actual = digest(file);
  // prepare_tools zeroes the AppImage magic at offsets 8..10 with dd.
  // Accept only the two complete, precomputed hashes, never mask arbitrary
  // bytes before hashing. Downloads must match the upstream asset verbatim.
  if (actual !== pin.sha256 && (downloaded || actual !== pin.bundlerSha256)) {
    throw new Error(`SHA-256 mismatch for ${pin.name}: expected ${pin.sha256}, got ${actual}`);
  }
}

function checkCache(cache, helpers) {
  const stat = fs.lstatSync(cache);
  if (!stat.isDirectory() || (stat.mode & 0o022)) {
    throw new Error(`Helper cache must be a directory writable only by its owner: ${cache}`);
  }
  const allowed = new Set(helpers.map((pin) => pin.name));
  for (const name of fs.readdirSync(cache)) {
    if (!allowed.has(name)) throw new Error(`Unverified helper/cache entry: ${path.join(cache, name)}`);
    const entry = fs.lstatSync(path.join(cache, name));
    if (!entry.isFile() || entry.nlink !== 1) throw new Error(`Helper must be a regular, unlinked file: ${name}`);
  }
}

function rejectOtherPlugins(cache, directories) {
  // linuxdeploy probes *all* plugins for their API level before selecting a
  // name, even duplicates shadowed by our plugins. Audit PATH and cwd before
  // that probing can execute third-party code. Bundled plugins are covered
  // by linuxdeploy's own digest; external plugins must be in the pinned set.
  for (const directory of new Set(directories.map((dir) => path.resolve(dir || ".")))) {
    if (!fs.existsSync(directory) || !fs.statSync(directory).isDirectory()) continue;
    if (fs.realpathSync(directory) === fs.realpathSync(cache)) continue;
    for (const name of fs.readdirSync(directory)) {
      if (name.startsWith("linuxdeploy-plugin-")) {
        throw new Error(`Unverified plugin in linuxdeploy search path: ${path.join(directory, name)}`);
      }
    }
  }
}

function download(url, file) {
  const result = spawnSync("curl", [
    "--fail", "--location", "--silent", "--show-error", "--retry", "3",
    "--connect-timeout", "20", "--max-time", "180",
    "--proto", "=https", "--proto-redir", "=https", "--output", file, url,
  ], { stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Helper download failed (${result.status}): ${url}`);
}

function prepare(cache, helpers, { fetch = download, verifyOnly = false, searchDirs = [] } = {}) {
  fs.mkdirSync(cache, { recursive: true, mode: 0o700 });
  checkCache(cache, helpers);
  rejectOtherPlugins(cache, searchDirs);
  // Check every existing entry before downloading anything. A corrupt warm
  // cache is an error, not an excuse to silently replace the evidence.
  for (const pin of helpers) {
    const file = path.join(cache, pin.name);
    if (fs.existsSync(file)) verifyHelper(file, pin);
    else if (verifyOnly) throw new Error(`Missing verified helper: ${file}`);
  }
  for (const pin of helpers) {
    const file = path.join(cache, pin.name);
    if (!fs.existsSync(file)) {
      // Temp files stay outside the cache inventory, and are never executable
      // until their bytes have passed SHA-256. rename publishes atomically.
      const tempDir = fs.mkdtempSync(path.join(path.dirname(cache), ".linuxdeploy-download-"));
      const temp = path.join(tempDir, pin.name);
      try {
        fs.writeFileSync(temp, "", { mode: 0o600, flag: "wx" });
        fetch(pin.url, temp);
        verifyHelper(temp, pin, true);
        fs.chmodSync(temp, 0o700);
        fs.renameSync(temp, file);
      } finally {
        fs.rmSync(tempDir, { recursive: true, force: true });
      }
    }
    verifyHelper(file, pin);
    fs.chmodSync(file, 0o700);
    console.log(`Verified ${pin.name}: ${digest(file)}`);
  }
  checkCache(cache, helpers);
}

function projectCache(root, env = process.env) {
  const cli = require(path.join(root, "node_modules/@tauri-apps/cli/package.json"));
  const locked = require(path.join(root, "package-lock.json")).packages["node_modules/@tauri-apps/cli"];
  if (cli.version !== pins.tauriCli || locked.version !== pins.tauriCli) {
    throw new Error(`Re-audit Tauri helper downloads before changing CLI ${pins.tauriCli}`);
  }
  const architecture = env.TAURI_ENV_ARCH || (process.arch === "x64" ? "x86_64" : process.arch);
  if (architecture !== pins.architecture) {
    throw new Error(`No verified linuxdeploy pins for architecture ${architecture}`);
  }
  const config = JSON.parse(fs.readFileSync(path.join(root, "src-tauri/tauri.linux.conf.json")));
  const override = JSON.parse(env.TAURI_CONFIG || "{}");
  if ((override.bundle?.useLocalToolsDir ?? config.bundle.useLocalToolsDir) !== true) {
    throw new Error("Linux packaging requires bundle.useLocalToolsDir=true");
  }
  const result = spawnSync("cargo", ["metadata", "--offline", "--no-deps", "--format-version", "1"], {
    cwd: path.join(root, "src-tauri"), env, encoding: "utf8",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Cannot locate Tauri's tool cache: ${result.stderr}`);
  return path.join(JSON.parse(result.stdout).target_directory, ".tauri");
}

if (require.main === module) {
  try {
    const root = path.resolve(__dirname, "..");
    if (process.argv.slice(2).some((arg) => arg !== "--verify")) throw new Error("Usage: prepare-linuxdeploy.cjs [--verify]");
    const cache = projectCache(root);
    prepare(cache, pins.helpers, {
      verifyOnly: process.argv.includes("--verify"),
      searchDirs: [...(process.env.PATH || "").split(path.delimiter), process.cwd(), root, path.join(root, "src-tauri")],
    });
  } catch (error) {
    console.error(`linuxdeploy verification failed: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { prepare, projectCache, verifyHelper, rejectOtherPlugins };
