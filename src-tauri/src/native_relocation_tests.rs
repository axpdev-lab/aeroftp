//! Synthetic, isolated regressions: a native Proton Drive session belongs to
//! one account, on the production save, refresh, sign-out and copy-user /
//! move-user paths.
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

const TEST: &str =
    "user_partitions::native_relocation_tests::a_native_session_keeps_to_its_account";
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

#[test]
fn a_native_session_keeps_to_its_account() {
    if std::env::var(MODE).as_deref() != Ok("suite") {
        // Isolate the real encrypted vault and partition DB from both the
        // owner's credentials and parallel lib tests, as the auth fixture does.
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
        let output = std::process::Command::new(child)
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
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
        return;
    }
    let _test = test_lock();
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
    let owner = vault::SessionOwner::for_user(Some(source.id));
    let owners_owner = vault::SessionOwner::for_user(Some(legacy));
    let key = |id: &str| vault::session_key(Some(id));

    let add_profile = |user: i64, id: &str| {
        let mut db = open_or_init_cli().unwrap();
        let mut profiles = list_server_profiles_for(&db, &root_key, user).unwrap();
        profiles.push(
            json!({"id":id,"name":id,"protocol":"proton","host":"drive.proton.me",
            "port":443,"username":format!("{id}@example.test"),"options":{"proton_mode":"native"}}),
        );
        replace_server_profiles_for(&mut db, &root_key, user, &profiles).unwrap();
    };
    let seed = |id: &str, uid: &str| {
        add_profile(source.id, id);
        vault::store_session(owner, Some(id), &session(uid, "r1")).unwrap();
    };
    let loaded = |owner: vault::SessionOwner, id: &str| {
        vault::load_session(owner, Some(id))
            .unwrap()
            .map(|session| generation(&raw(&session)))
    };
    let target_row = |id: &str| {
        read_credential_with_dek(&conn, target.id, &target_dek, &key(id))
            .unwrap()
            .map(|raw| generation(&raw))
    };
    let in_vault = |id: &str| store.get(&key(id)).is_ok();
    let relocate = |from: i64, id: &str, new_id: &str, moved: bool| {
        cli_relocate_server_profile(
            &store,
            from,
            target.id,
            id,
            new_id,
            Some(TARGET_PASSWORD),
            moved,
        )
        .unwrap()
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
        &conn,
        target.id,
        &target_dek,
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
    assert!(vault::another_profile_holds(&store, owner, &key("dup-a"), "shared-uid").unwrap());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(vault::end_session(owner, Some("dup-b")))
        .unwrap();
    assert_eq!(loaded(owner, "dup-b"), None);
    assert_eq!(loaded(owner, "dup-a").as_deref(), Some("r2"));
    assert!(in_vault("dup-b"));
    store.delete(&key("dup-a")).unwrap();
    store.delete(&key("dup-b")).unwrap();
    assert!(!vault::another_profile_holds(&store, owner, &key("dup-a"), "shared-uid").unwrap());

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
