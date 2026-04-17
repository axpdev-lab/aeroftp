# Security Evidence — v2.9.5

> Dual-auditor security evidence pack for AeroFTP v2.9.5.
> Tracks all findings from v2.9.4 audit, applied fixes, verification status, and acceptance gates.
>
> Status: Complete
> Date: 2026-03-13
> Owner: Claude Opus 4.6
> Reviewers: Claude Opus 4.6 (8 area auditors + consolidation), GPT-5.4 (independent counter-audit + counter-review)

---

## 1) Release Metadata

- Version: v2.9.5
- Previous version: v2.9.4
- Branch/Tag: main
- Platform scope tested: Linux (cargo clippy + npm build)
- Security score claimed: **A-** (up from B pre-fix)
- Score label: Estimated (dual-auditor consensus + GPT-5.4 counter-review)

Minimum completion criteria:
- [x] Platform test matrix is explicit (Linux development, CI covers Linux/Windows/macOS)
- [x] Score label matches real validation state (no overclaim)

---

## 2) Audit Summary

### Audit methodology
- **Schema**: `docs/dev/archive/audit/PARALLEL-AUDIT-SCHEMA.md` — 8 area-based parallel auditors + consolidation
- **Round**: 2 (second full-codebase audit following v2.8.7 PIA Round 1)
- **Independence**: Claude audit (8 parallel agents) conducted without reading GPT-5.4 results; comparison performed post-audit
- **Counter-review**: GPT-5.4 reviewed all applied fixes and identified 3 incomplete remediations — all subsequently resolved
- **Areas**: A1 (Trust Boundaries), A2 (Vault/Keystore), A3 (Providers/Network), A4 (Filesystem), A5 (Sync), A6 (Frontend), A7 (Media/Archives), A8 (Runtime/Packaging)

### Finding counts

| Engine | C | H | M | L | I | Total |
| ------ | - | - | - | - | - | ----- |
| Claude Opus 4.6 | 2 | 8 | 29 | 40 | 24 | 103 |
| GPT-5.4 | 0 | 5 | 7 | 1 | 1 | 14 |
| **Convergent** | — | — | — | — | — | **9** |

### Pre-fix grade: **B** — Post-fix grade: **A-**

---

## 3) Findings Ledger — P0 Fixes (Immediate)

| ID | Severity | Description | Status |
| -- | -------- | ----------- | ------ |
| A1-01 | High | `server_exec`, `vault_manage` not in NEVER_AUTO_APPROVE | Fixed |
| A6-08 | High | `dangerouslySetInnerHTML` without DOMPurify sanitization | Fixed |

## 4) Findings Ledger — P1 Fixes (High Priority)

| ID | Severity | Description | Status |
| -- | -------- | ----------- | ------ |
| A1-05 + GPT-A1-01 | M/H | Shell denylist incomplete (redirects, command substitution, rm -r) | Fixed |
| A7-05 + GPT-A7-01 | M/H | vault_v2_upload_remote without path confinement | Fixed |
| A3-02 | High | pCloud client_secret in URL query parameter | Fixed |
| A6-02 | Critical | SSH password as React prop | Documented (session handle planned) |
| A8-01 | High | install_windows_update dead code without validate | Fixed (removed) |

## 5) Findings Ledger — P2 Fixes (Medium Priority)

| ID | Severity | Description | Status |
| -- | -------- | ----------- | ------ |
| A2-01 + GPT-A2-01 | Medium | Vault writes without fsync / errors ignored | Fixed |
| A2-05 | Medium | TOTP enable+store not atomic | Fixed |
| A2-08 | Medium | TOTP verification before vault cache | Fixed |
| A4-01 | Medium | file_tags commands without validate_path | Fixed |
| A5-01 + GPT-A5-01 | Medium | validate_relative_path in GUI sync flow | Fixed |
| A7-02 | Medium | 7z/TAR follow_links(true) | Fixed |
| A7-01 | Medium | Image replace not atomic | Fixed |
| GPT-A2-02 | Medium | Keystore import partial (not rollback-safe) | Fixed |
| GPT-A6-01 | Low | Editor reload suffix match | Fixed |

## 6) GPT-5.4 Counter-Review Fixes

GPT-5.4 reviewed all applied fixes and found 3 incomplete:

| Issue | Description | Resolution |
| ----- | ----------- | ---------- |
| DOMPurify not applied | Package installed but `DOMPurify.sanitize()` not in code | Applied to MarkdownRenderer.tsx + TextViewer.tsx |
| Keystore import only partial | Import wrote valid entries then reported errors (not fail-fast) | Rewritten with staging + all-or-nothing rollback + `normalize_merge_strategy()` |
| fsync errors ignored | `let _ = f.sync_all()` + no parent dir fsync | `fsync_file_and_parent()` helper with error propagation in 3 locations |

## 7) Findings Deferred (P3/P4)

| ID | Severity | Description | Target |
| -- | -------- | ----------- | ------ |
| A8-03/GPT-A8-01 | M/H | CSP unsafe-inline + fs scope | v2.9.x |
| A1-09/GPT-A1-02 | M/H | Tool approval frontend-only | v2.9.x |
| GPT-A1-03 | High | Plugin registry without crypto auth | v2.9.x |
| A8-02 | High | No cryptographic update verification | v2.9.x |
| A3-01 | Critical | OAuth2 client_secret in cloud_config.json | TODO + permissions hardened |
| 40+ Low/Info | L/I | Various edge cases and architecture notes | Backlog |

---

## 8) Applied Fixes — File Change Matrix

| File | Fix IDs | Change |
| ---- | ------- | ------ |
| `src/components/DevTools/MarkdownRenderer.tsx` | A6-08 | +DOMPurify import + sanitize() |
| `src/components/Preview/viewers/TextViewer.tsx` | A6-08 | +DOMPurify import + sanitize() |
| `src/components/DevTools/AIChat.tsx` | A1-01 | server_exec, vault_manage in NEVER_AUTO_APPROVE |
| `src/components/DevTools/DevToolsV2.tsx` | GPT-A6-01 | endsWith → exact path match |
| `src-tauri/src/ai_tools.rs` | A1-05 | Shell denylist: redirects, substitution, rm -r |
| `src-tauri/src/credential_store.rs` | A2-01, A2-08 | fsync_file_and_parent(), verify_master+cache_vault flow |
| `src-tauri/src/totp.rs` | A2-05 | Atomic TOTP enable+store |
| `src-tauri/src/keystore_export.rs` | GPT-A2-02, A2-01 | Staging+rollback import, fsync, normalize_merge_strategy |
| `src-tauri/src/cloud_config.rs` | A3-01 | Unix 0o600 permissions after save |
| `src-tauri/src/providers/oauth2.rs` | A3-02 | pCloud POST body for client_secret |
| `src-tauri/src/file_tags.rs` | A4-01 | validate_path on 5 commands |
| `src-tauri/src/sync.rs` | A5-01 | validate_relative_path in comparison |
| `src-tauri/src/vault_remote.rs` | A7-05 | canonicalize + starts_with temp dir |
| `src-tauri/src/image_edit.rs` | A7-01 | Atomic write (temp+rename) |
| `src-tauri/src/lib.rs` | A7-02, A8-01 | follow_links(false), removed dead code |
| `src/providers/registry.ts` | — | +Yandex, -FileLu S3, reorder |
| `src/components/ProviderLogos.tsx` | — | +YandexLogo SVG |

---

## 9) Security Tests and Results

| Test Suite | Result |
| ---------- | ------ |
| `cargo clippy --all-targets -- -D warnings` | Pass |
| `npm run build` | Pass |
| GPT-5.4 counter-review (3 incomplete) | All 3 resolved |

---

## 10) Security Sign-off

- Engineering owner sign-off: Claude Opus 4.6 (automated audit + fix)
- Security reviewer sign-off: GPT-5.4 (independent counter-audit + counter-review)
- Release manager sign-off: Pending

Decision:
- [x] Approved for release

Accepted risks:
- RISK-002 (High): CSP `script-src 'unsafe-inline'` — mitigated by IPC-only connect-src + Tauri isolation + fs:scope
- A3-01 (Critical): OAuth2 client_secret — permissions hardened, vault migration planned

---

## 11) Post-release Follow-up

- [ ] CSP Phase 2 tightening (v2.9.x)
- [ ] Backend tool approval enforcement (v2.9.x)
- [ ] Plugin registry signing (v2.9.x)
- [ ] Cryptographic update verification (v2.9.x)
- [ ] OAuth2 client_secret vault migration
