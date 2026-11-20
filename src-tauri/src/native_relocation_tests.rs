//! Synthetic, isolated regressions: a native Proton Drive session belongs to
//! one account, on the production save, refresh, sign-out, import and
//! copy-user / move-user paths.
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::*;
use crate::providers::proton_native::{
    api::SessionStore,
    session::{ProtonSession, SessionTokens, StoredSession},
    vault,
};
use secrecy::SecretString;
use serde_json::json;

const MODE: &str = "AEROFTP_NATIVE_RELOCATION_FIXTURE";
const SOURCE_PASSWORD: &str = "synthetic source password";
const TARGET_PASSWORD: &str = "synthetic target password";

fn session(uid: &str, generation: &str) -> ProtonSession {
    ProtonSession {
        tokens: SessionTokens {
            uid: uid.into(),
            access_token: SecretString::from(format!("access-{generation}")),
            refresh_token: Some(SecretString::from(format!("refresh-{generation}"))),
        },
        key_password: SecretString::from("synthetic-key-password".to_string()),
    }
}

fn raw(session: &ProtonSession) -> String {
    serde_json::to_string(&StoredSession::from(session)).unwrap()
}

/// The generation of the pair a saved session holds.
fn generation(raw: &str) -> String {
    let saved: StoredSession = serde_json::from_str(raw).unwrap();
    assert_eq!(saved.key_password, "synthetic-key-password");
    let generation = saved
        .access_token
        .strip_prefix("access-")
        .unwrap()
        .to_string();
    assert_eq!(saved.refresh_token, Some(format!("refresh-{generation}")));
    generation
}

fn key(id: &str) -> String {
    vault::session_key(Some(id))
}

/// The generation of the session `owner` loads for profile `id`.
fn loaded(owner: vault::SessionOwner, id: &str) -> Option<String> {
    vault::load_session(owner, Some(id))
        .unwrap()
        .map(|session| generation(&raw(&session)))
}

/// Whether this is the child process the test `name` runs in. Outside it, the
/// test runs again there and its result is checked: the real encrypted vault
/// and partition DB are isolated from the owner's credentials and from
/// parallel lib tests, as the auth fixture does.
fn in_isolated_child(name: &str) -> bool {
    if std::env::var(MODE).as_deref() == Ok("suite") {
        return true;
    }
    let executable = std::env::current_exe().unwrap();
    let root = tempfile::Builder::new()
        .prefix("native-relocation-")
        .tempdir_in(executable.parent().unwrap())
        .unwrap();
    let child = root.path().join(format!(
        "native-relocation-test{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::hard_link(&executable, &child)
        .or_else(|_| std::fs::copy(&executable, &child).map(|_| ()))
        .unwrap();
    std::fs::write(root.path().join("portable.marker"), b"").unwrap();
    let empty_path = root.path().join("no-external-tools");
    std::fs::create_dir(&empty_path).unwrap();
    let test = format!("user_partitions::native_relocation_tests::{name}");
    let output = std::process::Command::new(child)
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(MODE, "suite")
        .env("PATH", empty_path)
        .env("AEROFTP_MASTER_PASSWORD", "synthetic native fixture master")
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-native-relocation",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// The legacy owner, a source account unlocked in this process and a target
/// account protected by a passphrase and locked, in the child's vault.
struct Accounts {
    _test: TestLock,
    store: CredentialStore,
    conn: Connection,
    root_key: [u8; 32],
    legacy: i64,
    source: UserMetadata,
    target: UserMetadata,
    target_dek: SecretKey,
}

impl Accounts {
    fn open() -> Self {
        let test = test_lock();
        CredentialStore::init().unwrap();
        let store = CredentialStore::from_cache().unwrap();
        init_or_migrate_cli(&store).unwrap();
        let mut conn = open_or_init_cli().unwrap();
        let root_key = store.derive_user_partition_wrapping_key();
        let legacy = active_user_id(&conn).unwrap().unwrap();
        let source = create_user(
            &mut conn,
            &root_key,
            "Source",
            None,
            None,
            Some(SOURCE_PASSWORD),
        )
        .unwrap();
        let target = create_user(
            &mut conn,
            &root_key,
            "Target",
            None,
            None,
            Some(TARGET_PASSWORD),
        )
        .unwrap();
        unlock_user_transient(&conn, &root_key, source.id, Some(SOURCE_PASSWORD)).unwrap();
        let root_secret = user_crypto::secret_key_from_bytes(&root_key);
        let target_dek =
            resolve_user_dek_scoped(&conn, &root_secret, target.id, Some(TARGET_PASSWORD)).unwrap();
        Self {
            _test: test,
            store,
            conn,
            root_key,
            legacy,
            source,
            target,
            target_dek,
        }
    }

    /// The source account.
    fn owner(&self) -> vault::SessionOwner {
        vault::SessionOwner::for_user(Some(self.source.id))
    }

    fn owners_owner(&self) -> vault::SessionOwner {
        vault::SessionOwner::for_user(Some(self.legacy))
    }

    fn add_profile(&self, user: i64, id: &str) {
        let mut db = open_or_init_cli().unwrap();
        let mut profiles = list_server_profiles_for(&db, &self.root_key, user).unwrap();
        profiles.push(
            json!({"id":id,"name":id,"protocol":"proton","host":"drive.proton.me",
            "port":443,"username":format!("{id}@example.test"),"options":{"proton_mode":"native"}}),
        );
        replace_server_profiles_for(&mut db, &self.root_key, user, &profiles).unwrap();
    }

    /// The profile ids `user` lists; the locked target's read with its key.
    fn profile_ids(&self, user: i64) -> Vec<String> {
        let profiles = if user == self.target.id {
            read_profiles_with_dek(&self.conn, user, &self.target_dek)
        } else {
            list_server_profiles_for(&self.conn, &self.root_key, user)
        };
        profiles
            .unwrap()
            .iter()
            .filter_map(|profile| profile["id"].as_str().map(str::to_string))
            .collect()
    }

    fn target_owner(&self) -> vault::SessionOwner {
        vault::SessionOwner::for_user(Some(self.target.id))
    }

    /// The target unlocked in this process, which locks the source.
    fn unlock_target(&self) {
        unlock_user_transient(
            &self.conn,
            &self.root_key,
            self.target.id,
            Some(TARGET_PASSWORD),
        )
        .unwrap();
    }

    /// The source unlocked in this process again, which locks the target.
    fn unlock_source(&self) {
        unlock_user_transient(
            &self.conn,
            &self.root_key,
            self.source.id,
            Some(SOURCE_PASSWORD),
        )
        .unwrap();
    }

    /// A source profile signed in with session `uid`.
    fn seed(&self, id: &str, uid: &str) {
        self.add_profile(self.source.id, id);
        vault::store_session(self.owner(), Some(id), &session(uid, "r1")).unwrap();
    }

    fn target_row(&self, id: &str) -> Option<String> {
        read_credential_with_dek(&self.conn, self.target.id, &self.target_dek, &key(id))
            .unwrap()
            .map(|raw| generation(&raw))
    }

    /// The generation of profile `id`'s session in the vault key, in the
    /// legacy owner's row and in the source's row, each read as it is stored.
    fn copies(&self, id: &str) -> (Option<String>, Option<String>, Option<String>) {
        let row = |user: i64| {
            get_user_credential_for(&self.conn, &self.root_key, user, &key(id))
                .unwrap()
                .map(|raw| generation(&raw))
        };
        (
            self.store.get(&key(id)).ok().map(|raw| generation(&raw)),
            row(self.legacy),
            row(self.source.id),
        )
    }

    fn in_vault(&self, id: &str) -> bool {
        self.store.get(&key(id)).is_ok()
    }

    fn relocate(&self, from: i64, id: &str, new_id: &str, moved: bool) -> ProfileRelocation {
        relocation(&self.store, self.target.id, from, id, new_id, moved).unwrap()
    }
}

/// A Copy or a Move of profile `id` of `from` into `target`, on a thread of
/// its own when the caller needs one (a connection is not shared).
fn relocation(
    store: &CredentialStore,
    target: i64,
    from: i64,
    id: &str,
    new_id: &str,
    moved: bool,
) -> Result<ProfileRelocation, String> {
    cli_relocate_server_profile(
        store,
        from,
        target,
        id,
        new_id,
        Some(TARGET_PASSWORD),
        moved,
    )
}

#[test]
fn a_native_session_keeps_to_its_account() {
    if !in_isolated_child("a_native_session_keeps_to_its_account") {
        return;
    }
    let accounts = Accounts::open();
    let (store, conn, legacy, source) = (
        &accounts.store,
        &accounts.conn,
        accounts.legacy,
        &accounts.source,
    );
    let (owner, owners_owner) = (accounts.owner(), accounts.owners_owner());
    let add_profile = |user: i64, id: &str| accounts.add_profile(user, id);
    let seed = |id: &str, uid: &str| accounts.seed(id, uid);
    let target_row = |id: &str| accounts.target_row(id);
    let in_vault = |id: &str| accounts.in_vault(id);
    let target = accounts.target.id;
    let relocate = |from: i64, id: &str, new_id: &str, moved: bool| {
        relocation(store, target, from, id, new_id, moved).unwrap()
    };

    // An account other than the legacy owner saves its session in its own
    // partition alone; the legacy owner keeps the vault copy too.
    // The vault is looked at first: the next read of the account runs its
    // one-time alignment, which would clear a vault copy written by mistake.
    seed("dup-a", "shared-uid");
    assert!(!in_vault("dup-a"));
    assert_eq!(loaded(owner, "dup-a").as_deref(), Some("r1"));
    add_profile(legacy, "owners-profile");
    vault::store_session(
        owners_owner,
        Some("owners-profile"),
        &session("owners-uid", "r1"),
    )
    .unwrap();
    assert!(in_vault("owners-profile"));

    // A refresh reaches every copy of the session inside the account, and no
    // copy another account holds of the same UID.
    seed("dup-b", "shared-uid");
    set_user_credential_with_dek(
        conn,
        accounts.target.id,
        &accounts.target_dek,
        &key("target-copy"),
        "proton_native_session",
        &raw(&session("shared-uid", "r1")),
    )
    .unwrap();
    vault::VaultSession::new(owner, Some("dup-a"), &session("shared-uid", "r1"))
        .save(&session("shared-uid", "r2").tokens);
    assert_eq!(loaded(owner, "dup-a").as_deref(), Some("r2"));
    assert_eq!(loaded(owner, "dup-b").as_deref(), Some("r2"));
    assert_eq!(target_row("target-copy").as_deref(), Some("r1"));
    assert!(!in_vault("dup-a") && !in_vault("dup-b"));
    // A vault key of the same name is the legacy owner's: never adopted, even
    // for the same UID, and never removed by this account's sign-out.
    store
        .store(&key("dup-a"), &raw(&session("shared-uid", "r9")))
        .unwrap();
    store
        .store(&key("dup-b"), &raw(&session("owners-other-uid", "r1")))
        .unwrap();
    assert_eq!(loaded(owner, "dup-a").as_deref(), Some("r2"));

    // A duplicate inside the account takes the pair its copies hold now.
    vault::restore_session_blocking(owner, "dup-c", &raw(&session("shared-uid", "r1"))).unwrap();
    assert_eq!(loaded(owner, "dup-c").as_deref(), Some("r2"));
    vault::delete_session(owner, Some("dup-c")).unwrap();

    // Rolling back a failed duplicate removes only its own copy: the source's
    // copy of the shared session stays, and the legacy owner's vault key of
    // the same name is not this account's to remove.
    vault::restore_session_blocking(owner, "dup-d", &raw(&session("shared-uid", "r1"))).unwrap();
    store
        .store(&key("dup-d"), &raw(&session("owners-other-uid", "r1")))
        .unwrap();
    vault::discard_session_blocking(owner, "dup-d").unwrap();
    assert_eq!(loaded(owner, "dup-d"), None);
    assert_eq!(loaded(owner, "dup-a").as_deref(), Some("r2"));
    assert!(in_vault("dup-d"));
    store.delete(&key("dup-d")).unwrap();

    // A rollback that cannot remove the partition copy reports it.
    vault::restore_session_blocking(
        owner,
        "discard-fail-copy",
        &raw(&session("discard-uid", "r1")),
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_partition_delete BEFORE DELETE ON user_credentials \
         BEGIN SELECT RAISE(ABORT, 'partition locked'); END;",
    )
    .unwrap();
    let refused = vault::discard_session_blocking(owner, "discard-fail-copy");
    conn.execute_batch("DROP TRIGGER refuse_partition_delete;")
        .unwrap();
    let error = refused.unwrap_err().to_string();
    assert!(error.contains("partition locked"), "{error}");
    vault::discard_session_blocking(owner, "discard-fail-copy").unwrap();
    assert_eq!(loaded(owner, "discard-fail-copy"), None);

    // A sign-out revokes on Proton only when no other profile of the same
    // account holds the session; another account's copy does not count. The
    // sign-out of a shared copy leaves the other profile signed in.
    // The revokes are counted, never sent: a test does not reach Proton.
    assert!(vault::another_profile_holds(store, owner, &key("dup-a"), "shared-uid").unwrap());
    let sign_out = |id: &str| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(vault::end_session(owner, Some(id)))
            .unwrap()
    };
    let revokes = vault::test_seams::revokes();
    sign_out("dup-b");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the sign-out of a shared copy sends no revoke"
    );
    assert_eq!(loaded(owner, "dup-b"), None);
    assert_eq!(loaded(owner, "dup-a").as_deref(), Some("r2"));
    assert!(in_vault("dup-b"));
    store.delete(&key("dup-a")).unwrap();
    store.delete(&key("dup-b")).unwrap();
    assert!(!vault::another_profile_holds(store, owner, &key("dup-a"), "shared-uid").unwrap());
    // The sign-out of a session no other profile holds revokes it, once no
    // other account may hold it: the locked target's row, saved without the
    // session index, could be that one.
    delete_user_credential_for(conn, accounts.target.id, &key("target-copy")).unwrap();
    seed("solo", "solo-uid");
    sign_out("solo");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes + 1,
        "the sign-out of a session no other profile holds revokes it"
    );
    assert_eq!(loaded(owner, "solo"), None);

    // A Copy into another account does not carry the session: the target is
    // told the profile asks to sign in again, and the source keeps it. The
    // legacy owner's vault session of the same name is not the source's, and
    // stays.
    let owners_raw = raw(&session("owners-copy-uid", "r1"));
    let vault_raw = |id: &str| store.get(&key(id)).ok();
    store.store(&key("copy-me"), &owners_raw).unwrap();
    store.store(&key("move-me"), &owners_raw).unwrap();
    seed("copy-me", "copy-uid");
    let copied = relocate(source.id, "copy-me", "copied", false);
    assert!(copied.inserted);
    assert_eq!(copied.secrets_not_copied, vec![key("copied")]);
    assert_eq!(target_row("copied"), None);
    assert_eq!(loaded(owner, "copy-me").as_deref(), Some("r1"));
    assert_eq!(vault_raw("copy-me").as_deref(), Some(owners_raw.as_str()));

    // A Move does not carry it either, and drops the source's copy locally.
    seed("move-me", "move-uid");
    let moved = relocate(source.id, "move-me", "moved", true);
    assert!(moved.moved && moved.inserted);
    assert_eq!(moved.secrets_not_copied, vec![key("moved")]);
    assert_eq!(target_row("moved"), None);
    assert_eq!(loaded(owner, "move-me"), None);
    assert_eq!(vault_raw("move-me").as_deref(), Some(owners_raw.as_str()));
    assert!(!in_vault("moved"));
    store.delete(&key("copy-me")).unwrap();
    store.delete(&key("move-me")).unwrap();

    // A Move waits for a refresh in flight, which holds the session's lease
    // until it has saved: the refreshed pair is saved before the drop, never
    // after it, where it would put the session back in the source account.
    seed("busy", "busy-uid");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lease = runtime
        .block_on(crate::providers::oauth2::RefreshLease::acquire_checked(
            &vault::refresh_lease_slug("busy-uid"),
        ))
        .unwrap()
        .unwrap();
    let waits = vault::test_seams::observe_waits();
    std::thread::scope(|scope| {
        let mover = scope.spawn(|| relocate(source.id, "busy", "busy-moved", true));
        let contended = format!("lease {}", vault::refresh_lease_slug("busy-uid"));
        while waits
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the Move waits for the refresh's lease")
            != contended
        {}
        assert_eq!(loaded(owner, "busy").as_deref(), Some("r1"));
        vault::VaultSession::new(owner, Some("busy"), &session("busy-uid", "r1"))
            .save(&session("busy-uid", "r2").tokens);
        assert_eq!(loaded(owner, "busy").as_deref(), Some("r2"));
        drop(lease);
        assert!(mover.join().unwrap().moved);
    });
    assert_eq!(loaded(owner, "busy"), None);
    assert!(!in_vault("busy"));

    // The legacy owner's Move also drops its vault copy, which no profile of
    // that account holds any more.
    let moved = relocate(legacy, "owners-profile", "owners-moved", true);
    assert_eq!(moved.secrets_not_copied, vec![key("owners-moved")]);
    assert!(!in_vault("owners-profile"));
    assert_eq!(loaded(owners_owner, "owners-profile"), None);
    assert_eq!(target_row("owners-moved"), None);

    // A profile without a native session reports nothing missing.
    add_profile(source.id, "no-session");
    assert!(relocate(source.id, "no-session", "no-session-copy", false)
        .secrets_not_copied
        .is_empty());
}

/// A row of the account that cannot be decrypted does not keep a refresh from
/// the other copies of the session: they are listed without decrypting any
/// row and read one by one, so only the unreadable one is left out.
#[test]
fn an_unreadable_row_does_not_stop_a_refresh_of_the_other_copies() {
    if !in_isolated_child("an_unreadable_row_does_not_stop_a_refresh_of_the_other_copies") {
        return;
    }
    let accounts = Accounts::open();
    let owner = accounts.owner();
    accounts.seed("unreadable-a", "unreadable-uid");
    accounts.seed("unreadable-b", "unreadable-uid");
    accounts.seed("unreadable-c", "unreadable-other-uid");
    accounts
        .conn
        .execute(
            "UPDATE user_credentials SET encrypted_blob = X'00'
             WHERE user_id = ?1 AND credential_id = ?2",
            params![accounts.source.id, key("unreadable-c")],
        )
        .unwrap();
    vault::VaultSession::new(
        owner,
        Some("unreadable-a"),
        &session("unreadable-uid", "r1"),
    )
    .save(&session("unreadable-uid", "r2").tokens);
    assert_eq!(loaded(owner, "unreadable-a").as_deref(), Some("r2"));
    assert_eq!(
        loaded(owner, "unreadable-b").as_deref(),
        Some("r2"),
        "a copy the refresh can read takes the rotated pair"
    );
}

/// A refresh of the legacy owner's session writes the legacy owner's copies
/// (its vault key and its row) and never the row another account holds of
/// the same profile id and session: a matching session does not make the
/// row the legacy owner's.
#[test]
fn a_refresh_of_the_legacy_owner_stays_in_its_account() {
    if !in_isolated_child("a_refresh_of_the_legacy_owner_stays_in_its_account") {
        return;
    }
    let accounts = Accounts::open();
    let (owner, owners_owner) = (accounts.owner(), accounts.owners_owner());
    // The source's one-time alignment runs at its first read, before it holds
    // a copy, so the copy below stays.
    assert_eq!(loaded(owner, "owners-shared"), None);
    accounts.add_profile(accounts.legacy, "owners-shared");
    vault::store_session(
        owners_owner,
        Some("owners-shared"),
        &session("owners-shared-uid", "r1"),
    )
    .unwrap();
    accounts.add_profile(accounts.source.id, "owners-shared");
    set_user_credential_for(
        &accounts.conn,
        &accounts.root_key,
        accounts.source.id,
        &key("owners-shared"),
        "proton_native_session",
        &raw(&session("owners-shared-uid", "r1")),
    )
    .unwrap();
    assert_eq!(loaded(owner, "owners-shared").as_deref(), Some("r1"));
    vault::VaultSession::new(
        owners_owner,
        Some("owners-shared"),
        &session("owners-shared-uid", "r1"),
    )
    .save(&session("owners-shared-uid", "r2").tokens);
    // Each copy read as it is stored: a load adopts the vault's pair for the
    // same UID, so it would show r2 over a legacy row left at r1.
    let (vault_copy, legacy_row, source_row) = accounts.copies("owners-shared");
    assert_eq!(vault_copy.as_deref(), Some("r2"), "the vault key");
    assert_eq!(
        legacy_row.as_deref(),
        Some("r2"),
        "the legacy owner's row takes the refreshed pair"
    );
    assert_eq!(
        source_row.as_deref(),
        Some("r1"),
        "the legacy owner's refresh does not write another account's row"
    );
}

/// An import brings a native Proton session into an account only when no
/// other account on this device may hold it: a copy another account holds,
/// in the vault or in its rows, or a saved session of an account this process
/// cannot read, keeps it out, and the profile is reported to sign in again.
/// Its sign-out then sends no revoke, and the account that holds the session
/// stays signed in.
#[test]
fn an_import_brings_no_session_another_account_may_hold() {
    if !in_isolated_child("an_import_brings_no_session_another_account_may_hold") {
        return;
    }
    let accounts = Accounts::open();
    let (owner, owners_owner) = (accounts.owner(), accounts.owners_owner());
    let (legacy, source, target) = (accounts.legacy, accounts.source.id, accounts.target.id);
    // Whether the import of profile `id` with session `uid` into `user`
    // reported the profile, which then signs in again.
    let import = |user: i64, id: &str, uid: &str| {
        let server = crate::profile_export::ServerProfileExport {
            id: id.into(),
            name: id.into(),
            protocol: Some("proton".into()),
            options: Some(json!({"proton_mode":"native"})),
            ..Default::default()
        };
        let secrets = HashMap::from([(
            id.to_string(),
            crate::profile_export::ProviderSecrets {
                proton_native_session: Some(raw(&session(uid, "r1"))),
                ..Default::default()
            },
        )]);
        crate::restore_shared_import_secrets(
            &accounts.store,
            user,
            &[server],
            &secrets,
            &HashMap::new(),
        )
        .failed
        .contains(id)
    };

    // The legacy owner holds the session, and the same export is imported
    // into the source.
    accounts.add_profile(legacy, "imported");
    vault::store_session(
        owners_owner,
        Some("imported"),
        &session("imported-uid", "r1"),
    )
    .unwrap();
    accounts.add_profile(source, "imported");
    let revokes = vault::test_seams::revokes();
    assert!(
        import(source, "imported", "imported-uid"),
        "the profile is reported to sign in again"
    );
    assert_eq!(
        loaded(owner, "imported"),
        None,
        "an import does not bring a session another account holds"
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(vault::end_session(owner, Some("imported")))
        .unwrap();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the sign-out of the importing account sends no revoke"
    );
    assert_eq!(loaded(owners_owner, "imported").as_deref(), Some("r1"));

    // Nor the other way: the legacy owner does not import a session the
    // source holds.
    accounts.seed("source-held", "source-held-uid");
    accounts.add_profile(legacy, "source-held-copy");
    assert!(import(legacy, "source-held-copy", "source-held-uid"));
    assert_eq!(loaded(owners_owner, "source-held-copy"), None);
    assert_eq!(loaded(owner, "source-held").as_deref(), Some("r1"));

    // A locked account with a saved session may hold any session: while it
    // does, no session is imported.
    set_user_credential_with_dek(
        &accounts.conn,
        target,
        &accounts.target_dek,
        &key("target-held"),
        "proton_native_session",
        &raw(&session("target-held-uid", "r1")),
    )
    .unwrap();
    accounts.add_profile(source, "fresh");
    assert!(import(source, "fresh", "fresh-uid"));
    assert_eq!(loaded(owner, "fresh"), None);

    // No other account may hold it: the session is imported.
    delete_user_credential_for(&accounts.conn, target, &key("target-held")).unwrap();
    assert!(!import(source, "fresh", "fresh-uid"));
    assert_eq!(loaded(owner, "fresh").as_deref(), Some("r1"));
}

/// An rclone import brings the session of a `protondrive` remote into the
/// active account under the rule of an `.aeroftp` import: when the legacy
/// owner already holds it, the import into another account leaves it out and
/// reports the profile to sign in again, whose sign-out then sends no revoke,
/// and the legacy owner stays signed in.
#[test]
fn an_rclone_import_brings_no_session_another_account_holds() {
    if !in_isolated_child("an_rclone_import_brings_no_session_another_account_holds") {
        return;
    }
    let accounts = Accounts::open();
    let (owner, owners_owner) = (accounts.owner(), accounts.owners_owner());
    let conf = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        conf.path(),
        "[pd]\ntype = protondrive\nusername = someone@example.test\n\
         client_uid = rclone-uid\nclient_access_token = access-1\n\
         client_refresh_token = refresh-1\nclient_salted_key_pass = a2V5LXBhc3N3b3JkLTE=\n",
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // The imported profile's id, and the reasons the import gave for it.
    let import = || {
        let imported = runtime
            .block_on(crate::bridge_commands::import_bridge_config(
                "rclone".into(),
                conf.path().to_string_lossy().into_owned(),
                None,
            ))
            .unwrap();
        let reasons: Vec<String> = imported["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|warning| warning["reason"].as_str().map(str::to_string))
            .collect();
        (
            imported["servers"][0]["id"].as_str().unwrap().to_string(),
            reasons,
        )
    };
    let uid_of = |owner: vault::SessionOwner, id: &str| {
        vault::load_session(owner, Some(id))
            .unwrap()
            .map(|session| session.tokens.uid)
    };

    // The legacy owner, the active account, imports the remote, and is told
    // its session is now shared with rclone.
    let (owners_id, reasons) = import();
    assert_eq!(
        uid_of(owners_owner, &owners_id).as_deref(),
        Some("rclone-uid")
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason == crate::rclone_import::RCLONE_SESSION_SHARED),
        "a stored session is reported shared with rclone: {reasons:?}"
    );

    // The same file, imported into the source once it is the active account,
    // unlocked as the window holds it.
    cli_set_active_user(&accounts.store, accounts.source.id).unwrap();
    accounts.unlock_source();
    let (id, reasons) = import();
    assert!(
        reasons
            .iter()
            .any(|reason| reason == vault::IMPORT_WITHHELD),
        "the profile is reported to sign in again: {reasons:?}"
    );
    assert!(
        !reasons
            .iter()
            .any(|reason| reason.contains("now shared with rclone")),
        "a session left out is not reported imported: {reasons:?}"
    );
    assert_eq!(
        uid_of(owner, &id),
        None,
        "an rclone import does not bring a session another account holds"
    );
    let revokes = vault::test_seams::revokes();
    runtime
        .block_on(vault::end_session(owner, Some(&id)))
        .unwrap();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the sign-out of the importing account sends no revoke"
    );
    assert_eq!(
        uid_of(owners_owner, &owners_id).as_deref(),
        Some("rclone-uid")
    );
}

/// A sign-out of `id` saved for `owner`, on a runtime of its own.
fn sign_out(owner: vault::SessionOwner, id: &str) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(vault::end_session(owner, Some(id)))
        .unwrap()
}

/// A sign-out revokes on Proton a session no other account holds, read from
/// the session index: a locked account's session of its own, indexed when it
/// was saved, is told apart without its key.
#[test]
fn a_sign_out_revokes_a_session_no_other_account_holds() {
    if !in_isolated_child("a_sign_out_revokes_a_session_no_other_account_holds") {
        return;
    }
    let accounts = Accounts::open();
    let owner = accounts.owner();
    accounts.unlock_target();
    vault::store_session(
        accounts.target_owner(),
        Some("target-own"),
        &session("target-uid", "r1"),
    )
    .unwrap();
    accounts.unlock_source();
    accounts.seed("alone", "alone-uid");
    let revokes = vault::test_seams::revokes();
    sign_out(owner, "alone");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes + 1,
        "the sign-out of a session no other account holds revokes it"
    );
    assert_eq!(loaded(owner, "alone"), None);
    assert_eq!(accounts.target_row("target-own").as_deref(), Some("r1"));
}

/// A sign-out sends no revoke for a session another account holds, in either
/// direction between the legacy owner and another account, and while the
/// account that holds it is locked: the copy here is dropped, and the other
/// account stays signed in.
#[test]
fn a_sign_out_sends_no_revoke_for_a_session_another_account_holds() {
    if !in_isolated_child("a_sign_out_sends_no_revoke_for_a_session_another_account_holds") {
        return;
    }
    let accounts = Accounts::open();
    let (owner, owners_owner) = (accounts.owner(), accounts.owners_owner());
    let revokes = vault::test_seams::revokes();

    accounts.add_profile(accounts.legacy, "owners-copy");
    vault::store_session(owners_owner, Some("owners-copy"), &session("two-uid", "r1")).unwrap();
    accounts.seed("source-copy", "two-uid");
    sign_out(owner, "source-copy");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the sign-out of a session the legacy owner holds sends no revoke"
    );
    assert_eq!(loaded(owner, "source-copy"), None);
    assert_eq!(loaded(owners_owner, "owners-copy").as_deref(), Some("r1"));

    accounts.seed("source-copy", "two-uid");
    sign_out(owners_owner, "owners-copy");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the legacy owner's sign-out of a session another account holds sends no revoke"
    );
    assert_eq!(loaded(owners_owner, "owners-copy"), None);
    assert_eq!(loaded(owner, "source-copy").as_deref(), Some("r1"));

    accounts.unlock_target();
    vault::store_session(
        accounts.target_owner(),
        Some("target-copy"),
        &session("shared-uid", "r1"),
    )
    .unwrap();
    accounts.unlock_source();
    accounts.seed("shared", "shared-uid");
    sign_out(owner, "shared");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the sign-out of a session a locked account holds sends no revoke"
    );
    assert_eq!(loaded(owner, "shared"), None);
    assert_eq!(accounts.target_row("target-copy").as_deref(), Some("r1"));
}

/// A session row saved before the index keeps every other account's sign-out
/// from revoking, since it may hold the same session; the unlock of its
/// account indexes it, and the next sign-out of a session it does not hold
/// revokes.
#[test]
fn a_session_saved_before_the_index_counts_until_its_account_is_unlocked() {
    if !in_isolated_child("a_session_saved_before_the_index_counts_until_its_account_is_unlocked") {
        return;
    }
    let accounts = Accounts::open();
    let owner = accounts.owner();
    set_user_credential_with_dek(
        &accounts.conn,
        accounts.target.id,
        &accounts.target_dek,
        &key("target-before-index"),
        "proton_native_session",
        &raw(&session("target-before-index-uid", "r1")),
    )
    .unwrap();
    accounts.seed("first", "first-uid");
    let revokes = vault::test_seams::revokes();
    sign_out(owner, "first");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "a session the index does not describe may be this one: no revoke"
    );
    assert_eq!(loaded(owner, "first"), None);

    accounts.unlock_target();
    accounts.unlock_source();
    accounts.seed("second", "second-uid");
    sign_out(owner, "second");
    assert_eq!(
        vault::test_seams::revokes(),
        revokes + 1,
        "once its account was unlocked, the row is indexed and the sign-out revokes"
    );
    assert_eq!(
        accounts.target_row("target-before-index").as_deref(),
        Some("r1")
    );
}

/// The session index keeps a keyed fingerprint of a session UID and never
/// the UID: neither in the clear nor as a hash anyone could recompute.
#[test]
fn the_session_index_keeps_no_session_uid() {
    if !in_isolated_child("the_session_index_keeps_no_session_uid") {
        return;
    }
    let accounts = Accounts::open();
    let uid = "index-plain-uid";
    accounts.seed("indexed", uid);
    let entries: Vec<Vec<String>> = accounts
        .conn
        .prepare("SELECT * FROM proton_session_index")
        .and_then(|mut statement| {
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| {
                            row.get::<_, rusqlite::types::Value>(column)
                                .map(|value| format!("{value:?}"))
                        })
                        .collect::<rusqlite::Result<Vec<String>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    assert_eq!(
        entries.len(),
        1,
        "the saved session is indexed: {entries:?}"
    );
    let unkeyed = hex::encode(Sha256::digest(uid.as_bytes()));
    for value in entries.iter().flatten() {
        assert!(!value.contains(uid), "the UID is in the index: {entries:?}");
        assert!(
            !value.contains(&unkeyed),
            "an unkeyed hash of the UID is in the index: {entries:?}"
        );
    }
    assert!(
        entries[0]
            .iter()
            .any(|value| value.contains(&key("indexed"))),
        "the entry names its row: {entries:?}"
    );
}

/// The purge of profile `id` of the active account, as the window runs it.
fn purge(id: &str, protocol: &str) -> Result<crate::PurgedProfileSecrets, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(crate::purge_profile_secrets(
            id.into(),
            Some(protocol.into()),
        ))
}

/// How many sessions purges left to an account's next unlock.
fn deferred_revokes(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM global_state WHERE key LIKE 'proton_deferred_revoke_%'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// Whether `user` has a row under `key`, read without its key.
fn has_row(conn: &Connection, user: i64, key: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM user_credentials WHERE user_id = ?1 AND credential_id = ?2",
        params![user, key],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

/// The purge of a profile with no native Proton session, in a locked active
/// account, never goes through Proton's session code, which cannot read a
/// locked account: its secrets go, and nothing is revoked.
#[test]
fn a_locked_accounts_profile_without_a_proton_session_is_purged() {
    if !in_isolated_child("a_locked_accounts_profile_without_a_proton_session_is_purged") {
        return;
    }
    let accounts = Accounts::open();
    let target = accounts.target.id;
    let keys = ["oauth_dropbox_locked-dropbox", "server_locked-dropbox"];
    for key in keys {
        set_user_credential_with_dek(
            &accounts.conn,
            target,
            &accounts.target_dek,
            key,
            "oauth",
            "synthetic token",
        )
        .unwrap();
    }
    cli_set_active_user(&accounts.store, target).unwrap();
    let revokes = vault::test_seams::revokes();
    let purged = purge("locked-dropbox", "dropbox");
    assert!(
        purged.is_ok(),
        "a locked account's Dropbox profile is purged: {purged:?}"
    );
    assert_eq!(purged.unwrap().proton_session_ends_at_unlock_of, None);
    for key in keys {
        assert!(!has_row(&accounts.conn, target, key), "{key} is gone");
    }
    assert_eq!(vault::test_seams::revokes(), revokes);
}

/// The purge of a native Proton profile in a locked active account cannot
/// read the session to end it on Proton: it keeps the row, marked, removes
/// the rest, and says the session ends at the account's next unlock, which
/// revokes it and removes the row and the mark.
#[test]
fn a_locked_accounts_proton_session_ends_at_its_next_unlock() {
    if !in_isolated_child("a_locked_accounts_proton_session_ends_at_its_next_unlock") {
        return;
    }
    let accounts = Accounts::open();
    let target = accounts.target.id;
    accounts.unlock_target();
    vault::store_session(
        accounts.target_owner(),
        Some("locked-proton"),
        &session("locked-proton-uid", "r1"),
    )
    .unwrap();
    accounts.unlock_source();
    cli_set_active_user(&accounts.store, target).unwrap();
    let revokes = vault::test_seams::revokes();
    let purged = purge("locked-proton", "proton");
    assert!(
        purged.as_ref().is_ok_and(
            |purged| purged.proton_session_ends_at_unlock_of.as_deref() == Some("Target")
        ),
        "the purge says the session ends at the next unlock of its account: {purged:?}"
    );
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "nothing is revoked yet"
    );
    assert_eq!(accounts.target_row("locked-proton").as_deref(), Some("r1"));
    assert_eq!(deferred_revokes(&accounts.conn), 1);

    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes + 1,
        "the unlock ends the session on Proton"
    );
    assert!(!has_row(&accounts.conn, target, &key("locked-proton")));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// As above, for a session another account also holds: the unlock removes
/// the locked account's copy and its mark without a revoke, and the other
/// account stays signed in.
#[test]
fn a_locked_accounts_shared_proton_session_is_not_revoked_at_its_unlock() {
    if !in_isolated_child("a_locked_accounts_shared_proton_session_is_not_revoked_at_its_unlock") {
        return;
    }
    let accounts = Accounts::open();
    let target = accounts.target.id;
    accounts.seed("source-shared", "purge-shared-uid");
    accounts.unlock_target();
    vault::store_session(
        accounts.target_owner(),
        Some("locked-shared"),
        &session("purge-shared-uid", "r1"),
    )
    .unwrap();
    accounts.unlock_source();
    cli_set_active_user(&accounts.store, target).unwrap();
    let revokes = vault::test_seams::revokes();
    let purged = purge("locked-shared", "proton");
    assert!(
        purged
            .as_ref()
            .is_ok_and(|purged| purged.proton_session_ends_at_unlock_of.is_some()),
        "the purge leaves the session to the next unlock: {purged:?}"
    );
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "a session another account holds is not revoked at the unlock"
    );
    assert!(!has_row(&accounts.conn, target, &key("locked-shared")));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
    accounts.unlock_source();
    assert_eq!(
        loaded(accounts.owner(), "source-shared").as_deref(),
        Some("r1")
    );
}

/// The target open in another process: its key at hand there, without this
/// process's unlock, so nothing a purge left to the unlock runs.
fn open_target_elsewhere(accounts: &Accounts) {
    let root_secret = user_crypto::secret_key_from_bytes(&accounts.root_key);
    let dek = resolve_user_dek_scoped(
        &accounts.conn,
        &root_secret,
        accounts.target.id,
        Some(TARGET_PASSWORD),
    )
    .unwrap();
    set_user_session(accounts.target.id, dek).unwrap();
}

/// The purge of the target's profile `id`, signed in with `uid`, while the
/// target is locked here: its session is left to the next unlock.
fn defer_target_session(accounts: &Accounts, id: &str, uid: &str) {
    accounts.unlock_target();
    vault::store_session(accounts.target_owner(), Some(id), &session(uid, "r1")).unwrap();
    accounts.unlock_source();
    cli_set_active_user(&accounts.store, accounts.target.id).unwrap();
    let purged = purge(id, "proton");
    assert!(
        purged
            .as_ref()
            .is_ok_and(|purged| purged.proton_session_ends_at_unlock_of.is_some()),
        "the purge leaves the session to the next unlock: {purged:?}"
    );
    assert_eq!(deferred_revokes(&accounts.conn), 1);
}

/// A sign-in that ends in another process, where the target is open, after
/// a purge here left the profile's session to the target's next unlock: the
/// unlock keeps the new session, revokes nothing, and the mark goes.
#[test]
fn a_sign_in_after_a_deferred_purge_keeps_its_session_at_the_unlock() {
    if !in_isolated_child("a_sign_in_after_a_deferred_purge_keeps_its_session_at_the_unlock") {
        return;
    }
    let accounts = Accounts::open();
    defer_target_session(&accounts, "signed-in-again", "purged-uid");
    open_target_elsewhere(&accounts);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(vault::replace_session(
            accounts.target_owner(),
            Some("signed-in-again"),
            &session("new-sign-in-uid", "n1"),
        ))
        .unwrap();
    clear_user_session();
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the unlock ends no session a sign-in saved after the purge"
    );
    assert_eq!(
        accounts.target_row("signed-in-again").as_deref(),
        Some("n1")
    );
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// As above, for a process that saves the new sign-in and leaves the mark
/// as it is: the mark names the session the purge left, and a session with
/// another UID is not that one.
#[test]
fn a_session_saved_over_a_deferred_purge_is_kept_at_the_unlock() {
    if !in_isolated_child("a_session_saved_over_a_deferred_purge_is_kept_at_the_unlock") {
        return;
    }
    let accounts = Accounts::open();
    defer_target_session(&accounts, "saved-over", "purged-uid");
    open_target_elsewhere(&accounts);
    vault::store_session(
        accounts.target_owner(),
        Some("saved-over"),
        &session("saved-over-uid", "n1"),
    )
    .unwrap();
    clear_user_session();
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the unlock ends no session other than the one the purge left"
    );
    assert_eq!(accounts.target_row("saved-over").as_deref(), Some("n1"));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// The session a purge left, refreshed in another process before the unlock:
/// a new ciphertext of the same session, which the unlock still ends.
#[test]
fn a_refreshed_deferred_session_still_ends_at_the_unlock() {
    if !in_isolated_child("a_refreshed_deferred_session_still_ends_at_the_unlock") {
        return;
    }
    let accounts = Accounts::open();
    defer_target_session(&accounts, "refreshed", "refreshed-uid");
    open_target_elsewhere(&accounts);
    vault::store_session(
        accounts.target_owner(),
        Some("refreshed"),
        &session("refreshed-uid", "r2"),
    )
    .unwrap();
    clear_user_session();
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes + 1,
        "a refresh keeps the session the purge left, which the unlock ends"
    );
    assert!(!has_row(
        &accounts.conn,
        accounts.target.id,
        &key("refreshed")
    ));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// A purged profile listed again by its account before the unlock (an
/// import of it, in another process): its session is its own again, and the
/// unlock keeps it.
#[test]
fn a_deferred_session_of_a_profile_listed_again_is_kept() {
    if !in_isolated_child("a_deferred_session_of_a_profile_listed_again_is_kept") {
        return;
    }
    let accounts = Accounts::open();
    defer_target_session(&accounts, "listed-again", "listed-again-uid");
    open_target_elsewhere(&accounts);
    accounts.add_profile(accounts.target.id, "listed-again");
    clear_user_session();
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the session of a profile the account lists again is not ended"
    );
    assert_eq!(accounts.target_row("listed-again").as_deref(), Some("r1"));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// The target's profile `id` signed in with `uid` by a writer that left it
/// out of the session index, then purged while the target is locked here:
/// the mark carries no fingerprint.
fn defer_unindexed_target_session(accounts: &Accounts, id: &str, uid: &str) {
    set_user_credential_with_dek(
        &accounts.conn,
        accounts.target.id,
        &accounts.target_dek,
        &key(id),
        "proton_native_session",
        &raw(&session(uid, "r1")),
    )
    .unwrap();
    cli_set_active_user(&accounts.store, accounts.target.id).unwrap();
    let purged = purge(id, "proton");
    assert!(
        purged
            .as_ref()
            .is_ok_and(|purged| purged.proton_session_ends_at_unlock_of.is_some()),
        "the purge leaves the session to the next unlock: {purged:?}"
    );
    assert_eq!(deferred_revokes(&accounts.conn), 1);
}

/// A purge of a session the index did not describe, in a locked account:
/// the unlock indexes it, ends it on Proton as the index decides (no other
/// account holds it), and leaves neither the row nor the mark.
#[test]
fn an_unindexed_deferred_session_ends_at_the_unlock() {
    if !in_isolated_child("an_unindexed_deferred_session_ends_at_the_unlock") {
        return;
    }
    let accounts = Accounts::open();
    defer_unindexed_target_session(&accounts, "unindexed", "unindexed-uid");
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(vault::test_seams::revokes(), revokes + 1);
    assert!(!has_row(
        &accounts.conn,
        accounts.target.id,
        &key("unindexed")
    ));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// As above, for a session another account holds: no revoke, and still no
/// row or mark left.
#[test]
fn an_unindexed_shared_deferred_session_is_removed_without_a_revoke() {
    if !in_isolated_child("an_unindexed_shared_deferred_session_is_removed_without_a_revoke") {
        return;
    }
    let accounts = Accounts::open();
    accounts.seed("source-copy", "unindexed-shared-uid");
    defer_unindexed_target_session(&accounts, "unindexed-shared", "unindexed-shared-uid");
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "a session another account holds is not revoked"
    );
    assert!(!has_row(
        &accounts.conn,
        accounts.target.id,
        &key("unindexed-shared")
    ));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
    accounts.unlock_source();
    assert_eq!(
        loaded(accounts.owner(), "source-copy").as_deref(),
        Some("r1")
    );
}

/// A session the index did not describe, purged, then refreshed in another
/// process into another ciphertext before the unlock: still the purged
/// session, which the unlock ends, leaving no row behind.
#[test]
fn an_unindexed_deferred_session_refreshed_since_ends_at_the_unlock() {
    if !in_isolated_child("an_unindexed_deferred_session_refreshed_since_ends_at_the_unlock") {
        return;
    }
    let accounts = Accounts::open();
    defer_unindexed_target_session(&accounts, "unindexed-refreshed", "unindexed-refreshed-uid");
    set_user_credential_with_dek(
        &accounts.conn,
        accounts.target.id,
        &accounts.target_dek,
        &key("unindexed-refreshed"),
        "proton_native_session",
        &raw(&session("unindexed-refreshed-uid", "r2")),
    )
    .unwrap();
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(vault::test_seams::revokes(), revokes + 1);
    assert!(!has_row(
        &accounts.conn,
        accounts.target.id,
        &key("unindexed-refreshed")
    ));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// An unlock whose cleanup a refresh's lease holds off keeps the mark; a
/// sign-in saved in another process before the next unlock is the
/// profile's own, and that unlock keeps it.
#[test]
fn a_sign_in_after_a_failed_deferred_cleanup_is_kept() {
    if !in_isolated_child("a_sign_in_after_a_failed_deferred_cleanup_is_kept") {
        return;
    }
    let accounts = Accounts::open();
    defer_unindexed_target_session(&accounts, "held-off", "held-off-uid");
    let lease = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(crate::providers::oauth2::RefreshLease::acquire_checked(
            &vault::refresh_lease_slug("held-off-uid"),
        ))
        .unwrap()
        .unwrap();
    vault::test_seams::set_exclusive_wait(Some(std::time::Duration::ZERO));
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    vault::test_seams::set_exclusive_wait(None);
    drop(lease);
    assert_eq!(vault::test_seams::revokes(), revokes);
    assert_eq!(accounts.target_row("held-off").as_deref(), Some("r1"));
    assert_eq!(deferred_revokes(&accounts.conn), 1, "the mark waits");

    clear_user_session();
    open_target_elsewhere(&accounts);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(vault::replace_session(
            accounts.target_owner(),
            Some("held-off"),
            &session("held-off-new-uid", "n1"),
        ))
        .unwrap();
    clear_user_session();
    let revokes = vault::test_seams::revokes();
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes,
        "the unlock ends no session a sign-in saved after the purge"
    );
    assert_eq!(accounts.target_row("held-off").as_deref(), Some("n1"));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// A refresh writes a copy of its session only while the copy still holds
/// it, and never creates one: the alignment drops an account's copy of a
/// session the legacy owner holds without the session's lease, and when it
/// does so between a refresh's look and its write (in another process), the
/// refresh does not bring the copy back.
#[test]
fn a_refresh_does_not_bring_back_a_copy_the_alignment_dropped() {
    if !in_isolated_child("a_refresh_does_not_bring_back_a_copy_the_alignment_dropped") {
        return;
    }
    let accounts = Accounts::open();
    let (owner, owners_owner) = (accounts.owner(), accounts.owners_owner());
    // The source's alignment has run, so the copy below is left for now.
    assert_eq!(loaded(owner, "aligned"), None);
    accounts.add_profile(accounts.legacy, "aligned");
    vault::store_session(owners_owner, Some("aligned"), &session("aligned-uid", "r1")).unwrap();
    accounts.add_profile(accounts.source.id, "aligned");
    set_user_credential_for(
        &accounts.conn,
        &accounts.root_key,
        accounts.source.id,
        &key("aligned"),
        "proton_native_session",
        &raw(&session("aligned-uid", "r1")),
    )
    .unwrap();
    assert_eq!(loaded(owner, "aligned").as_deref(), Some("r1"));
    let source = accounts.source.id;
    vault::test_seams::before_next_refresh_write(move |_| {
        let store = CredentialStore::from_cache().unwrap();
        let conn = open_or_init_cli().unwrap();
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![format!("{PRIVATE_TOKENS_ALIGNED_PREFIX}{source}")],
        )
        .unwrap();
        init_or_migrate_cli(&store).unwrap();
        assert!(private_tokens_aligned(&conn, source).unwrap());
        let root_key = store.derive_user_partition_wrapping_key();
        assert!(
            get_user_credential_for(&conn, &root_key, source, &key("aligned"))
                .unwrap()
                .is_none(),
            "the alignment dropped the copy"
        );
    });
    vault::VaultSession::new(owner, Some("aligned"), &session("aligned-uid", "r1"))
        .save(&session("aligned-uid", "r2").tokens);
    assert_eq!(
        loaded(owner, "aligned"),
        None,
        "a refresh does not bring back a copy the alignment dropped"
    );
    // The legacy owner's copies read as they are stored, since a load would
    // adopt the vault's pair over its row.
    let (vault_copy, legacy_row, _) = accounts.copies("aligned");
    assert_eq!(vault_copy.as_deref(), Some("r1"));
    assert_eq!(
        legacy_row.as_deref(),
        Some("r1"),
        "the source's refresh writes no copy of the legacy owner"
    );
}

/// A Move holds the source's session key from before it looks whether the
/// source holds a session until it has removed it: a sign-in of the profile
/// that ends meanwhile waits for the Move, instead of saving a session the
/// Move has looked past. Once the Move is done, the sign-in saves nothing in
/// the account the profile left, while a sign-in of a profile not saved yet,
/// or of the moved one saved there again, still saves.
#[test]
fn a_sign_in_waits_for_the_move_of_its_profile() {
    if !in_isolated_child("a_sign_in_waits_for_the_move_of_its_profile") {
        return;
    }
    let accounts = Accounts::open();
    let owner = accounts.owner();
    accounts.add_profile(accounts.source.id, "late-sign-in");
    let waits = vault::test_seams::observe_waits();
    let sign_in = std::rc::Rc::new(std::cell::RefCell::new(None));
    let waited = std::rc::Rc::new(std::cell::Cell::new(false));
    let (hook_sign_in, hook_waited) = (sign_in.clone(), waited.clone());
    RELOCATION_LOOKED.set(Some(Box::new(move || {
        let signing_in = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(vault::replace_session(
                    owner,
                    Some("late-sign-in"),
                    &session("late-uid", "r1"),
                ))
        });
        // The sign-in either waits for the Move's hold of its key, or ends.
        let contended = format!("key {}", key("late-sign-in"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !signing_in.is_finished() && std::time::Instant::now() < deadline {
            if waits
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_ok_and(|wait| wait == contended)
            {
                hook_waited.set(true);
                break;
            }
        }
        *hook_sign_in.borrow_mut() = Some(signing_in);
    })));
    let moved = accounts.relocate(accounts.source.id, "late-sign-in", "late-moved", true);
    assert!(moved.moved);
    assert!(
        waited.get(),
        "a sign-in that ends during the Move of its profile waits for the Move"
    );
    let signed_in = sign_in.borrow_mut().take().unwrap().join().unwrap();
    assert_eq!(
        loaded(owner, "late-sign-in"),
        None,
        "a sign-in that ends after the Move saves no session in the account the profile left"
    );
    let refused = signed_in.unwrap_err().to_string();
    assert!(refused.contains("moved to another account"), "{refused}");

    let sign_in = |id: &str| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(vault::replace_session(
                owner,
                Some(id),
                &session(&format!("{id}-uid"), "r1"),
            ))
    };
    sign_in("not-saved-yet").unwrap();
    assert_eq!(loaded(owner, "not-saved-yet").as_deref(), Some("r1"));
    accounts.add_profile(accounts.source.id, "late-sign-in");
    sign_in("late-sign-in").unwrap();
    assert_eq!(loaded(owner, "late-sign-in").as_deref(), Some("r1"));
}

/// A Move takes the source session's refresh lease before it commits: while a
/// refresh holds the lease past the wait, the Move stops with both profile
/// lists and the session as they were, and it goes ahead once the lease is
/// free.
#[test]
fn a_move_the_lease_holds_off_changes_nothing() {
    if !in_isolated_child("a_move_the_lease_holds_off_changes_nothing") {
        return;
    }
    let accounts = Accounts::open();
    let owner = accounts.owner();
    let (source, target) = (accounts.source.id, accounts.target.id);
    accounts.seed("held", "held-uid");
    let lease = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(crate::providers::oauth2::RefreshLease::acquire_checked(
            &vault::refresh_lease_slug("held-uid"),
        ))
        .unwrap()
        .unwrap();
    vault::test_seams::set_exclusive_wait(Some(std::time::Duration::ZERO));
    let refused = relocation(&accounts.store, target, source, "held", "held-moved", true);
    vault::test_seams::set_exclusive_wait(None);
    assert!(refused.is_err(), "the Move stops: {refused:?}");
    assert!(
        accounts.profile_ids(source).contains(&"held".to_string()),
        "a Move the lease holds off leaves the source's profile in place"
    );
    assert!(!accounts
        .profile_ids(target)
        .contains(&"held-moved".to_string()));
    assert_eq!(loaded(owner, "held").as_deref(), Some("r1"));
    drop(lease);
    assert!(accounts.relocate(source, "held", "held-moved", true).moved);
    assert_eq!(loaded(owner, "held"), None);
    assert!(accounts
        .profile_ids(target)
        .contains(&"held-moved".to_string()));
}

/// A lease storage that fails (its directory is a regular file) is no
/// contention: a change of the session stops at once with that cause, never
/// after the whole wait as a refresh that "did not let go", and a refresh
/// goes ahead without the lease at once instead of spinning for the wait.
#[test]
fn a_lease_storage_that_fails_stops_a_change_at_once() {
    if !in_isolated_child("a_lease_storage_that_fails_stops_a_change_at_once") {
        return;
    }
    let accounts = Accounts::open();
    let owner = accounts.owner();
    accounts.seed("lease-storage", "lease-storage-uid");
    let tokens = crate::portable::aeroftp_data_root()
        .unwrap()
        .join("oauth_tokens");
    if tokens.is_dir() {
        std::fs::remove_dir(&tokens).unwrap();
    }
    std::fs::write(&tokens, b"").unwrap();
    vault::test_seams::set_exclusive_wait(Some(std::time::Duration::from_secs(5)));
    let started = std::time::Instant::now();
    let held = vault::hold_session_blocking(owner, "lease-storage");
    let held_for = started.elapsed();
    let started = std::time::Instant::now();
    let refresh_lease = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(
            vault::VaultSession::new(
                owner,
                Some("lease-storage"),
                &session("lease-storage-uid", "r1"),
            )
            .lock(),
        );
    let refresh_waited = started.elapsed();
    vault::test_seams::set_exclusive_wait(None);
    let refused = held.err().expect("the hold stops").to_string();
    assert!(
        refused.contains("lease storage unavailable"),
        "the hold names the lease storage: {refused}"
    );
    assert!(
        held_for < std::time::Duration::from_secs(1),
        "the hold stops at once: {held_for:?}"
    );
    assert!(refresh_lease.is_none());
    assert!(
        refresh_waited < std::time::Duration::from_secs(1),
        "the refresh does not wait for a lease it cannot take: {refresh_waited:?}"
    );
    assert_eq!(loaded(owner, "lease-storage").as_deref(), Some("r1"));
}

/// An unlock whose sign-out on Proton fails (offline) ends nothing locally:
/// the purged session's row and its mark stay, and the next unlock tries
/// again, ends the session on Proton and removes both.
#[test]
fn a_deferred_revoke_that_fails_is_tried_again_at_the_next_unlock() {
    if !in_isolated_child("a_deferred_revoke_that_fails_is_tried_again_at_the_next_unlock") {
        return;
    }
    let accounts = Accounts::open();
    defer_target_session(&accounts, "offline", "offline-uid");
    let (revokes, failed) = (
        vault::test_seams::revokes(),
        vault::test_seams::failed_revokes(),
    );
    vault::test_seams::fail_revokes(true);
    accounts.unlock_target();
    vault::test_seams::fail_revokes(false);
    assert_eq!(
        vault::test_seams::failed_revokes(),
        failed + 1,
        "the unlock tried to end the session"
    );
    assert_eq!(vault::test_seams::revokes(), revokes);
    assert_eq!(
        accounts.target_row("offline").as_deref(),
        Some("r1"),
        "a session whose sign-out failed keeps its row"
    );
    assert_eq!(
        deferred_revokes(&accounts.conn),
        1,
        "a session whose sign-out failed keeps its mark"
    );

    accounts.unlock_source();
    accounts.unlock_target();
    assert_eq!(
        vault::test_seams::revokes(),
        revokes + 1,
        "the next unlock ends the session on Proton"
    );
    assert!(!has_row(
        &accounts.conn,
        accounts.target.id,
        &key("offline")
    ));
    assert_eq!(deferred_revokes(&accounts.conn), 0);
}

/// A Move takes a Proton session lock only for a profile that may carry a
/// native Proton session. On a filesystem without file locks, where the
/// profile list goes on without its lock, an SFTP Move goes through; the Move
/// of a Proton profile with no session yet still takes the lock (a sign-in of
/// it may be open), so it is refused with the cause named and both lists as
/// they were.
#[test]
fn only_a_proton_move_takes_a_proton_session_lock() {
    if !in_isolated_child("only_a_proton_move_takes_a_proton_session_lock") {
        return;
    }
    let accounts = Accounts::open();
    let (source, target) = (accounts.source.id, accounts.target.id);
    let mut db = open_or_init_cli().unwrap();
    let mut profiles = list_server_profiles_for(&db, &accounts.root_key, source).unwrap();
    profiles.push(
        json!({"id":"plain-sftp","name":"plain-sftp","protocol":"sftp",
        "host":"sftp.example.test","port":22,"username":"someone"}),
    );
    replace_server_profiles_for(&mut db, &accounts.root_key, source, &profiles).unwrap();
    accounts.add_profile(source, "unsigned-proton");
    NO_FILE_LOCKS_IN_TEST.set(true);
    vault::test_seams::set_no_file_locks(true);
    let sftp = relocation(
        &accounts.store,
        target,
        source,
        "plain-sftp",
        "plain-sftp-moved",
        true,
    );
    let proton = relocation(
        &accounts.store,
        target,
        source,
        "unsigned-proton",
        "unsigned-proton-moved",
        true,
    );
    vault::test_seams::set_no_file_locks(false);
    NO_FILE_LOCKS_IN_TEST.set(false);
    assert!(
        sftp.as_ref().is_ok_and(|moved| moved.moved),
        "an SFTP Move takes no Proton session lock: {sftp:?}"
    );
    assert!(
        proton
            .as_ref()
            .is_err_and(|refused| refused.contains(NO_FILE_LOCKS)),
        "a Proton Move without file locks is refused with the cause: {proton:?}"
    );
    assert!(accounts
        .profile_ids(source)
        .contains(&"unsigned-proton".to_string()));
    assert!(!accounts
        .profile_ids(target)
        .contains(&"unsigned-proton-moved".to_string()));
}

/// An rclone import writes every secret of one scan into the account active
/// when it began. Another account becomes active while the first session's
/// restore waits for its lease: neither the second session nor the OAuth
/// token after it goes there, and the response names the account the secrets
/// went to, for the window to refuse a confirm in another.
#[test]
fn an_rclone_import_keeps_to_the_account_it_began_in() {
    if !in_isolated_child("an_rclone_import_keeps_to_the_account_it_began_in") {
        return;
    }
    let accounts = Accounts::open();
    let owners_owner = accounts.owners_owner();
    let conf = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        conf.path(),
        concat!(
            "[pd-first]\ntype = protondrive\nusername = first@example.test\n",
            "client_uid = pinned-first-uid\nclient_access_token = access-1\n",
            "client_refresh_token = refresh-1\nclient_salted_key_pass = a2V5LXBhc3N3b3JkLTE=\n",
            "[pd-second]\ntype = protondrive\nusername = second@example.test\n",
            "client_uid = pinned-second-uid\nclient_access_token = access-1\n",
            "client_refresh_token = refresh-1\nclient_salted_key_pass = a2V5LXBhc3N3b3JkLTE=\n",
            "[box]\ntype = dropbox\n",
            "token = {\"access_token\":\"box-access\",\"token_type\":\"bearer\",\"refresh_token\":\"box-refresh\"}\n",
        ),
    )
    .unwrap();
    let first_slug = vault::refresh_lease_slug("pinned-first-uid");
    let lease = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(crate::providers::oauth2::RefreshLease::acquire_checked(
            &first_slug,
        ))
        .unwrap()
        .unwrap();
    let waits = vault::test_seams::observe_waits();
    let path = conf.path().to_string_lossy().into_owned();
    let importing = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(crate::bridge_commands::import_bridge_config(
                "rclone".into(),
                path,
                None,
            ))
    });
    let contended = format!("lease {first_slug}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut waited = false;
    while !importing.is_finished() && std::time::Instant::now() < deadline {
        if waits
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_ok_and(|wait| wait == contended)
        {
            waited = true;
            break;
        }
    }
    assert!(waited, "the first session's restore waits for its lease");
    cli_set_active_user(&accounts.store, accounts.source.id).unwrap();
    drop(lease);
    let imported = importing.join().unwrap().unwrap();
    let id_of = |name: &str| {
        imported["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|server| server["name"] == name)
            .and_then(|server| server["id"].as_str())
            .unwrap()
            .to_string()
    };
    for name in ["pd-first", "pd-second"] {
        let id = id_of(name);
        assert!(
            vault::load_session(owners_owner, Some(&id))
                .unwrap()
                .is_some(),
            "{name}'s session is in the account the import began in"
        );
        assert!(
            !has_row(&accounts.conn, accounts.source.id, &key(&id)),
            "{name}'s session is not in the account switched to"
        );
    }
    let token = format!("oauth_dropbox_{}", id_of("box"));
    assert!(
        !has_row(&accounts.conn, accounts.source.id, &token),
        "the OAuth token is not in the account switched to"
    );
    assert!(has_row(&accounts.conn, accounts.legacy, &token));
    assert_eq!(imported["importedUserId"], json!(accounts.legacy));
}

/// A session save is whole or not made. For the legacy owner, whose session
/// is a vault key and its row, a row the account store refuses puts the
/// vault key back as it was: an import reported as not restored leaves no
/// session the profile would connect with, and a session saved before stays
/// the one in use. Without the refusal both halves take the import.
#[test]
fn a_session_the_account_store_refuses_leaves_the_vault_as_it_was() {
    if !in_isolated_child("a_session_the_account_store_refuses_leaves_the_vault_as_it_was") {
        return;
    }
    let accounts = Accounts::open();
    let owners_owner = accounts.owners_owner();
    let vault_copy = |id: &str| accounts.copies(id).0;
    let legacy_row = |id: &str| accounts.copies(id).1;

    accounts.add_profile(accounts.legacy, "refused-fresh");
    vault::test_seams::refuse_next_mirror();
    let fresh = vault::import_session_blocking(
        owners_owner,
        "refused-fresh",
        &raw(&session("refused-fresh-uid", "r1")),
    );
    assert!(fresh.is_err(), "the refused import is an error: {fresh:?}");
    assert_eq!(
        vault_copy("refused-fresh"),
        None,
        "the vault holds no session of an import that failed"
    );
    assert_eq!(loaded(owners_owner, "refused-fresh"), None);

    accounts.add_profile(accounts.legacy, "refused-over");
    vault::store_session(
        owners_owner,
        Some("refused-over"),
        &session("refused-previous-uid", "p1"),
    )
    .unwrap();
    vault::test_seams::refuse_next_mirror();
    let over = vault::import_session_blocking(
        owners_owner,
        "refused-over",
        &raw(&session("refused-imported-uid", "i1")),
    );
    assert!(over.is_err(), "the refused import is an error: {over:?}");
    assert_eq!(
        vault_copy("refused-over").as_deref(),
        Some("p1"),
        "the vault keeps the session saved before"
    );
    assert_eq!(legacy_row("refused-over").as_deref(), Some("p1"));
    assert_eq!(loaded(owners_owner, "refused-over").as_deref(), Some("p1"));

    let saved = vault::import_session_blocking(
        owners_owner,
        "refused-over",
        &raw(&session("refused-imported-uid", "i1")),
    );
    assert_eq!(saved.ok(), Some(true));
    assert_eq!(vault_copy("refused-over").as_deref(), Some("i1"));
    assert_eq!(legacy_row("refused-over").as_deref(), Some("i1"));
}

/// An import of a profile a Move took out of the account saves no session
/// there: the Move's mark refuses it under the session's key lock, as it
/// refuses a sign-in, so no orphan row is left in the account the profile
/// left.
#[test]
fn an_import_saves_no_session_for_a_profile_moved_away() {
    if !in_isolated_child("an_import_saves_no_session_for_a_profile_moved_away") {
        return;
    }
    let accounts = Accounts::open();
    let (owner, source) = (accounts.owner(), accounts.source.id);
    accounts.add_profile(source, "moved-then-imported");
    assert!(
        accounts
            .relocate(
                source,
                "moved-then-imported",
                "moved-then-imported-there",
                true
            )
            .moved
    );
    let imported = vault::import_session_blocking(
        owner,
        "moved-then-imported",
        &raw(&session("moved-then-imported-uid", "r1")),
    );
    assert!(
        imported
            .as_ref()
            .is_err_and(|refused| refused.to_string().contains("moved to another account")),
        "the import of a profile moved away is refused: {imported:?}"
    );
    assert!(!has_row(
        &accounts.conn,
        source,
        &key("moved-then-imported")
    ));
    assert_eq!(loaded(owner, "moved-then-imported"), None);
}

/// Every writer that saves a secret to the vault and then to the account's
/// row is whole or not at all: a row the account store refuses puts the vault
/// key back as it was, the previous value or no key, for the generic and the
/// typed dual writers, the import writer and a new OAuth authorization.
#[test]
fn a_secret_the_account_store_refuses_leaves_the_vault_as_it_was() {
    if !in_isolated_child("a_secret_the_account_store_refuses_leaves_the_vault_as_it_was") {
        return;
    }
    use std::sync::atomic::Ordering::SeqCst;
    let accounts = Accounts::open();
    let (store, legacy) = (&accounts.store, accounts.legacy);
    let refused = |write: &dyn Fn() -> Result<(), String>| {
        PARTITION_WRITE_REFUSED.store(true, SeqCst);
        let written = write();
        PARTITION_WRITE_REFUSED.store(false, SeqCst);
        written
    };
    let vault_value = |key: &str| store.get(key).ok();
    let mut failures = Vec::new();

    // The generic writer, over a value saved before.
    store_credential_dual_as(store, Some(legacy), "server_refused-generic", "before").unwrap();
    let written = refused(&|| {
        store_credential_dual_as(store, Some(legacy), "server_refused-generic", "after")
    });
    if !written
        .as_ref()
        .is_err_and(|e| e.contains("put back as it was"))
        || vault_value("server_refused-generic").as_deref() != Some("before")
    {
        failures.push(format!(
            "generic: {written:?}, vault {:?}",
            vault_value("server_refused-generic")
        ));
    }

    // The typed writer, with nothing saved before.
    let written = refused(&|| {
        store_typed_credential_dual_as(
            store,
            Some(legacy),
            "oauth_dropbox_refused-typed",
            "oauth",
            "imported-token",
        )
    });
    if written.is_ok() || vault_value("oauth_dropbox_refused-typed").is_some() {
        failures.push(format!(
            "typed: {written:?}, vault {:?}",
            vault_value("oauth_dropbox_refused-typed")
        ));
    }

    // The writer of an import with no type.
    let written = refused(&|| {
        store_imported_credential_for_user(store, legacy, "server_refused-import", None, "x")
    });
    if written.is_ok() || vault_value("server_refused-import").is_some() {
        failures.push(format!(
            "import: {written:?}, vault {:?}",
            vault_value("server_refused-import")
        ));
    }

    // A new OAuth authorization of the legacy owner.
    let tokens = crate::providers::oauth2::StoredTokens {
        access_token: "refused-access".into(),
        refresh_token: Some("refused-refresh".into()),
        expires_at: None,
        token_type: "Bearer".into(),
        scopes: Vec::new(),
    };
    let manager = crate::providers::oauth2::OAuth2Manager::new().with_credential_user(Some(legacy));
    let written = refused(&|| {
        manager
            .store_tokens(
                crate::providers::oauth2::OAuthProvider::Dropbox,
                "refused-oauth",
                &tokens,
            )
            .map_err(|e| e.to_string())
    });
    if written.is_ok() || vault_value("oauth_dropbox_refused-oauth").is_some() {
        failures.push(format!(
            "oauth: {written:?}, vault {:?}",
            vault_value("oauth_dropbox_refused-oauth").is_some()
        ));
    }

    // Without the refusal each writer saves both halves.
    store_credential_dual_as(store, Some(legacy), "server_refused-generic", "after").unwrap();
    assert_eq!(
        vault_value("server_refused-generic").as_deref(),
        Some("after")
    );
    assert_eq!(
        get_user_credential_for(
            &accounts.conn,
            &accounts.root_key,
            legacy,
            "server_refused-generic"
        )
        .unwrap()
        .as_deref()
        .map(String::as_str),
        Some("after")
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A mark the purge left without a fingerprint (the account locked, its row
/// not indexed), and a sign-in that saves a new session for the profile
/// after the unlock read the marks and before it fingerprints the row: the
/// sign-in took the mark, so the unlock neither puts it back nor ends the new
/// session, which stays with its row.
#[test]
fn a_sign_in_during_the_unlock_cleanup_keeps_its_session() {
    if !in_isolated_child("a_sign_in_during_the_unlock_cleanup_keeps_its_session") {
        return;
    }
    let accounts = Accounts::open();
    defer_unindexed_target_session(&accounts, "raced", "raced-purged-uid");
    let owner = accounts.target_owner();
    *BEFORE_DEFERRED_SESSION_END.lock().unwrap() = Some(Box::new(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(vault::replace_session(
                owner,
                Some("raced"),
                &session("raced-new-uid", "n1"),
            ))
            .unwrap();
    }));
    accounts.unlock_target();
    assert!(
        BEFORE_DEFERRED_SESSION_END.lock().unwrap().is_none(),
        "the sign-in ran"
    );
    assert_eq!(
        accounts.target_row("raced").as_deref(),
        Some("n1"),
        "the session saved by the sign-in is not ended"
    );
    assert_eq!(deferred_revokes(&accounts.conn), 0, "no mark is put back");
}

/// An unlock whose deferred cleanup cannot end its session in time (offline,
/// here the purged session's lease held elsewhere) is not held by it: the
/// unlock returns within the cleanup's budget with the row and the mark in
/// place, and the cleanup goes on in the background, ending the session and
/// removing both once the lease is free.
#[test]
fn an_unlock_is_not_held_by_a_deferred_cleanup() {
    if !in_isolated_child("an_unlock_is_not_held_by_a_deferred_cleanup") {
        return;
    }
    let accounts = Accounts::open();
    defer_target_session(&accounts, "slow-cleanup", "slow-cleanup-uid");
    let lease = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(crate::providers::oauth2::RefreshLease::acquire_checked(
            &vault::refresh_lease_slug("slow-cleanup-uid"),
        ))
        .unwrap()
        .unwrap();
    vault::test_seams::set_exclusive_wait(Some(std::time::Duration::from_secs(8)));
    *DEFERRED_CLEANUP_BUDGET_IN_TEST.lock().unwrap() = Some(std::time::Duration::from_millis(300));
    let revokes = vault::test_seams::revokes();
    let started = std::time::Instant::now();
    accounts.unlock_target();
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(3),
        "the unlock is not held by the cleanup: {took:?}"
    );
    assert_eq!(accounts.target_row("slow-cleanup").as_deref(), Some("r1"));
    assert_eq!(deferred_revokes(&accounts.conn), 1, "the mark waits");

    drop(lease);
    let ended = std::time::Instant::now();
    while deferred_revokes(&accounts.conn) != 0
        && ended.elapsed() < std::time::Duration::from_secs(20)
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    *DEFERRED_CLEANUP_BUDGET_IN_TEST.lock().unwrap() = None;
    vault::test_seams::set_exclusive_wait(None);
    assert_eq!(
        deferred_revokes(&accounts.conn),
        0,
        "the background cleanup ends it"
    );
    assert_eq!(vault::test_seams::revokes(), revokes + 1);
    assert!(!has_row(
        &accounts.conn,
        accounts.target.id,
        &key("slow-cleanup")
    ));
}

/// A configuration template's Proton Drive block brings its saved session,
/// and its apply says the session is now shared with the external
/// configuration only once the store took it: for the account that stored
/// it, not for an account the store kept it out of because another account
/// holds the session.
#[test]
fn a_template_import_says_shared_only_for_a_session_it_stored() {
    if !in_isolated_child("a_template_import_says_shared_only_for_a_session_it_stored") {
        return;
    }
    use crate::template_import::{apply_template_for_user, TemplateApplyOptions, TemplateInput};
    let accounts = Accounts::open();
    let template = "[pd]\ntype = protondrive\nusername = someone@example.test\n\
        client_uid = template-uid\nclient_access_token = access-1\n\
        client_refresh_token = refresh-1\nclient_salted_key_pass = a2V5LXBhc3N3b3JkLTE=\n";
    let apply = |user: i64| {
        let applied = apply_template_for_user(
            &accounts.store,
            user,
            &TemplateInput::Text {
                content: template.to_string(),
            },
            &TemplateApplyOptions {
                default_secret_form: Some(crate::rclone_import::TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let shared = applied
            .warnings
            .iter()
            .any(|warning| warning.reason == crate::rclone_import::TEMPLATE_SESSION_SHARED);
        (applied.applied[0].profile_id.clone(), shared)
    };
    let uid_of = |owner: vault::SessionOwner, id: &str| {
        vault::load_session(owner, Some(id))
            .unwrap()
            .map(|session| session.tokens.uid)
    };

    let (owners_id, shared) = apply(accounts.legacy);
    assert_eq!(
        uid_of(accounts.owners_owner(), &owners_id).as_deref(),
        Some("template-uid")
    );
    assert!(shared, "a stored session is reported shared");

    cli_set_active_user(&accounts.store, accounts.source.id).unwrap();
    accounts.unlock_source();
    let (id, shared) = apply(accounts.source.id);
    assert_eq!(
        uid_of(accounts.owner(), &id),
        None,
        "the store kept out a session another account holds"
    );
    assert!(!shared, "a session left out is not reported shared");
}
