# Handoff: AeroSync EC P5 autostart

Track: AeroSync Error Correction, branch `feat/aerovault-v4-ecc`.

This handoff is intentionally auto-start. A new Codex chat should read this file,
verify the working tree, and begin P5-A immediately. Do not stop to ask for a
plan unless local state makes the task unsafe or impossible.

## Current State

Repository: `/var/www/html/FTP_CLIENT_GUI`

Branch: `feat/aerovault-v4-ecc`

P4 baseline commit:

```text
e16c1fe8e feat(aerosync): add sync error correction CLI flag
```

P4 is complete and was verified locally with:

```bash
cd src-tauri
cargo test --bin aeroftp-cli
cargo clippy --bin aeroftp-cli --all-features --tests -- -D warnings
cargo fmt --all -- --check
cd ..
npm run i18n:validate
```

P4 shipped:

- `aeroftp-cli sync --error-correction[=low|medium|quartile|high|<5-50>]`
  plus alias `--ec`.
- Named level parse: `low=7`, `medium=15`, `quartile=25`, `high=30`;
  numeric values clamp to `5..50`.
- Hook into the hand-rolled CLI `cmd_sync` loop in
  `src-tauri/src/bin/aeroftp_cli.rs`.
- `*.aerorec` auto-excluded from sync comparisons when EC is enabled.
- `.aerorec` sidecars generated after confirmed uploads.
- CLI JSON counters when EC is enabled:
  `ec_generated`, `ec_skipped_too_large`, `ec_generate_failed`.
- A public wrapper in `src-tauri/src/sync.rs`:
  `generate_sync_error_correction_sidecar_after_upload(...)`.
- Docs in `docs/CLI-GUIDE.md`, including `AEROSYNC-EC`.

## Non-Negotiable Invariants

- Do not touch the Reed-Solomon codec, AVEC payload format, AERC1 sidecar
  format, `.aerorec` extension, or binding semantics.
- Do not change AeroVault `.aerovault.rec`.
- Do not touch `peer-l0/`.
- Do not touch the CLI-TUI track or
  `/home/axpdev/.claude/projects/-var-www-html-FTP-CLIENT-GUI/memory/project-cli-tui-track.md`.
- Keep changes scoped to AeroSync EC follow-up behavior and docs/tests.
- No tag or merge.
- Commit conventional commits in English.
- Push only to `origin feat/aerovault-v4-ecc` after green gates.

## Autostart Procedure

1. Start in `/var/www/html/FTP_CLIENT_GUI`.
2. Run:

   ```bash
   git status --short --branch
   git log --oneline -5
   ```

3. If the worktree is dirty with changes you did not make, inspect them and do
   not overwrite. If they are unrelated, leave them alone.
4. Begin **P5-A** below immediately.
5. After P5-A is implemented, run the Rust gates. If green, commit P5-A.
6. If there is enough context/time and no risky drift, continue to **P5-B**.
7. After the final completed checkpoint, run all gates listed below, commit, and
   push the branch.

## P5-A: Delete Companion EC Sidecars

Goal: when sync deletes a protected remote file, it should also best-effort
delete its companion `<remote-file>.aerorec` sidecar so EC does not leave
orphan metadata behind.

Scope:

- Apply to remote deletes when EC is enabled.
- Cover both:
  - the hand-rolled CLI sync loop in `src-tauri/src/bin/aeroftp_cli.rs`;
  - the shared Rust sync engine in `src-tauri/src/sync.rs`, if its delete path
    can be wired cleanly without broad refactor.
- Do not affect local-to-local sync.
- Do not make sidecar delete failure fail the primary file delete.
- Treat missing sidecar as OK.
- Warn/report sidecar delete failures honestly.

Suggested implementation notes:

- Reuse the existing sidecar path construction rather than duplicating string
  rules. `src-tauri/src/error_correction/aerosync.rs` has the private helper
  `sync_error_correction_sidecar_path(remote_path)`.
- Since the codec module is intentionally private, add a small public wrapper in
  `src-tauri/src/sync.rs`, for example:

  ```rust
  pub fn sync_error_correction_sidecar_remote_path(remote_path: &str) -> String
  ```

  Internally it can call the private aerosync helper. Avoid changing codec
  visibility if possible.

- CLI anchor:
  - `src-tauri/src/bin/aeroftp_cli.rs`
  - `cmd_sync(...)`
  - remote delete loop over `to_delete_remote`
  - current primary delete call resembles `provider.delete(&remote_path).await`.

- Shared sync engine anchor:
  - `src-tauri/src/sync.rs`
  - `perform_delete_remote(...)` around the provider delete path.
  - `SyncOptions.error_correction` is already available to `sync_tree_core`.

Counter/report suggestion:

- Prefer adding optional counters only when EC is enabled:
  - CLI JSON: `ec_sidecar_deleted`, `ec_sidecar_delete_failed`.
  - Shared `SyncReport`: same names or similarly clear names.
- If adding counters causes too much blast radius, a warning-only best-effort
  delete is acceptable for P5-A, but document the tradeoff in the commit body.

Tests:

- At minimum add unit tests for any new pure helpers/counter accumulation.
- If practical, add a provider-mock or focused shared-engine test proving
  missing sidecar is ignored and sidecar delete failure does not fail the
  primary delete.

Suggested P5-A commit:

```text
fix(aerosync): delete EC sidecars with remote sync deletes
```

## P5-B: Sync-Doctor EC Estimate

Only start this if P5-A is green and committed.

Goal: when the user asks sync-doctor to plan a sync with EC enabled, surface the
expected sidecar cost before executing the sync.

Scope:

- Add `sync-doctor --error-correction[=LEVEL]` plus alias `--ec`, using the same
  level semantics as `sync`.
- Reuse the CLI parser helper if possible:
  `parse_sync_error_correction_level_pct(...)`.
- Auto-exclude `*.aerorec` during the doctor scan when EC is enabled.
- JSON output should include stable EC estimate fields when enabled, such as:
  - `ec_enabled`
  - `ec_level_pct`
  - `ec_estimated_sidecars`
  - `ec_estimated_overhead_bytes`
  - `ec_skipped_too_large`
  - `ec_phase1_max_file_size`
- Text output should include a concise EC estimate block.
- If sync-doctor emits a `next_command`, include the matching
  `--error-correction` flag.

Important:

- This is an estimate, not generation.
- Do not compute actual parity in sync-doctor.
- Estimate against files planned for upload. Use the Phase 1 256 MiB cap.
- Storage overhead can be approximate. A simple conservative estimate based on
  level percentage is enough for this slice unless a better local helper already
  exists.

Suggested P5-B commit:

```text
feat(aerosync): estimate EC sidecar cost in sync doctor
```

## Stop Criteria

Stop after P5-A if:

- shared sync engine delete wiring requires a broad refactor;
- tests reveal existing behavior ambiguity around delete semantics;
- the next step would touch codec/AERC1 or unrelated sync architecture.

Stop after P5-B if both P5-A and P5-B are green.

Do not start download verify/repair in this handoff. That is a later slice.

## Gates

From `src-tauri/`:

```bash
cargo test --bin aeroftp-cli
cargo clippy --bin aeroftp-cli --all-features --tests -- -D warnings
cargo fmt --all -- --check
```

From repo root:

```bash
npm run i18n:validate
```

If any TypeScript/UI files are touched, also run:

```bash
npm run typecheck
npm run test:unit
```

## Push Policy

After the last completed checkpoint is committed and gates are green:

```bash
git status --short --branch
git push origin feat/aerovault-v4-ecc
```

No tags. No merges.

## New Chat Starter

Paste this to the next Codex chat:

```text
Work in /var/www/html/FTP_CLIENT_GUI on branch feat/aerovault-v4-ecc.
Read docs/dev/HANDOFF-2026-06-12_aerosync-ec-p5-autostart.md completely.
It is auto-start: begin P5-A immediately, verify, commit, and if green continue
to P5-B. Run the listed gates and push only origin feat/aerovault-v4-ecc.
Do not touch codec/AERC1, peer-l0, or the CLI-TUI track.
```
