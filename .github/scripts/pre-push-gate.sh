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
if [[ -z "$cargo_version" || "$cargo_version" != "$package_version" ||
      "$cargo_version" != "$tauri_version" || "$cargo_version" != "$snap_version" ]]; then
    printf 'R10 version drift: cargo=%s package=%s tauri=%s snap=%s\n' \
        "$cargo_version" "$package_version" "$tauri_version" "$snap_version" >&2
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
