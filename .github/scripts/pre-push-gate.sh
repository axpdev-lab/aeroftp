#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

# Keep aligned with the deterministic checks run by GitHub Actions. Task-specific
# tests supplement this gate; they never replace it.
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
node .github/scripts/security-regression.cjs

# The eight files that declare the application version, compared in one place.
# This block used to inline the list, and build.yml's R10 step inlined a narrower
# one with four of them: two checks with the same name and different scopes, and
# the narrower was the only one that ran automatically. v4.1.9 was tagged with
# package-lock.json a version behind in both of its fields because of exactly
# that. The list now lives in the script, and this gate is one of its callers.
node scripts/check-version-sites.mjs

npm run test:unit
npm run typecheck
npm run i18n:validate
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
(
    cd src-tauri
    # Same strictness as checks.yml: a yanked crate or an unrun yank check is
    # a warning with exit 0 by default, which reads as a pass. Deny them.
    cargo audit --deny warnings
)
