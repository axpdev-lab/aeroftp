#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# Keep aligned with the deterministic checks run by GitHub Actions. Task-specific
# tests supplement this gate; they never replace it.
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
node .github/scripts/security-regression.cjs

cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' src-tauri/Cargo.toml | head -n 1)"
package_version="$(node -p "require('./package.json').version")"
tauri_version="$(node -p "require('./src-tauri/tauri.conf.json').version")"
snap_version="$(sed -n "s/^version: ['\"]\\(.*\\)['\"]/\1/p" snap/snapcraft.yaml | head -n 1)"
# package-lock.json carries the app version in two places. It is NOT cosmetic:
# `npm ci` (checks.yml, build.yml) hard-fails on a lock whose version disagrees
# with package.json, so a bump that misses the lock goes green locally and red
# in CI *after* the tag is pushed. Both entries are checked.
lock_version="$(node -p "require('./package-lock.json').version")"
lock_pkg_version="$(node -p "require('./package-lock.json').packages[''].version")"
# Cargo.lock re-locks on any cargo build, but a bump committed without one
# leaves the shipped binary's own crate version stale.
cargo_lock_version="$(node -p "
    const fs = require('fs');
    const lock = fs.readFileSync('src-tauri/Cargo.lock', 'utf8');
    const m = lock.match(/\[\[package\]\]\nname = \"aeroftp\"\nversion = \"([^\"]+)\"/);
    m ? m[1] : '';
")"
# public/splash.html hardcodes the version: Tauri IPC is unavailable in the
# splash window, so nothing resolves it at runtime (missed in v2.2.3).
splash_version="$(sed -n 's/.*class="version">v\([0-9][^ <]*\).*/\1/p' public/splash.html | head -n 1)"
if [[ -z "$cargo_version" || "$cargo_version" != "$package_version" ||
      "$cargo_version" != "$tauri_version" || "$cargo_version" != "$snap_version" ||
      "$cargo_version" != "$lock_version" || "$cargo_version" != "$lock_pkg_version" ||
      "$cargo_version" != "$cargo_lock_version" || "$cargo_version" != "$splash_version" ]]; then
    printf 'R10 version drift: cargo=%s package=%s tauri=%s snap=%s package-lock=%s/%s cargo-lock=%s splash=%s\n' \
        "$cargo_version" "$package_version" "$tauri_version" "$snap_version" \
        "$lock_version" "$lock_pkg_version" "$cargo_lock_version" "$splash_version" >&2
    exit 1
fi

npm run test:unit
npm run typecheck
npm run i18n:validate
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
(
    cd src-tauri
    cargo audit
)
