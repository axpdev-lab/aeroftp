# MU-REG matrix - APPENDIX-MULTI-USER

> Updated: 2026-05-27.
> Branch `codex/multi-user-partitioning`, post MU-FE P7 local gate.
> No push, no tag.

## Summary

| R | Category | Status | Evidence |
|---|---|---|---|
| R1 | Clean install | PASS | Existing boot predicate and lock-screen cache tests |
| R2 | Forward migration | PASS | Schema v2->v3 idempotent migration tests |
| R3 | Partition leak | PASS | Owner GUI PASS plus partition CRUD tests |
| R4 | Transfer survives switch | GUI REQUIRED | Structurally unchanged transfer engine, live owner test still required |
| R5 | Cross-user dedup | PASS | MU-7 HMAC keyed probe tests |
| R6 | Session leak | PASS | USER_SESSION clear-on-switch and unlock tests |
| R7 | Keystore export foundations | PASS for v4.0.0 foundation | Manifest version and `KeystoreScope::AllUsers` placeholder wired |
| R8 | MCP/agent partition-aware | RESIDUAL | Tracked for MU-MCP follow-up |
| R9 | Watcher per-user scope | RESIDUAL | Tracked for MU-4 P3 follow-up |
| R10 | Per-user lockout | PASS | Persistent `user_lockout_<id>` tests |
| R11 | Intra-user duplicate block | PASS | Existing dedup key path |
| R12 | CLI allowlist drift | PASS | Existing CLI dispatcher test |
| R13 | Passphrase boundary | PASS | Wrong passphrase and verifier tests |
| R14 | DEK lifetime | PASS | `SecretBox` + session clear tests |
| R15 | device root isolation | PASS | Passphrase user unlock path uses Argon2 passphrase key |
| R-Self-Only | Non-admin self-only account edits | PASS | Backend self-or-admin gate plus UsersManagePanel UI gating |
| R-Admin | Admin account management | PASS | Admin badge, promote/revoke, peer edit/delete, destructive reset |

## MU-FE Closure

| Slice | Status | Notes |
|---|---|---|
| MU-FE-P4b | DONE | UsersManagePanel admin badge, self-only UI, admin peer controls, destructive reset dialog |
| MU-FE-P5 | DONE | Current password UX, `WRONG_PASSPHRASE`/`PASSPHRASE_REQUIRED` mapping, unlock spinner, strength bars |
| MU-FE-P5-BUG | DONE | `MASTER_PASSWORD_CHANGED_EVENT` refreshes App titlebar lock state after enable/disable |
| MU-FE-P6 | DONE | Backend `set_user_avatar`, UI avatar/color edit, IconPicker reuse for image avatars |
| MU-FE-FOUND-BACKUP | DONE | Keystore metadata `manifestVersion`, `scope`, `KeystoreScope` placeholder |
| MU-FE-P7 | DONE locally | Gate passed, release handoff written |

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

## Remaining Owner Live Gate

R4 still needs a live transfer-survives-switch test before tagging v4.0.0:

1. Start a large upload/download as user A.
2. Switch to user B during the transfer.
3. Confirm the transfer continues and completion state remains visible.

