# Security Evidence: v4.1.7

> Pre-release commit audit evidence for AeroFTP v4.1.7.
> Tracks confirmed findings on `v4.1.6..main`, applied fixes, deferred items, and acceptance gates.
>
> Status: Fixes on PR (pre-merge); gate at tag after merge  
> Date: 2026-07-30  
> Scope: cycle commits since tag `v4.1.6` (not a full-surface re-audit)

---

## 1) Release Metadata

- Version: v4.1.7 (in preparation)
- Previous version: v4.1.6
- Branch: `audit/v4.1.7-prerelease` → PR #533
- Platform scope tested: Linux (targeted unit tests, CLI live smoke, aerorsync bench fixture, `i18n:validate`)
- Full per-push suite (fmt / clippy / full cargo test matrix): not re-run as wall-clock; required green on every merge into `main`

Minimum completion criteria:

- [x] Findings ledger complete (confirmed / deferred explicit)
- [x] Ship-risk findings fixed on the audit branch
- [ ] PR #533 merged to `main`
- [ ] Full local gate re-run after merge
- [ ] CHANGELOG Security line: **Pre-tag commit audit: PASS.**

---

## 2) Audit Summary

Adversarial review of the release cycle: sync/crypt compare, providers (WebDAV/Nextcloud, Swift), profile export secrets, updater Sigstore path, titlebar/extract UI, i18n regressions, snap packaging GPU gate. Live smoke against the local rsync SSH fixture (`:2224`) and aerorsync bench A1–A5.

### Finding counts

| Severity | Confirmed | Fixed in PR #533 | Deferred |
| -------- | --------- | ---------------- | -------- |
| MED | 8 | 7 | 1 (SEC-2) |
| LOW | 6 | 5 | 1 (PROV-4) |
| **Total** | **14** | **12** | **2** |

---

## 3) Findings Ledger: Fixed

| ID | Severity | Area | Description | Status |
| -- | -------- | ---- | ----------- | ------ |
| FRONT-1 | MED | titlebar | Drag fillers used `h-full` under auto-height flex → 0px; #511 right-half drag residual | Fixed |
| FRONT-2 | MED | extract | Extract window closed silently when the file chooser was unavailable (#510 / #515 residual) | Fixed |
| SEC-1 | MED | profile export | Credentials-off export still embedded options-borne secrets (TOTP, key passphrase, SAS, …) and `has_credentials=false` | Fixed |
| SEC-3 | LOW | snap CI | Tag pipeline GPU gate was a narrow grep; full `snap-gpu-lint-check.sh` only on weekly job | Fixed |
| PROV-1 | MED | WebDAV | `nextcloud_base_url` stripped install subpath → OCS/trash/chunked broke under `/owncloud` | Fixed |
| PROV-2 | MED | Swift | `rmdir_recursive` returned success when bulk-delete HTTP failed | Fixed |
| PROV-3 | LOW | Swift | `storage_info` ignored HEAD status and fabricated 0 used / 40 GB | Fixed |
| SYNC-1 | LOW | crypt compare | `overlay_wrapped` sampled only after remote scan; badge toggle mid-compare mis-classified rows | Fixed |
| I18N-1 | MED | lv.json | Rebase lost corrected checksum / S3 ETag strings | Fixed |
| I18N-2 | MED | speedTest | `compareDisclaimer` missing second `{total}` (local temp storage) in most locales | Fixed |
| I18N-3 | MED | AeroCrypt | `defaultSaltLabel` still required “attestation” after the control was removed | Fixed |
| I18N-4 | LOW | km | `nav_sync_enabled` paths unseparated and reversed | Fixed |

---

## 4) Findings Ledger: Deferred

| ID | Severity | Description | Rationale | Target |
| -- | -------- | ----------- | --------- | ------ |
| SEC-2 | MED | Sigstore verify `Err` maps to `VerificationUnavailable`; `VerificationFailed` never constructed; install not blocked | Product + digests story: hard-gate needs published/fetched release digests and release-note disclosure; soft advisory already shown | post-v4.1.7 |
| PROV-4 | LOW | Swift recursive delete lists at most 10k objects, no pagination | Honest errors after PROV-2 for the posted batch; large trees need marker pagination | post-v4.1.7 |

---

## 5) Applied Fixes: File Change Matrix

| File | Fix IDs | Change |
| ---- | ------- | ------ |
| `src/components/CustomTitlebar.tsx` | FRONT-1 | Drag fillers / spacers use `h-9` |
| `src/components/ExtractWindow.tsx` | FRONT-2 | Re-check chooser availability; error phase instead of silent close |
| `src-tauri/src/profile_export.rs` | SEC-1 | `strip_options_borne_secrets`; options secrets flip `has_credentials`; unit tests |
| `src-tauri/src/lib.rs` | SEC-1 | Call strip when `include_credentials` is false |
| `src-tauri/src/providers/webdav.rs` | PROV-1 | Keep install subpath in `nextcloud_base_url`; tests |
| `src-tauri/src/providers/swift.rs` | PROV-2, PROV-3 | Bulk-delete and storage_info fail on non-success HTTP |
| `src-tauri/src/provider_commands.rs` | SYNC-1 | Pin overlay wrap for compare scan; fail closed on mid-scan toggle |
| `.github/workflows/build.yml` | SEC-3 | G1 runs `scripts/snap-gpu-lint-check.sh` |
| `src/i18n/locales/*` | I18N-1..4 | lv checksum restore; compareDisclaimer; defaultSaltLabel; km nav_sync |

---

## 6) Verification Evidence

| Check | Result |
| ----- | ------ |
| `cargo test --lib profile_export` | 9 passed |
| `cargo test --lib …nextcloud_base_url*` | 5 passed |
| `cargo test --features aerorsync --lib aerorsync_bench -- --ignored` | 3 passed (fixture `:2224`) |
| CLI vault create / add / info | OK (v3 throwaway) |
| CLI `ls sftp://… --key … --trust-host-key` against fixture | OK (22 entries) |
| `npm run i18n:validate` | PASSED 46/46 |
| `cargo audit` (session scan) | no vulnerabilities reported |

---

## 7) Acceptance Gate (before tag)

1. Merge PR #533.
2. Full local gate: `cargo fmt --check`, clippy `-D warnings` (lib + CLI), `cargo test`, frontend checks, `i18n:validate`.
3. CHANGELOG Security section entry starting **Pre-tag commit audit: PASS.** listing fixed IDs and deferred SEC-2 / PROV-4.
4. Tracker row + house-style comment for the audit.

Only then proceed to the version tag.
