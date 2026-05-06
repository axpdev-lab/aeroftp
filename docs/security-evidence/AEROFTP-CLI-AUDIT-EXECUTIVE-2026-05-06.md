# AeroFTP CLI Audit Executive Attachment: 2026-05-06

## Decision Summary

AeroFTP CLI v3.7.2 underwent a strict audit focused on external grant review, security certification readiness, correctness, and code cleanliness.

The audit fixed high and medium impact issues in:

- Backend approval enforcement for GUI AI tool execution.
- MCP/core tool validation and profile matching.
- Read-only enforcement for `server_exec`.
- Local and remote path validation before transfers.
- Symlink rejection in local copy operations.
- Safe temp-file creation for upload and atomic-write paths.
- Daemon token file permissions.
- Malformed SFTP packet handling.
- Sync preflight correctness, invalid direction handling, transfer cancellation exit status, and exit-code documentation.
- Avoidable RustCrypto RSA dependency exposure.

## Verification Gates

Passed:

- `cargo check --all-targets --all-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- `npm run typecheck`
- `npm run test:unit`
- `npm run build`
- `npm audit --json` with 0 vulnerabilities
- CLI JSON smoke tests for `agent-info`, `profiles`, and invalid `sync --direction`

Not passed:

- `cargo audit --json`

Reason: residual transitive `rsa` advisories remain through `sigstore` and `russh`. Direct `rsa` usage and `jsonwebtoken`'s RustCrypto RSA path were removed. No ignore was added. This is documented as an architecture risk requiring replacement, feature gating, or explicit external acceptance.

## External Review Position

AeroFTP CLI is in a significantly stronger state for functional, correctness, and code-quality review. It can be presented with a clear evidence trail and without hiding known issues through suppressions.

For security certification, the remaining `cargo audit` result should be disclosed up front. The recommended certification path is:

1. Feature-gate or replace the `sigstore` path that pulls `rsa` 0.9.10.
2. Evaluate whether `russh` RSA-key support can be disabled or replaced to remove `rsa` 0.10.0-rc.17.
3. Re-run `cargo audit --json` and require a clean result before claiming dependency-audit compliance.

Primary technical report:

- `docs/security-evidence/AEROFTP-CLI-AUDIT-2026-05-06.md`

