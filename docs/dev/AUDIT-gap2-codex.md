# Gap 2 Pre-Delivery Audit: AeroSync Compare Crypt Awareness

Date: 2026-06-30
Branch: `fix/aerosync-compare-crypt`
Scope: GUI AeroSync Compare for provider-backed remotes with an active Crypt overlay. Gap 1 was not touched.

## Summary

Implemented the Gap 2 fix in the GUI provider Compare path:

- Frontend passes the live overlay vault to `provider_compare_directories` as `cryptVaultId` and `cryptKind`, and passes `null` on plain connections.
- Backend rewrites remote compare rows after the provider scan when a vault is active.
- Remote relative paths are decrypted segment by segment before `build_comparison_results_with_index`.
- rclone-crypt ciphertext sizes are mapped back to plaintext sizes before comparison.
- AeroCrypt remote names are decrypted for Compare, with content-size mapping explicitly deferred.
- S3 recursive scan fast-path is bypassed while a crypt vault is active.
- Ciphertext remote checksums are not compared against plaintext local checksums under crypt overlays.

## Files Touched

- `src/App.tsx`
- `src-tauri/src/provider_commands.rs`
- `src-tauri/src/rclone_crypt.rs`
- `src-tauri/src/sync_core/scan.rs`
- `docs/dev/AUDIT-gap2-codex.md`

Diff stat before this report:

```text
src-tauri/src/provider_commands.rs | 292 ++++++++++++++++++++++++++++++++++++-
src-tauri/src/rclone_crypt.rs      |  19 +++
src-tauri/src/sync_core/scan.rs    |  10 +-
src/App.tsx                        |  45 +++---
4 files changed, 344 insertions(+), 22 deletions(-)
```

## Audit Findings And Fixes

### 1. Plain, non-overlay Compare path

Caught: The post-scan decrypt pass must not affect plain connections.
Patch: `src/App.tsx:8598` derives `cryptVaultId` from live overlay state and leaves it `null` when no overlay is active. `src-tauri/src/provider_commands.rs:5352` runs `decrypt_remote_file_map_for_compare` only when `crypt_vault_id` is present.
Rationale: Plain Compare keeps the same scan, path keys, size values, and checksum behavior unless the frontend explicitly reports an active overlay.

### 2. rclone-crypt size mapping

Caught: Name-only decryption would still classify matching files as size mismatches because remote sizes are ciphertext sizes.
Patch: Added `rclone_decrypted_size` at `src-tauri/src/provider_commands.rs:5020` and apply it to non-directory rclone-crypt remote rows at `src-tauri/src/provider_commands.rs:4991`.
Rationale: Compare now matches plaintext-local size against plaintext-equivalent remote size, preventing redundant re-uploads for unchanged files.

### 3. rclone-crypt per-segment name decryption

Caught: Multi-level remote paths must decrypt every encrypted segment, and when directory name encryption is disabled only the leaf should decrypt. Foreign or undecryptable names must be dropped.
Patch: Added `decrypt_one_name` in `src-tauri/src/rclone_crypt.rs:669`, `decrypt_rel_rclone` in `src-tauri/src/provider_commands.rs`, and normalization tests:

- `provider_commands::tests::decrypt_rel_rclone_decrypts_all_segments_when_directory_names_are_encrypted`
- `provider_commands::tests::decrypt_rel_rclone_decrypts_only_leaf_when_directory_names_are_plain`
- `provider_commands::tests::rclone_compare_normalization_drops_foreign_names_and_maps_size`

Rationale: Compare now sees the same plaintext relative path shape as the local scan. Invalid ciphertext is not passed through as a fake remote filename.

### 4. AeroCrypt name awareness and explicit size deferral

Caught: AeroCrypt can decrypt names now, but content-size mapping needs the native overlay container decoder and should not be claimed complete.
Patch: Added `decrypt_rel_aerocrypt` and `normalize_aerocrypt_remote_files_for_compare`; code comment at `src-tauri/src/provider_commands.rs:5007` states the size deferral. Added test `provider_commands::tests::aerocrypt_compare_normalization_decrypts_names_and_defers_size`.
Rationale: AeroCrypt Compare is now name-aware, while size-policy false positives remain explicitly visible as a follow-up instead of being silently hidden.

### 5. Vault lock and async safety

Caught: The vault mutex must not be held across provider scans or `compare_trees` style work.
Patch: The remote provider scan completes before `decrypt_remote_file_map_for_compare` locks either vault map. The helper takes the lock once, performs an in-memory synchronous normalization, returns the map, and only then the command builds comparison results.
Rationale: No provider list, checksum call, frontend event emit, or compare-index work is performed while holding a crypt vault mutex.

### 6. S3 recursive fast-path

Caught: The flat recursive fast-path should be bypassed under a vault to keep encrypted segment handling in the same BFS shape as the provider browser.
Patch: Added `ScanOptions::disable_recursive_fastpath` at `src-tauri/src/sync_core/scan.rs:81` and gated both fast-path call sites at `src-tauri/src/sync_core/scan.rs:270` and `src-tauri/src/sync_core/scan.rs:373`. Provider Compare sets it when `crypt_compare_active` is true.
Rationale: Non-crypt callers still get the previous default fast-path behavior. Crypt Compare forces the portable scanner.

### 7. Ciphertext checksum mismatch risk

Caught: Even after size correction, a future checksum-enabled Compare would compare local plaintext SHA-256 against remote ciphertext checksum and report false conflicts. Strict checksum mode would be worse because one-sided checksums become conflicts.
Patch: When a crypt vault is active, `provider_compare_directories` disables `options.compare_checksum` and `options.strict_checksum`, does not request remote checksums, and clears remote checksum fields in the normalized rows.
Rationale: Until remote content is hashed as plaintext through the overlay, size and mtime are the only safe compare signals for this path.

### 8. Tauri command registration and IPC shape

Caught: Adding two Tauri `State` parameters plus two optional IPC parameters made the command exceed clippy's argument-count lint. The command is already registered in `generate_handler!`.
Patch: Kept registration unchanged and added a local `#[allow(clippy::too_many_arguments)]` on `provider_compare_directories`.
Rationale: The broad signature is the Tauri IPC contract, not an internal API smell. `cargo clippy --all-features --tests -- -D warnings` proves the command still resolves.

### 9. Frontend live overlay binding

Caught: Compare previously invoked the backend without the active vault, so backend code could not know how to decrypt the remote scan.
Patch: `src/App.tsx:8598` derives `cryptVaultId`; `src/App.tsx:8599` derives `cryptKind`; `src/App.tsx:8643` and `src/App.tsx:8644` pass both only to provider Compare. Added both overlay states to the `openAeroSync` dependency list.
Rationale: Plain connections pass no crypt metadata. Provider-backed crypt overlays now send exactly the live vault that the browser overlay already unlocked.

### 10. Fail-closed crypt state handling

Caught: Returning ciphertext rows when a vault id is present but the backend vault is missing would recreate the bug silently.
Patch: `decrypt_remote_file_map_for_compare` returns an error for missing vaults, missing kind, or unsupported kind.
Rationale: An active overlay compare must be correct or fail visibly. Silent ciphertext comparison is the dangerous behavior this fix removes.

## Tests Added

Rust unit tests added under `provider_commands::tests`:

- `rclone_decrypted_size_matches_encrypted_content_lengths`
- `decrypt_rel_rclone_decrypts_all_segments_when_directory_names_are_encrypted`
- `decrypt_rel_rclone_decrypts_only_leaf_when_directory_names_are_plain`
- `rclone_compare_normalization_drops_foreign_names_and_maps_size`
- `aerocrypt_compare_normalization_decrypts_names_and_defers_size`

Targeted runs before the full gate:

```text
cargo test rclone_decrypted_size_matches_encrypted_content_lengths --lib
test provider_commands::tests::rclone_decrypted_size_matches_encrypted_content_lengths ... ok

cargo test compare_normalization --lib
test provider_commands::tests::aerocrypt_compare_normalization_decrypts_names_and_defers_size ... ok
test provider_commands::tests::rclone_compare_normalization_drops_foreign_names_and_maps_size ... ok

cargo test decrypt_rel_rclone --lib
test provider_commands::tests::decrypt_rel_rclone_decrypts_only_leaf_when_directory_names_are_plain ... ok
test provider_commands::tests::decrypt_rel_rclone_decrypts_all_segments_when_directory_names_are_encrypted ... ok
```

## Gate Outputs

Extra frontend type safety check:

```text
npm run typecheck
> aeroftp@4.1.0 typecheck
> tsc --noEmit
exit 0
```

Required gates:

```text
cargo fmt --all
exit 0
```

```text
cargo clippy --all-features --tests -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 36s
exit 0
```

```text
cargo clippy --bin aeroftp-cli -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 50s
exit 0
```

```text
cargo test
test result: ok. 2431 passed; 0 failed; 10 ignored; 0 measured; 0 filtered out; finished in 1053.39s
test result: ok. 417 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.53s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
integration and doc test groups completed with 0 failures; live WAN and Docker tests were ignored by their existing guards.
exit 0
```

```text
npm run i18n:validate
Languages checked: 46
Languages clean: 46/46
Reference keys: 4797
Total errors: 0
Total warnings: 0
Missing keys: 0
Extra/orphan keys: 0
[NEEDS TRANSLATION]: 0
PASSED: All translations are complete and structurally valid!
exit 0
```

## Not Verified Live

- No live rclone-crypt remote was available in this session. The required live validation remains: upload through the overlay, run AeroSync Compare twice, verify the second run is all match and 0 re-upload.
- No live AeroCrypt remote was available in this session. Expected current behavior is name-aware Compare with the documented size caveat.
- OAuth Gap 1 live verification was intentionally not repeated.

## Deferrals And Boundaries

- AeroCrypt content-size decryption remains deferred. This fix decrypts AeroCrypt remote names but does not map AeroCrypt ciphertext file sizes to plaintext sizes.
- CLI and MCP sync/check paths remain deferred for crypt overlays. This change targets the GUI `provider_compare_directories` path Ehud reported.
- Non-provider `compare_directories` was not changed. The overlay offer currently applies to provider-backed FTP/FTPS and non-FTP providers in the GUI path used here.
- No tracker, GitHub post, or push was performed.

## Deviations From The Plan

- I made the crypt compare path fail closed when the vault id is present but the backend vault or kind is invalid. The section-8 snippet returned the original ciphertext rows in that case, but that would silently reproduce the bug.
- I disabled checksum comparison under active crypt overlays because remote provider checksums are over ciphertext, not plaintext. This was found during audit and fixed in the same pass.
