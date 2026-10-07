# v4.2.3 requested items: reverse triage

2026-10-05 snapshot of [tracker #1081](https://github.com/axpdev-lab/aeroftp/issues/1081), baseline `2f62c68a1` (v4.2.2). Numbers identify requested rows in this snapshot. Implementation evidence does not establish a deferred live check as passed.

| Row | Assessment and evidence | Bundle |
|---|---|---|
| 31 | Initial code inspection suggested GUI visibility/sort already worked. Subsequent live testing reproduced a wrong vault-key read and a Saved % id mismatch, in addition to missing persistent CLI choices and the GUI breakdown checkbox. See [the row 31 verification](V423-PROFILE-PREFERENCES.md). | Profile preferences |
| 30 | Reproduced: `clone_for_list` mints an unconnected SFTP worker, `list` calls `get_sftp` without dialing. v4.2.2 live scan emits eight cold-worker reconnect warnings. | **Storage walker, selected** |
| 29 | Reproduced: `walk_used_bytes` sums the vector the walker retains. v4.2.2 lab WebDAV: 60,800 files, 875 folders, 4,704,965,753 bytes, 102,672 KiB peak RSS. | **Storage walker, selected** |
| 28 | 2026-10-07 implementation in progress: CLI 2.11.2 checked against official code; local helper cache, release/commit pins and SHA-256 rejection tests added. Packaging/PR verification tracked in [the helper report](V423-LINUXDEPLOY-PINS.md). Not merged. | Linux packaging |
| 27 | Upstream dependent, rechecked 2026-10-07: release API still lists only continuous, 20251108, old. The 0700 fix is in continuous only; retain dated runtime pin and digest, keep requested 27 open. | Linux packaging |
| 26 | Open: #1038 explicitly deferred Fetch/Git STDIO presets and pinned artifact installation; current presets supply remote DeepWiki. | MCP supply chain |
| 25 | Platform dependent: #1051 deliberately retained CRLF handling until Windows clones refresh. LICENSE LF requires an actual NSIS page check. | Windows checkout / installer |
| 24 | Fixes shipped, live checks open: #981 implements FTPS final-reply handling, OneDrive stale-id resolution, ambiguous-trash refusal, Twake retry progress and Proton argument handling. Its live list was deferred. | Provider live contracts |
| 23 | Fix shipped, GUI live check open: #979 records kept folders/reasons. PR says runner tests, not a live GUI Mirror. | Mirror safety |
| 22 | Open: Swift recursive deletion advances until empty and checks its marker. Ordinary `SwiftProvider::list` still stops at `entries.len() < 10000`, so a short server page can truncate it. Distinguish the two paths. | Provider pagination |
| 21 | Fix shipped, HNS live check open: Azure stat/removal recognizes `hdi_isfolder` and resource type; tests are fixtures, not a real HNS account. | Provider live contracts |
| 20 | Open in dependency: pinned aerovault 0.6.6 `v3/chunking::zstd_decompress_bounded` still uses unchecked `plaintext_len + 1`. #966 fixed the app overlay, not this helper. | Archive / vault lengths |
| 19 | Open in extraction dependency: app uses unrar 0.5 / unrar_sys 0.5.8, whose RAR5 redirection resolves hardlinks. TAR in-root handling in lib.rs does not fix RAR5. Needs a RAR5 fixture. | Archive / vault lengths |
| 18 | Partially stale: #987 imports the three host vault files atomically into a vault-free sandbox, handles JSON vault.db, and rolls back failures. `flatpak_import_copies_a_whole_host_vault_with_its_json_vault_db` pins this. Existing sandbox vaults are deliberately skipped; migration into those needs separate scope. | Flatpak migration |
| 17 | Post-tag check open: #999 implemented update metadata/zsync publication; asset existence is not an AppImageUpdate/AM delta run. | Linux packaging |
| 16 | Open: resume needs a persisted source-version binding for existing .aerotmp bytes. #885/#887 multi-range checks are a different path. | Source consistency |
| 15 | Investigation, not a proven bug: concurrent upload part readers on Azure/Box/Drime/Dropbox need a local-file change experiment and a source snapshot contract. | Source consistency |
| 14 | Open: atomic_rename defaults to Unsupported in transfer_dag/capabilities.rs; measured SFTP posix-rename support is not surfaced there. | Transfer reporting |
| 13 | Open: multi_thread.rs and callers warn in logs on fallback. Effective planner/progress fixes do not provide a terminal refusal reason. | Transfer reporting |
| 12 | Open: range_source_changed_through compares a post-transfer fingerprint, retrying failed reads. This does not prove a multi-step replacement stayed stable throughout writing. | Source consistency |
| 11 | Open: 412 lacks distinct capability-refusal versus source-replacement classification. Test an always-refusing gateway and a replacement control. | Source consistency / reporting |
| 10 | Open: transfer_dag_batch awaits multipart_abort on errors/token cancellation; no drop guard covers a hard-dropped runner. #766 provider guards protect a different path. | Multipart lifetime |
| 9 | Open: EN defaultSaltToggle/defaultSaltTierMeaning still promise password-alone recovery. Choose reconstruction or corrected copy in every locale. | AeroCrypt recovery |
| 8 | Open, measure first: S3 trailing-slash enumeration still needs the slashless-key probe. Optimization needs sibling-prefix and real-file survival tests. #979 empty-folder removal is separate. | S3 delete performance |

## Proposed bundles and sequence

1. **30 + 29, storage walker:** selected; shared scanner, live CLI/GUI proofs, no new UI strings. Keep totals, progress, caps, cancellation, unreadable-folder handling and ordinary sync rows.
2. **31, profile preferences:** fix the live-reproduced settings reader, implement persisted choices and breakdown UI, with one cross-process preference contract.
3. **28 + 27 + 17, Linux packaging:** helper pinning and real delta verification; runtime waits for a dated upstream release.
4. **24 + 23 + 22 + 21, provider live contracts:** split account-dependent subcases; ordinary Swift short-page listing merits a regression fixture.
5. **20 + 19, archive/vault lengths:** upstream crate fixes may be needed; bounded-length and RAR5 fixture proofs.
6. **16 + 15 + 12 + 11, source consistency**, then **14 + 13, reporting:** related investigation, distinct publication/refusal semantics.
7. Keep **26, 25, 18, 10, 9, 8** separate: installers, Windows UI, vault migration, multipart lifetime, crypto recovery and deletion need different gates.

No whole requested row is established as done on main by this triage. Narrow the already implemented parts of 18; keep 21/23/24 as live verification work. The later row 31 live investigation corrects its initial assessment above.

## Selected bundle: verification

Implementation: remote BFS scans have a totals-only mode with file counts independent of retained entries. `walk_used_bytes` takes the final observer totals; normal sync/compare scans still retain their rows. The SFTP listing path lazily dials through the captured, host-key-pinned connection specification before its first READDIR.

| Check | Baseline v4.2.2 | Branch | Result |
|---|---|---|---|
| Release CLI, lab WebDAV tree, two runs per build | 102,672 / 104,076 KiB peak RSS | 89,168 / 87,340 KiB | Same 4,704,965,753 bytes, 60,800 files, 875 folders, complete, no unreadable folders |
| Release CLI, SFTP NAS, requested depth 2 | 8 cold-worker reconnect warnings | 0 | Same 352,583,781 bytes, 25 files, 80 folders, depth-limited but not truncated |
| GUI, Calculate used storage button on lab WebDAV | | 26 progress events; visible chip rises through 101 MB (3 files), 1003 MB (7 files), 3.8 GB (5262 files) | Final 4.4 GB (60800 files); exact byte/file totals persisted in the active profile |
| GUI, rescan then Cancel | | Previous complete figure restored | 4.4 GB / 60800 files remains stored; subsequent remote listing succeeds |
| GUI SFTP connection and bounded scan via IPC | | Same as release CLI on the test subtree | 100,000,000 bytes, one file, complete; no cold-worker reconnect warning |

Both memory measurements use optimized release CLIs. Timings were 7.30 / 7.36 s before and 7.83 / 9.96 s after while other compilation and GUI work was active; these runs establish totals and process memory, not a speed claim. The optimization removes retention across directories; provider listings, in-flight batches and the directory queue still occupy memory. Native S3/WebDAV recursive fast paths are unchanged.

Focused verification: cargo fmt and diff whitespace checks; 16 used_scan tests, 50 sync_core::scan tests (including retained-row/totals-only cap parity on both listing models), 51 SFTP unit tests, four WebDAV scan fixtures, one Flatpak JSON-import proof, and the SFTP wire fixture (first clone listing without any walker retry, then warm reuse). Frontend lifecycle/persistence tests: 34 passed. GUI ran under G_SLICE=always-malloc, G_DEBUG=gc-friendly and MALLOC_CHECK_=3 without an abort. This does not close the separately deferred tray corruption investigation.

Live GUI verification used a separate portable development vault populated by an encrypted export of only the lab WebDAV and test NAS profiles, including current credentials. No remote file was changed. The PR's GitHub Actions runs remain the authoritative complete gate; no full local ci:pre-push was duplicated.
