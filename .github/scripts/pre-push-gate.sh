#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# Keep aligned with the deterministic checks run by GitHub Actions. Task-specific
# tests supplement this gate; they never replace it.
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
node .github/scripts/security-regression.cjs
npm run test:unit
npm run typecheck
npm run i18n:validate
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
(
    cd src-tauri
    cargo audit
)
