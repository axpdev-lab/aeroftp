# Security Evidence: v4.1.7

> Pre-release commit audit evidence for AeroFTP v4.1.7.
> Tracks confirmed findings on `v4.1.6..main`, applied fixes, deferred items, and acceptance gates.
>
> Status: **PRE-TAG AUDIT PASS** on the candidate branch; final merge, version bump, all-platform CI, and tag still pending
> Date: 2026-08-05
> Scope: cycle commits since tag `v4.1.6` (not a full-surface re-audit)  
> Independent audit (Kimi L2): the historical 2026-07-30 snapshot is retained in §8; its live findings are reconciled and superseded by the final audit in §9

---

## 1) Release Metadata

- Version: v4.1.7 (in preparation)
- Previous version: v4.1.6
- Audit branches: `audit/v4.1.7-prerelease` → PR #533 (merged); `audit/v4.1.7-fixes` (final candidate)
- Platform scope tested locally: Linux full mandatory gate; final all-platform CI remains required after merge
- Full per-push suite: passed on the final code candidate on 2026-08-05

Minimum completion criteria:

- [x] Findings ledger complete (confirmed / deferred explicit)
- [x] Ship-risk findings fixed on the audit branch
- [x] PR #533 merged to `main`
- [x] Full local gate re-run on the final code candidate
- [x] CHANGELOG Security line: **Pre-tag commit audit: PASS.**
- [ ] PR #569 and the final audit fixes merged to `main`
- [ ] Version bump and final all-platform CI green before tag

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

---

## 9) Final Pre-tag Audit (2026-08-05)

### 9.1 Scope and method

The final code candidate review covers `v4.1.6..b8054861a`: **333 commits, 236 non-merge commits, 342 changed files, 45,916 insertions, and 12,879 deletions**. Four adversarial lanes reviewed AeroRsync (55/55 assigned commits), frontend and UI regressions (99/99), CI, release, and dependencies (60/60), plus the remaining provider, sync, crypt, CLI, MCP, and i18n boundaries. Each confirmed release-risk finding was fixed on `audit/v4.1.7-fixes` and paired with targeted regression coverage before the full repository gate.

The dependency candidate includes the exact two commits from PR #569 rather than a duplicate remediation: `rkyv 0.7.46` is removed from the graph and the JavaScript `plugin-log` package is aligned with the Rust crate. The final Dependabot refresh also found GHSA-fxqj-rqcc-2cmp in `postcss` 8.5.20 and GHSA-m65r-rprj-r5rg in `russh` 0.62.4; the candidate moves them to 8.5.25 and 0.62.5 respectively. `npm audit` reports zero vulnerabilities and the project-configured `cargo audit` is clean without adding an advisory ignore.

### 9.2 Final findings ledger

| Area | Confirmed risk | Resolution |
| ---- | -------------- | ---------- |
| AeroRsync | Unbounded peer-controlled xattr count, names, values, and retry buffering; download did not resolve out-of-band xattr data before finalization | Bounded at decode and buffering boundaries; transactional multi-frame OOB resolution; sender/receiver regression tests |
| Frontend performance | Duplicate thumbnails mounted and read eagerly for every result; size-only cache keys could serve stale images; a reused component could carry viewport eligibility to a new cache key | Intersection-observer lazy loading, process-wide concurrency cap of four, cancellation, versioned cache keys, and eligibility bound to the current key |
| Frontend regressions | Generic/demo providers could inherit an unrelated company identity; Trash range anchor survived sort and marquee lacked the owning scroller | Exact provider identity before protocol fallback; generic/demo tier bypass; anchor reset and scroller-owned marquee autoscroll |
| Provider security | Swift could forward `X-Auth-Token` across redirects or a stale refreshed endpoint; Nextcloud trash listing left the username raw and href-derived IDs were double-encoded | Redirects disabled; storage endpoints reject downgrade/URL credentials and every token-bearing request is origin-bound; username and decoded trash IDs are encoded exactly once with coverage |
| Crypt boundaries | View-only lock could disarm the raw-write guard; scope checks did not conservatively handle case-insensitive or backslash-backed paths | Capability remains armed after overlay removal; normalized fail-closed scope comparison with regression tests |
| Filesystem and sync | Predictable shared-temp hash-drop staging had a symlink replacement window; concurrent atomic sync writes reused one staging sibling | Hash drops use an atomically created random owner-only process directory and 0600 files; sync uses unique `create_new` staging, durable flush, cleanup, and serialized publish |
| Provider listing authority | ImageKit can store and serve an upload while its List API omits it; Compare could also span a reconnect and mix two provider sessions | Providers declare listing authority; ImageKit is non-authoritative; CLI/legacy/DAG deletes fail closed; GUI Compare pins connection generation and revalidates authority before constructing a plan |
| MCP and CLI | Content-addressed speed-test uploads could report dedupe speed; disabled integrity checks still hashed every upload; OAuth/cloud connect output could show an empty host | Per-iteration unique payload, conditional integrity hashing, and profile-name fallback with tests. The #549 fail-closed existence guard remains intact |
| Watchers and reporting | Generation changes could race the final watcher-slot write and stale callbacks could still emit; dedupe resource-cap skips were invisible | Request creation, stop, and installation share the slot mutex; callbacks verify generation before emission; skipped-file count reaches progress and result UI |
| i18n | Static shell `lang="en"` was mistaken for a mounted locale and could override the saved locale before provider mount | Provider publishes an explicit mounted-language marker synchronously; pre-mount saved-locale regression test |
| CI | Delta-sync job timeout was shorter than the combined legitimate step budgets | Job timeout raised from 40 to 110 minutes, above the 93-minute declared sequential budget plus setup and teardown |
| Dependencies | `RUSTSEC-2026-0235` through `rkyv 0.7.46`; GHSA-fxqj-rqcc-2cmp through `postcss` 8.5.20; GHSA-m65r-rprj-r5rg through `russh` 0.62.4 | `rkyv` closed by PR #569; PostCSS and russh bumped to patched releases; no new audit suppression |

### 9.3 Reconciliation of the historical §8 snapshot

The §8 snapshot is evidence of the state on 2026-07-30, not the final release verdict. LIVE-1 was fixed by #536; MCP-1 and MCP-2 by #549; SEC-K1 by #548; I18N-K1 and I18N-K2 by `b13372f21`; PR-2 by the serialized configuration mutation helpers; SEC-K2, SEC-K6, SYNC-K3, and CI-4 by intervening main commits; and SEC-K3 through SEC-K5, MCP-3 through MCP-5, PR-3, SYNC-K1, SYNC-K2, and I18N-K5 by the final audit branch. The later tracker reconciliation also surfaced ImageKit's non-authoritative upstream listing; the final audit closes its local-data-loss path rather than leaving it as an untracked provider caveat. I18N-K4 is pre-existing locale-quality debt rather than a v4.1.7 regression. SYNC-K4 remains a noted multipart lifecycle design concern rather than a confirmed release regression.

The two previously accepted deferred items remain explicit and unchanged:

- **SEC-2 (MED):** a Sigstore verification error can still degrade to unavailable; a hard install gate requires the published-digest and product-disclosure flow.
- **PROV-4 (LOW):** Swift recursive delete still requires pagination to cover trees beyond 10,000 objects.

### 9.4 Verification and tag gate

| Check | Final candidate result |
| ----- | ---------------------- |
| Frontend Vitest | 80 files, 718 passed |
| TypeScript | Passed |
| i18n validation | 46/46 non-English locales, 5,185 keys each, zero errors, warnings, or placeholders |
| Clippy | All targets, warnings denied, passed |
| Rust library | 3,453 passed, 0 failed, 19 ignored |
| CLI | 502 passed, 0 failed |
| Offline integration and doc tests | Passed |
| `cargo audit` | 1,186 dependencies scanned, no vulnerability reported |
| `npm audit` | Zero vulnerabilities |

**Verdict: PRE-TAG COMMIT AUDIT PASS.** No confirmed release-blocking finding remains on the candidate branch. This is not authorization to tag: manifests still identify 4.1.6, PR #569 and the audit fixes must land on `main`, and the release workflow must complete its version bump and all-platform green gate before creating v4.1.7.
