# Security Evidence: v4.1.7

> Pre-release commit audit evidence for AeroFTP v4.1.7.
> Tracks confirmed findings on `v4.1.6..main`, applied fixes, deferred items, and acceptance gates.
>
> Status: Fixes on PR (pre-merge); gate at tag after merge  
> Date: 2026-07-30  
> Scope: cycle commits since tag `v4.1.6` (not a full-surface re-audit)  
> Independent audit (Kimi L2): appended in §8 — **one OPEN BLOCKER (LIVE-1, data-loss class), green pass withheld**; majors MCP-1 / MCP-2 / SEC-K1 / I18N-K1 / PR-2 recommended before tag

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

---

## 8) Independent Audit (Kimi L2, 2026-07-30)

Second, independent pre-release audit run in parallel to this one: 9 adversarial review lanes over `v4.1.6..669814f32` (164 non-merge commits), raw-code reading only (per-push CI gates not re-run), plus **live protocol testing** against the axpbuntu lab (Nextcloud, MinIO S3, FTPS) using a release-code CLI built from `669814f32`, with the installed v4.1.6 engine as regression baseline. Coverage: 6/9 lanes completed (security-crypt-vault, sync-DAG-transfer, i18n, CLI-MCP, CI-release-updater, pending-PRs); 3 lanes incomplete (aerorsync engine, provider backends, frontend UI — API quota exhausted mid-run; recorded as audit debt). Full detail: `kimi-greenpass-v417/KIMI-AUDIT-v4.1.7.md` (owner's source tree).

**Verdict: GREEN PASS WITHHELD** — one open blocker, five majors recommended before tag.

### 8.1 LIVE-1 [BLOCKER · CONFIRMED · OPEN] — XML `trim_text(true)` corrupts listed names around entities → sync `delete_local` cascade

quick-xml readers parsing name-bearing XML are configured `trim_text(true)`. For key `sp ace & ünïcodé.txt` the server sends `<Key>sp ace &amp; ünïcodé.txt</Key>`; quick-xml emits `Text("sp ace ")` + `GeneralRef("amp")` + `Text(" ünïcodé.txt")`, each fragment trimmed → listing shows `sp ace&ünïcodé.txt`. Upload stores the **correct** name (`get`/`file_info` on the real name succeed); GET of the **listed** name 404s.

Live evidence (release CLI @ `669814f32`, axpbuntu lab):

| Check | Result |
| ----- | ------ |
| `put "raw & name.txt"` → MinIO | stored correctly; `ls` shows `raw&name.txt` |
| `get` of the listed (mangled) name | **404 Path not found** |
| `get` of the real name | OK, sha256 intact |
| Same fixture on Nextcloud WebDAV | same mangling; FTPS unaffected (no XML) |
| sha256 round-trip, all other files (NC / MinIO / FTPS) | intact |
| `sync --direction download --delete --dry-run` (MinIO) | plans `download sp ace&ünïcodé.txt` (404s) **and `delete_local "sp ace & ünïcodé.txt"` — the user's real file** |

With `--delete` on a real run, local data is destroyed while the download fails. Affected readers at HEAD: `s3.rs:1118` (ListObjectsV2 — every S3 provider) plus `s3.rs:1354, 2282, 3683, 4086, 4757, 5106, 5245`; `webdav.rs:507, 1425, 1715, 1970`; `azure.rs:451, 2242`; `jottacloud.rs:612, 664, 797, 1935`. Trigger: names containing `& ' < > "` with adjacent whitespace. The `s3.rs:1170-1177` comment already says "Do NOT trim" — the reader config contradicts it.

Fix direction: `trim_text(false)` on name-bearing readers (explicit `.trim()` only on numeric/date fields), plus a regression test listing + syncing `a & b.txt`. **Status: FIXED in PR #536** (17 readers across s3/webdav/azure/jottacloud; 5 regression tests; live round-trip + `sync --delete` cascade re-run clean on MinIO and Nextcloud with a CLI built from the fix branch). Merge of #536 flips this to closed.

### 8.2 LIVE-2 [OK — regression closed] FTPS sync hang on mkdir-during-sync

Installed v4.1.6 engine stalls reproducibly when sync execution must create a missing remote directory over FTPS (zero progress; manual `mkdir` unblocks). Release CLI @ `669814f32` completes the same scenario cleanly. Fixed on main; no action.

### 8.3 Findings ledger (confirmed; K-IDs cross-referenced where convergent)

| ID | Severity | Area | Description | Status |
| -- | -------- | ---- | ----------- | ------ |
| PR-1 | — | #532 pre-merge | Merged-tree E0308 (`Vec<String>` vs `Option<Vec<String>>` on `export_sync_template_cmd`), found via `git merge-tree` despite MERGEABLE badge | Resolved pre-merge (verified in `756d52293`) |
| PR-2 | MED | commands (#532, now on main) | Whole-file read-modify-write commands lost main-thread serialization (`cloud_pairs.rs:187`, `cloud_config.rs:262`, multi-path) — rapid UI actions lose updates | OPEN |
| PR-3 | LOW | commands (#532, now on main) | `atomic_write` fixed tmp sibling (`sync.rs:2794-2803`) newly concurrent — torn JSON on unlocked files | OPEN |
| SEC-K1 | MED | crypt anchor | Anchor-escape rule (01d1bf84f) enforced on 1 of 3 save paths: `handleOAuthMetadataSave` (ConnectionScreen.tsx:1801-1848) and `handleSaveAsNew` (:1874, button :3548 has no disabled condition) persist anchor-orphaning profiles | OPEN |
| SEC-K2 | LOW | speedtest | Docroot guard is exact-string (speedtest.rs:801-820): subdirs, `..`, backslashes pass | OPEN |
| SEC-K3 | LOW | Swift/Blomp | Client still follows redirects with `X-Auth-Token` (swift.rs:147-153) — the class f9b77aca8 fixed for S3 | OPEN |
| SEC-K4 | LOW | crypt guard | View-only lock sets `active_crypt_overlay=false` (provider_commands.rs:1881) → raw-write guard no-ops incl. into the anchor; error message still claims "locked" coverage; ai_core guard inherits | OPEN |
| SEC-K5 | LOW | crypt scope | `path_is_outside_crypt_scope` byte-exact (provider_commands.rs:212-235): case-insensitive / backslash servers cross it | OPEN |
| SEC-K6 | LOW | aerorsync | "local_transport keeps 0o7777" claim false: mask lives in shared writers (delta_transport_impl.rs:2371, streaming_writer.rs:244) | OPEN |
| MCP-1 | MED | MCP/CLI speed | `aeroftp_speed` / `cli speed --remote-path`: no existence check, overwrite-then-delete any caller-named remote file; bypasses webroot refusal + scratch confinement (remote_tools.rs:2449-2490, aeroftp_cli.rs:39793-39802) | OPEN |
| MCP-2 | MED | MCP trash | `remote_trash empty` defaults `dry_run=false`, `prefix=""` → one call irreversibly purges all versions/delete-markers in the bucket; sibling destructive tools default dry-run (tools.rs:1347-1363, remote_tools.rs:1316-1321) | OPEN |
| MCP-3 | LOW | CLI | `connect` prints empty host for OAuth cloud profiles (bb463d1cb regression, aeroftp_cli.rs:29739-29742) | OPEN |
| MCP-4 | LOW | cyber tools | `stage_hash_drop` ignores dir chmod failure (cyber_tools.rs:189-193) | OPEN |
| MCP-5 | LOW | CLI trash | trash `id` interpolated unvalidated into DAV URLs (aeroftp_cli.rs:43425-43459, webdav.rs:1554-1560) | OPEN |
| I18N-K1 | MED | i18n · in-scope | `aerocryptProfile.defaultSaltTierMeaning` (DefaultSaltDisclosure.tsx:41) in NO locale — raw key in all 47 languages; same class as 669814f32 | OPEN |
| I18N-K2 | MED | i18n · pre-existing | 32 referenced keys absent from en.json → raw keys in UI; `\|\| 'fallback'` guards dead by construction (incl. `duplicates.confirmDelete` mis-reference of `deleteConfirm`); validator never scans code references | OPEN |
| I18N-K3 | LOW | i18n · pre-existing | `speedTest.compareDisclaimer` `{total}` mismatch in 45 locales | **Cross-confirmed = I18N-2, Fixed** |
| I18N-K4 | LOW | i18n · pre-existing | Cross-script corruption hy ×3 / ka ×1; ZWSP mid-word et ×2, km healthCheck | OPEN |
| I18N-K5 | LOW | i18n · in-scope | 5d99fa9bc pre-mount fallback chain dead (both shells ship `lang="en"`) | OPEN |
| SYNC-K1 | LOW | local watcher | `local_panel_watch` stale-watcher race after 7a4c7e537 (local_panel_watcher.rs:93-151) | OPEN |
| SYNC-K2 | LOW | dedupe | Oversize/over-budget skips invisible; bar hits 100% while >256 MB candidates unexamined (dedupe/mod.rs:413-423) | OPEN |
| SYNC-K3 | LOW | pty | Spawn commit→fill window (pty.rs:263-269), reachable by guessed id only | OPEN |
| SYNC-K4 | note | multipart | Abandoned-endpoint orphan parts bill indefinitely (7d TTL scavenger is per-current-endpoint; pre-existing design) | Noted |
| CI-1 | LOW | snap CI | Tag-build GPU gate kept fragile inline grep | **Cross-confirmed = SEC-3, Fixed** |
| CI-2 | LOW | updater | Marker `verified: true` also on `VerificationUnavailable` (lib.rs:2889-2890) → log prints "(Verified)" | OPEN |
| CI-3 | LOW | updater UI | Green "match" chip inside amber unverified panel (UpdateVerificationPanel.tsx:156-159; digest computed pre-verification) | OPEN |
| CI-4 | LOW | release | Re-created tag keeps old assets/sigstore bundles attesting the old commit (build.yml:589) | OPEN |
| CI-5 | note | release | Manifests still 4.1.6 — version bump must land before tag (R10 by design) | Noted |

Lanes verified clean (samples): S3 STS redirect fix + tests; OAuth profile export encryption (AES-256-GCM/Argon2id, 0600); Windows vault DACL; default-salt entropy enforcement; russh 0.62.4 bump clean; 7a4c7e537 threading move (30 conversions, apart from SYNC-K1); multipart cancel-from-part; export excludes parity; AeroSync tab state lifting; MCP surface validation (`validate_remote_path`, local deny-list, argv-array subprocess, rate limits); i18n key parity 46/46 + placeholder integrity + JSON validity; release workflow overwrite guards; snap base/payload pipeline; dep bump consistency. #527 / #531 verified READY pre-merge.

### 8.4 Recommended before tag (Kimi L2)

1. **LIVE-1** — fix `trim_text` on name-bearing XML readers + regression test (BLOCKER, data-loss class).
2. **MCP-1** — stat-before-overwrite + scratch confinement on `aeroftp_speed` / `cli speed`.
3. **MCP-2** — `remote_trash empty`: default `dry_run=true` or explicit confirm + non-empty prefix.
4. **SEC-K1** — hoist the anchor-escape check into `handleOAuthMetadataSave` / `handleSaveAsNew`.
5. **I18N-K1** — add `aerocryptProfile.defaultSaltTierMeaning` to en.json + re-derive 46 locales; extend the i18n validator with a code-reference pass (root cause of I18N-K2).
6. **PR-2** — acknowledge or fix the RMW lost-update class now on main via #532.
7. Complete or formally defer the 3 uncovered lanes (aerorsync engine, provider backends, frontend UI).

### 8.5 Updated acceptance gate

The §7 gate stands, plus: **item 0 — LIVE-1 resolved (or tag slips)**, and items 2–7 of §8.4 fixed or explicitly deferred by the owner as accepted debt with a tracker row (item 7 included: the three uncovered lanes must be completed or formally deferred, not silently skipped).
