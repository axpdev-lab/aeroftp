# Multi-User Account Partition

> _Last updated: 2026-06-28_

*Introduced in v4.0.0.*

AeroFTP can split its encrypted vault into **per-user partitions** so that several people sharing one machine each keep their own server profiles, AeroSync settings, and credentials, isolated from one another. The feature is fully optional: a single-user install behaves exactly as before, and migration from an older single-user keystore is automatic and idempotent.

## How it works

- **Encrypted partitions.** Each user owns an isolated partition stored in an additive `user_partitions.db` database (the legacy `vault.db` credential vault is left unchanged). The partition key is derived with **Argon2id** and the partition payload is encrypted with **AES**. A user's data is never readable from another user's session.
- **Account Lock Screen.** On launch, AeroFTP presents the configured users (avatar, name) and unlocks the selected partition with that user's passphrase. A user with no passphrase uses device-wrapped access (no prompt). The unlock prompts use honest crypto-stack labels (Argon2id key derivation, AES partition encryption).
- **Partition-aware everywhere.** Server profiles, saved servers, AeroSync settings, export/import, the keystore wizard, and AeroCloud all read and write the active user's partition.
- **Admin role.** An opt-in admin role gates user management and exposes an admin reset-passphrase path. A last-admin guard prevents an installation from locking itself out.
- **Migration.** An existing v3.8.x single-user keystore is migrated into the first user's partition on first launch, preserving every profile and credential.

## Using it in the GUI

- Add, rename, reorder, and delete users from the **Manage Users** panel, opened via the account dropdown (or the Account Lock Screen) (admin role required to manage other users).
- Set or change a per-user passphrase, or remove it to return to device-wrapped access.
- Switch the active user from the account dropdown; the Account Lock Screen re-appears when a passphrase-protected partition is selected.

## Using it from the CLI

Multi-user is exposed through the `users` subcommand and a global `--user` flag.

```bash
# List, add, switch, and manage users
aeroftp-cli users list
aeroftp-cli users add alice
aeroftp-cli users switch alice            # persists the active user
aeroftp-cli users set-passphrase alice
aeroftp-cli users rename alice alicia
aeroftp-cli users delete alice
aeroftp-cli users sort alice bob carol    # order for the GUI / lock screen
aeroftp-cli users lock                    # lock the in-memory session

# Run any command as a specific user for this invocation only
aeroftp-cli --user alice ls --profile "Backup" /
aeroftp-cli --user alice sync --profile "Backup" /local /remote
```

- `--user` is **per-invocation**: it selects the partition for that single command without changing the persistent active user (`users switch` does that).
- When the selected partition is passphrase-protected, supply it with `--user-passphrase`, `--passphrase-file`, or the `AEROFTP_USER_PASSPHRASE` environment variable; otherwise the CLI prompts on a TTY.
- `--user` is optional everywhere and defaults to the active user, so existing scripts keep working unchanged.

## Per-user groups, favourites, and the default user (v4.1.0)

- **Per-user groups and favourites.** Server groups and favourites used to be a single global vault blob shared across every local user. In v4.1.0 they route through each user's encrypted partition, so one user's grouping and starred servers stay private to that user. On first launch after the upgrade, a one-time best-effort seed copies the legacy global blob into the active user's partition so existing groups and favourites carry over.
- **Default user as a real column.** The default user is now a real `is_default` database column (previously a localStorage flag), with Manage Users parity in the GUI. The default user's partition is auto-unlocked on launch.
- **Vault-aware sidebar.** Standard buckets hide when they are empty (at zero), while user-defined groups always remain visible.
- **CLI.** In `aeroftp-cli users -i`, the `f` / Fav action marks the default user, matching the GUI behaviour.

## Security notes

- Passphrases are never persisted; partition keys live in memory only for the duration of an unlocked session and are zeroized on lock/logout.
- A cross-user deduplication probe uses HMAC keying so the app can detect shared credentials without exposing one user's secrets to another.
- See [SECURITY.md](../SECURITY.md) and [UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md) for the underlying vault architecture.
