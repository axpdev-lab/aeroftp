# Handoff 2026-05-27 - MU-FE P7 local release gate

Branch: `codex/multi-user-partitioning`
Worktree: `/var/www/html/FTP_CLIENT_GUI_MU`
Push/tag: not performed.

## Closed

- MU-FE-P4b: admin-aware `UsersManagePanel`.
  - Admin badge.
  - Admin can edit peer metadata, promote/revoke admin, delete eligible peers.
  - Non-admin sees account actions only for self.
  - Add/reorder accounts are admin-only in UI and Tauri command gate.
  - Destructive admin reset dialog requires `RESET`, new password, confirm password, and shows target storage stats.
- MU-FE-P5:
  - Current account password is required for self password rotation.
  - `WRONG_PASSPHRASE` and `PASSPHRASE_REQUIRED` map to user-facing errors.
  - Account unlock button shows spinner parity.
  - `PasswordStrengthBar` added to master setup, settings master change, account create/change, and admin reset.
- MU-FE-P5-BUG:
  - Added `MASTER_PASSWORD_CHANGED_EVENT`.
  - Settings enable/disable and master setup dispatch the event.
  - `App.tsx` updates `masterPasswordSet` and clears stale lock state when master mode is disabled.
- MU-FE-P6:
  - Backend `set_user_avatar` plus Tauri command `user_partitions_set_user_avatar`.
  - Gated by self-or-admin.
  - `UserAvatar` supports `data:image/*` avatars.
  - Users panel supports post-creation avatar/color edit and IconPicker reuse.
- MU-FE-FOUND-BACKUP:
  - Keystore metadata now includes `manifestVersion`.
  - Added `KeystoreScope::{AllUsers, SingleUser}` placeholder.
  - Current export passes `KeystoreScope::AllUsers`; behavior remains unchanged.
- MU-FE-P7:
  - Added `docs/dev/roadmap/APPENDIX-MULTI-USER/MU-REG_MATRIX.md`.

## Gate

```text
npm run typecheck                         : clean
npx vitest run                            : 18 files, 228 passed
cargo test --lib user_partitions          : 36 passed
cargo test --lib keystore_export          : 15 passed
cargo clippy --lib -- -D warnings         : clean
cargo test --lib                          : 1819 passed, 8 ignored
npm run i18n:validate                     : PASSED with 38 warnings
```

## Remaining

- R4 owner live gate: transfer survives user switch.
- No push and no tag done.

