const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const { prepare, verifyHelper, projectCache } = require("../../scripts/prepare-linuxdeploy.cjs");

const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const names = require("../../scripts/linuxdeploy-pins.json").helpers.map((pin) => pin.name);

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "linuxdeploy-pins-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const cache = path.join(root, ".tauri");
  const contents = new Map(names.map((name) => [name, Buffer.from(`verified fixture: ${name}\n`)]));
  const helpers = names.map((name) => ({ name, url: `https://fixture.invalid/${name}`, sha256: hash(contents.get(name)) }));
  const downloads = [];
  const fetch = (url, file) => {
    downloads.push(url);
    assert.equal(fs.statSync(file).mode & 0o111, 0, "unverified download is not executable");
    fs.writeFileSync(file, contents.get(path.basename(file)));
  };
  return { root, cache, contents, helpers, downloads, fetch };
}

test("empty cache publishes all five verified executable helpers atomically", (t) => {
  const f = fixture(t);
  prepare(f.cache, f.helpers, { fetch: f.fetch });
  assert.equal(f.downloads.length, 5);
  assert.deepEqual(fs.readdirSync(f.cache).sort(), names.slice().sort());
  for (const pin of f.helpers) {
    verifyHelper(path.join(f.cache, pin.name), pin);
    assert.equal(fs.statSync(path.join(f.cache, pin.name)).mode & 0o777, 0o700);
  }
});

test("warm cache is rehashed and needs no network, also in verify-only mode", (t) => {
  const f = fixture(t);
  prepare(f.cache, f.helpers, { fetch: f.fetch });
  const fetch = () => assert.fail("warm cache attempted a download");
  prepare(f.cache, f.helpers, { fetch });
  prepare(f.cache, f.helpers, { fetch, verifyOnly: true });
});

for (const name of names) {
  test(`corrupt cached ${name} fails before downloads or packaging`, (t) => {
    const f = fixture(t);
    prepare(f.cache, f.helpers, { fetch: f.fetch });
    const file = path.join(f.cache, name);
    fs.appendFileSync(file, "tampered");
    const observed = fs.readFileSync(file);
    assert.throws(() => prepare(f.cache, f.helpers, { fetch: () => assert.fail("retried checksum failure") }), /SHA-256 mismatch/);
    assert.deepEqual(fs.readFileSync(file), observed, "preserves the bad cache for investigation");
  });
}

test("wrong downloaded checksum never publishes an executable or leaves a partial", (t) => {
  const f = fixture(t);
  assert.throws(() => prepare(f.cache, f.helpers, {
    fetch: (_url, file) => fs.writeFileSync(file, "bad download"),
  }), /SHA-256 mismatch/);
  assert.deepEqual(fs.readdirSync(f.cache), []);
  assert.deepEqual(fs.readdirSync(f.root), [".tauri"]);
});

test("failed network download leaves no partial helper; next attempt can recover", (t) => {
  const f = fixture(t);
  assert.throws(() => prepare(f.cache, f.helpers, {
    fetch: (_url, file) => { fs.writeFileSync(file, "partial"); throw new Error("network failure"); },
  }), /network failure/);
  assert.deepEqual(fs.readdirSync(f.cache), []);
  prepare(f.cache, f.helpers, { fetch: f.fetch });
});

test("verify-only refuses a missing AppImage plugin instead of permitting fallback", (t) => {
  const f = fixture(t);
  prepare(f.cache, f.helpers, { fetch: f.fetch });
  fs.unlinkSync(path.join(f.cache, "linuxdeploy-plugin-appimage.AppImage"));
  assert.throws(() => prepare(f.cache, f.helpers, { verifyOnly: true }), /Missing verified helper/);
});

test("unknown cache plugin is refused before its API probe can run", (t) => {
  const f = fixture(t);
  prepare(f.cache, f.helpers, { fetch: f.fetch });
  fs.writeFileSync(path.join(f.cache, "linuxdeploy-plugin-evil.sh"), "exit 0\n", { mode: 0o700 });
  assert.throws(() => prepare(f.cache, f.helpers), /Unverified helper\/cache entry/);
});

test("a plugin on PATH or cwd is rejected even when the pinned copy takes precedence", (t) => {
  const f = fixture(t);
  const external = path.join(f.root, "external");
  fs.mkdirSync(external);
  fs.writeFileSync(path.join(external, "linuxdeploy-plugin-gtk.sh"), "exit 0\n", { mode: 0o700 });
  assert.throws(() => prepare(f.cache, f.helpers, { fetch: f.fetch, searchDirs: [external] }), /Unverified plugin in linuxdeploy search path/);
  assert.equal(f.downloads.length, 0);
});

test("symlink and hardlink helpers cannot redirect verification or chmod", (t) => {
  const f = fixture(t);
  fs.mkdirSync(f.cache, { mode: 0o700 });
  const outside = path.join(f.root, "outside");
  fs.writeFileSync(outside, f.contents.get(names[0]), { mode: 0o600 });
  const file = path.join(f.cache, names[0]);
  fs.symlinkSync(outside, file);
  assert.throws(() => prepare(f.cache, f.helpers), /regular, unlinked/);
  fs.unlinkSync(file);
  fs.linkSync(outside, file);
  assert.throws(() => prepare(f.cache, f.helpers), /regular, unlinked/);
  assert.equal(fs.statSync(outside).mode & 0o777, 0o600);
});

test("cache symlinks and directories writable by another user are refused", (t) => {
  const f = fixture(t);
  const outside = path.join(f.root, "outside");
  fs.mkdirSync(outside);
  fs.symlinkSync(outside, f.cache);
  assert.throws(() => prepare(f.cache, f.helpers), /writable only by its owner/);
  fs.unlinkSync(f.cache);
  fs.mkdirSync(f.cache, { mode: 0o777 });
  fs.chmodSync(f.cache, 0o777);
  assert.throws(() => prepare(f.cache, f.helpers), /writable only by its owner/);
});

test("only the exact upstream linuxdeploy and exact dd result are accepted", (t) => {
  const f = fixture(t);
  const file = path.join(f.root, "linuxdeploy");
  const original = Buffer.from("01234567AI2rest-of-verified-image");
  const transformed = Buffer.from(original);
  transformed.fill(0, 8, 11);
  const pin = { name: "linuxdeploy", sha256: hash(original), bundlerSha256: hash(transformed) };
  fs.writeFileSync(file, original);
  verifyHelper(file, pin, true);
  fs.writeFileSync(file, transformed);
  verifyHelper(file, pin);
  assert.throws(() => verifyHelper(file, pin, true), /SHA-256 mismatch/);
  transformed[9] = 1;
  fs.writeFileSync(file, transformed);
  assert.throws(() => verifyHelper(file, pin), /SHA-256 mismatch/);
});

test("CLI upgrade, wrong architecture and disabling local tools fail closed", (t) => {
  const f = fixture(t);
  fs.mkdirSync(path.join(f.root, "node_modules/@tauri-apps/cli"), { recursive: true });
  const cli = path.join(f.root, "node_modules/@tauri-apps/cli/package.json");
  fs.writeFileSync(cli, JSON.stringify({ version: "unreviewed" }));
  fs.writeFileSync(path.join(f.root, "package-lock.json"), JSON.stringify({ packages: { "node_modules/@tauri-apps/cli": { version: "2.11.2" } } }));
  assert.throws(() => projectCache(f.root), /Re-audit Tauri/);
  const realRoot = path.resolve(__dirname, "../..");
  assert.throws(() => projectCache(realRoot, { TAURI_ENV_ARCH: "aarch64" }), /No verified linuxdeploy pins/);
  assert.throws(() => projectCache(realRoot, { TAURI_ENV_ARCH: "x86_64", TAURI_CONFIG: '{"bundle":{"useLocalToolsDir":false}}' }), /requires bundle.useLocalToolsDir/);
});

test("helper cache follows Cargo metadata's target directory, rather than a shared user cache", (t) => {
  const f = fixture(t);
  const target = path.join(f.root, "custom-cargo-target");
  const root = path.resolve(__dirname, "../..");
  assert.equal(projectCache(root, { ...process.env, CARGO_TARGET_DIR: target, TAURI_ENV_ARCH: "x86_64", TAURI_CONFIG: "{}" }), path.join(target, ".tauri"));
});
