# AeroFTP CLI Audit: 2026-05-06

> Scope: `aeroftp-cli` and its Rust backend integration surfaces in AeroFTP v3.7.2.
> Purpose: prepare the CLI for external grant, security, correctness, and code-quality review.
> Status: Fixed findings applied; residual upstream/architecture risks documented without `cargo audit` or `clippy` ignores.
> Auditor: Codex, with three parallel audit tracks for security, CLI behavior, and code quality.

---

## 1. Executive Result

The audit identified and fixed high-impact issues in backend approval enforcement, MCP/core tool validation, local and remote path validation, temp-file safety, SFTP packet parsing, sync preflight correctness, transfer cancellation exit status, and dependency exposure.

The CLI now passes the main engineering gates used in this audit:

| Gate | Result |
| ---- | ------ |
| `cargo check --all-targets --all-features` | Pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | Pass |
| `cargo test --all-targets --all-features` | Pass |
| `npm run typecheck` | Pass |
| `npm run test:unit` | Pass |
| `npm run build` | Pass |
| `npm audit --json` | Pass, 0 vulnerabilities |
| CLI smoke: invalid `sync --direction` | Pass, JSON error with exit code 5 |
| CLI smoke: `agent-info --json` | Pass, version 3.7.2, 39 agent native tools |
| CLI smoke: `profiles --json` | Pass, JSON array |

Security caveat: `cargo audit --json` still reports two instances of `RUSTSEC-2023-0071` for transitive `rsa` dependencies through `sigstore` and `russh`. Direct `rsa` usage and the `jsonwebtoken` RustCrypto RSA path were removed. The remaining exposure requires an architecture decision rather than an ignore file.

---

## 2. Methodology

The audit used a multi-pass, adversarial method:

| Track | Objective | Methods |
| ----- | --------- | ------- |
| Security | Validate approval boundaries, path boundaries, temp-file safety, credential isolation, and dependency risks | Manual code review, threat modeling, dependency graph analysis, `cargo audit`, focused exploitability review |
| Functional CLI | Validate command behavior, JSON outputs, agent contracts, exit codes, and preflight commands | CLI command map review, direct smoke tests, consistency checks against `agent-info` and AGENTS.md contract |
| Code Quality | Identify panic surfaces, unwrap misuse in network-facing code, clippy suppressions, workaround patterns, and low-grade correctness debt | Static review, `rg` sweeps, compiler/clippy gates, code-path inspection |

Severity definitions:

| Severity | Definition |
| -------- | ---------- |
| High | Could bypass an explicit security boundary, expose sensitive local state, or cause unsafe remote mutation without the expected approval model |
| Medium | Could cause correctness failures, unexpected mutation, unsafe edge-case behavior, or materially weaken defense in depth |
| Low | Documentation, consistency, hardening, or maintainability issue that external reviewers would reasonably flag |

No new `cargo audit` ignore, `clippy` allow, or blanket suppression was added as part of this audit.

---

## 3. Fixed Findings

| ID | Severity | Finding | Resolution | Files |
| -- | -------- | ------- | ---------- | ----- |
| CLI-AUDIT-01 | High | GUI AI tool execution could bypass backend approval by dispatching directly through `execute_ai_tool` | `execute_ai_tool` now enforces allowed tools and calls backend approval checks for scoped remote operations before dispatch | `src-tauri/src/ai_tools.rs` |
| CLI-AUDIT-02 | High | MCP/core remote dispatcher bypassed per-tool validators, allowing unsafe local upload/download paths and weak remote path validation | Added local validation for upload/download paths and strengthened remote path validation for nulls, traversal, control chars, option-like paths, and excessive length | `src-tauri/src/ai_core/remote_tools.rs` |
| CLI-AUDIT-03 | High | `server_exec` contract was documented as read-only but accepted mutative operations | `server_exec` is now read-only and rejects `get`, `put`, `mkdir`, `rm`, and `mv` with an explicit-use error | `src-tauri/src/ai_core/remote_tools.rs` |
| CLI-AUDIT-04 | High | MCP profile lookup accepted first substring match, creating wrong-server risk | Profile matching now requires exact ID/name or a unique substring; ambiguous matches return an explicit error listing candidates | `src-tauri/src/mcp/pool.rs` |
| CLI-AUDIT-05 | High | `local_copy_files` single-file branch bypassed validation and recursive copy followed symlinks | Added validation on single-file and recursive paths; symlinks are refused before copy | `src-tauri/src/ai_core/local_tools.rs` |
| CLI-AUDIT-06 | Medium | `local_stat_batch` bypassed local path validation | Each resolved path is validated and denied paths return per-item error JSON | `src-tauri/src/ai_core/local_tools.rs` |
| CLI-AUDIT-07 | Medium | SFTP serve parser used unchecked slicing and `unwrap`, allowing malformed packets to panic | Packet parsing is bounds-checked, oversized packets are rejected, malformed packets receive status failure, worker-channel failure no longer panics | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-08 | Medium | `.aerotmp` writes were predictable and could resume through symlinked temp paths | Atomic/resumable temp writes now use `create_new`, reject symlinked temp paths, and validate regular-file metadata before resume | `src-tauri/src/providers/atomic_write.rs` |
| CLI-AUDIT-09 | Medium | Inline upload temp files used predictable names in world-writable temp | Replaced predictable paths with `tempfile::Builder::tempfile()` and write through the open handle | `src-tauri/src/ai_core/mcp_impl.rs`, `src-tauri/src/ai_core/cli_impl.rs` |
| CLI-AUDIT-10 | Medium | Daemon auth token file permissions were hardened after write instead of during creation | Unix token writes now create with mode `0600`, `O_NOFOLLOW`, write/sync via open handle, then enforce permissions | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-11 | Medium | `sync --direction <invalid>` could no-op and exit success | Invalid direction now fails before connection with JSON error and exit code 5 | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-12 | Medium | `sync-doctor` ignored profile `initialPath` and could inspect a different remote path than `sync` | `sync-doctor` now resolves remote paths through the same profile-aware resolver used by `sync` | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-13 | Low | `sync-doctor --checksum` suggested a non-existent `sync --checksum` flag | Suggested command no longer includes unsupported `--checksum` | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-14 | Low | `transfer` could return success after cancellation between plan and execution | Added post-plan and post-execution cancellation checks returning exit code 130 | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-15 | Low | `profiles --json` and `agent-info --json` diverged when no saved profiles key existed | `agent-info` now treats a missing profile list as an empty profile set, matching `profiles --json` | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-16 | Low | CLI help omitted documented exit codes 9, 10, 11, and 130 | Help footer now lists the extended exit-code contract | `src-tauri/src/bin/aeroftp_cli.rs` |
| CLI-AUDIT-17 | Medium | Direct dependency graph pulled RustCrypto RSA through direct `rsa` and `jsonwebtoken` features | Removed direct `rsa` dependency and changed `jsonwebtoken` to `aws-lc-rs` with explicit features | `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` |

---

## 4. Dependency Audit

### Fixed dependency exposure

Direct or avoidable `rsa` exposure was reduced:

- Removed direct `rsa = "0.9"` from `src-tauri/Cargo.toml`.
- Changed `jsonwebtoken` from `features = ["rust_crypto"]` to `default-features = false, features = ["aws-lc-rs", "hmac", "use_pem"]`.
- Verified dependency graph no longer includes direct root `rsa` or `jsonwebtoken` -> RustCrypto `rsa`.

### Residual dependency exposure

`cargo audit --json` still fails with:

| Advisory | Package | Source | Status |
| -------- | ------- | ------ | ------ |
| `RUSTSEC-2023-0071` | `rsa` 0.9.10 | `sigstore` 0.13.0 and `openidconnect` 4.0.1 | Residual architecture risk |
| `RUSTSEC-2023-0071` | `rsa` 0.10.0-rc.17 | `russh` 0.60.1 through `internal-russh-forked-ssh-key` | Residual architecture risk |

Observed graph:

```text
rsa v0.9.10
+-- openidconnect v4.0.1
|   +-- sigstore v0.13.0
|       +-- aeroftp v3.7.2
+-- sigstore v0.13.0

rsa v0.10.0-rc.17
+-- internal-russh-forked-ssh-key v0.6.18+upstream-0.6.7
|   +-- russh v0.60.1
|       +-- aeroftp v3.7.2
+-- russh v0.60.1
```

Recommendation for certification:

1. Decide whether `russh` RSA-key support can be disabled, replaced, or feature-gated.
2. Decide whether `sigstore` verification can be isolated behind an optional feature, replaced, or moved to a process boundary.
3. Keep `cargo audit` failing until this is actually resolved; do not add ignore entries for certification evidence unless an external reviewer formally accepts the risk.

---

## 5. Verification Evidence

Commands executed during the audit:

```bash
npm run typecheck
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
npm audit --json
cargo audit --json
cargo tree -i rsa@0.9.10
cargo tree -i rsa@0.10.0-rc.17
npm run test:unit
npm run build
cargo check --all-targets --all-features
target/debug/aeroftp-cli --no-banner agent-info --json
target/debug/aeroftp-cli --no-banner profiles --json
target/debug/aeroftp-cli --no-banner sync --direction sideways ./ /tmp/aeroftp-audit-smoke --dry-run --json
```

Key results:

| Check | Result |
| ----- | ------ |
| Rust tests | 1235 library tests passed, 8 ignored; 117 CLI tests passed; 5 delta-sync tests passed; 11 tool-parity tests passed |
| Clippy | Pass with `-D warnings` |
| TypeScript | Pass |
| Unit tests | 27 tests passed |
| Production build | Pass; Vite reported only large chunk/plugin timing warnings |
| NPM audit | 0 vulnerabilities |
| Cargo audit | Fails on residual transitive `rsa` exposure described above |
| Invalid sync direction smoke | Returned `{"status":"error","code":5}` and process exit 5 |
| Agent info smoke | Returned version `3.7.2`, 39 agent native tools, exit-code map includes `11` |
| Profiles JSON smoke | Returned JSON array |

Note: `cargo fmt --check` was not used as a release gate in this audit because the working tree already contained unrelated formatting drift in pre-existing dirty files outside the CLI audit patch set. No formatter ignore was added.

---

## 6. Reviewer Notes

The current state is materially stronger than the pre-audit baseline:

- Mutative tool execution is no longer reachable through the read-only `server_exec` compatibility surface.
- GUI AI execution now has backend approval enforcement instead of relying on UI-only controls.
- MCP and AI core local/remote paths have stricter validation before transfer operations.
- Malformed SFTP packets should fail closed instead of panicking the bridge.
- Temp-file creation now uses safer primitives in the reviewed upload and atomic-write paths.
- CLI preflight commands better match actual execution semantics.

The build is suitable for external functional and code-quality review. For external security certification, the remaining `cargo audit` findings should be treated as an explicit open risk pending dependency replacement, feature gating, or accepted-risk signoff.

---

## 7. Residual Backlog

| ID | Severity | Residual | Recommended owner decision |
| -- | -------- | -------- | -------------------------- |
| CLI-RESIDUAL-01 | High | `cargo audit` fails on transitive `rsa` via `sigstore` and `russh` | Architecture decision required before claiming dependency-audit clean |
| CLI-RESIDUAL-02 | Medium | Some OAuth/profile error paths may still bypass uniform CLI JSON error formatting | Standardize all profile/OAuth failures through `print_error` |
| CLI-RESIDUAL-03 | Low | `agent-info` contains hardcoded command/protocol descriptions that can drift from Clap and provider registry | Generate command inventory from Clap/provider metadata or add snapshot tests |
| CLI-RESIDUAL-04 | Low | Full rustfmt gate is blocked by unrelated dirty formatting drift | Run a dedicated formatting cleanup commit after preserving user changes |

No residual item above is hidden by an ignore directive.
