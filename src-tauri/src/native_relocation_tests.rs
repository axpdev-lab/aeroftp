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

    fn profile_ids(&self, user: i64) -> Vec<String> {
        let db = open_or_init_cli().unwrap();
        list_server_profiles_for(&db, &self.root_key, user)
            .unwrap()
            .iter()
            .filter_map(|profile| profile["id"].as_str().map(str::to_string))
            .collect()
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
    // The sign-out of a session no other profile holds revokes it.
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
        .block_on(crate::providers::oauth2::RefreshLease::acquire(
            &vault::refresh_lease_slug("busy-uid"),
        ))
        .unwrap();
    std::thread::scope(|scope| {
        let mover = scope.spawn(|| relocate(source.id, "busy", "busy-moved", true));
        std::thread::sleep(std::time::Duration::from_millis(500));
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
    assert_eq!(loaded(owners_owner, "owners-shared").as_deref(), Some("r2"));
    assert_eq!(
        loaded(owner, "owners-shared").as_deref(),
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
    assert_eq!(loaded(owners_owner, "aligned").as_deref(), Some("r1"));
}
