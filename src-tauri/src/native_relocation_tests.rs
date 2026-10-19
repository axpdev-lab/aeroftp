//! Synthetic, isolated regressions for the production copy-user/move-user path.
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
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const TEST: &str =
    "user_partitions::native_relocation_tests::native_cross_user_relocation_is_guarded_and_atomic";
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

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn hook(callback: impl FnMut(&str) + 'static) {
    NATIVE_RELOCATION_HOOK.set(Some(Box::new(callback)));
}

fn clear_hook() {
    NATIVE_RELOCATION_HOOK.set(None);
}

fn assert_pair(raw: &str, uid: &str, generation: &str) {
    let saved: StoredSession = serde_json::from_str(raw).unwrap();
    assert_eq!(saved.uid, uid);
    assert_eq!(saved.access_token, format!("access-{generation}"));
    assert_eq!(saved.refresh_token, Some(format!("refresh-{generation}")));
    assert_eq!(saved.key_password, "synthetic-key-password");
}

fn replacement_child() {
    let _test = test_lock();
    CredentialStore::init().unwrap();
    let owner = std::env::var("AEROFTP_RELOCATION_SOURCE_USER")
        .unwrap()
        .parse()
        .unwrap();
    let marker = PathBuf::from(std::env::var_os("AEROFTP_RELOCATION_PROBE").unwrap());
    let raw = serde_json::to_string(&StoredSession::from(&session(
        "replacement-uid",
        "replacement",
    )))
    .unwrap();
    std::fs::write(marker.with_extension("started"), b"ready").unwrap();
    runtime()
        .block_on(vault::restore_session(
            vault::SessionOwner::for_user(Some(owner)),
            "replacement-source",
            &raw,
        ))
        .unwrap();
    std::fs::write(marker.with_extension("finished"), b"done").unwrap();
}

#[test]
fn native_cross_user_relocation_is_guarded_and_atomic() {
    match std::env::var(MODE).as_deref() {
        Ok("replacement") => {
            replacement_child();
            return;
        }
        Ok("suite") => {}
        _ => {
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
    }
    let _test = test_lock();
    CredentialStore::init().unwrap();
    let store = CredentialStore::from_cache().unwrap();
    init_or_migrate_cli(&store).unwrap();
    let mut conn = open_or_init_cli().unwrap();
    let root_key = store.derive_user_partition_wrapping_key();
    let active = active_user_id(&conn).unwrap().unwrap();
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
    let source_id = source.id;

    let seed = |id: &str, uid: &str| {
        let mut db = open_or_init_cli().unwrap();
        let mut profiles = list_server_profiles_for(&db, &root_key, source.id).unwrap();
        profiles.push(
            json!({"id":id,"name":id,"protocol":"proton","host":"drive.proton.me",
            "port":443,"username":uid,"options":{"proton_mode":"native"}}),
        );
        replace_server_profiles_for(&mut db, &root_key, source.id, &profiles).unwrap();
        let saved = session(uid, "r1");
        vault::store_session(owner, Some(id), &saved).unwrap();
        saved
    };
    let relocate = |id: &str, new_id: &str, moved: bool| {
        cli_relocate_server_profile(
            &store,
            source.id,
            target.id,
            id,
            new_id,
            Some(TARGET_PASSWORD),
            moved,
        )
    };
    let target_raw = |id: &str| {
        read_credential_with_dek(&conn, target.id, &target_dek, &vault::session_key(Some(id)))
            .unwrap()
            .unwrap()
    };
    let source_has = |id: &str| {
        list_server_profiles_for(&conn, &root_key, source.id)
            .unwrap()
            .iter()
            .any(|profile| profile["id"] == id)
    };
    let target_has = |id: &str| {
        read_profiles_with_dek(&conn, target.id, &target_dek)
            .unwrap()
            .iter()
            .any(|profile| profile["id"] == id)
    };

    // R1 is captured under the key locks. A real refresh store saves R2 in
    // another thread before relocation acquires its UID lease. A retained
    // SessionHold in the caller would deadlock this worker's partition mirror.
    let saved = seed("copy-source", "copy-uid");
    hook(move |stage| {
        if stage == "captured" {
            let saved = saved.clone();
            let stale = serde_json::to_string(&StoredSession::from(&saved)).unwrap();
            let (finished_tx, finished_rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let refresher = vault::VaultSession::new(owner, Some("copy-source"), &saved);
                let _lease = runtime().block_on(refresher.lock()).unwrap();
                refresher.save(&session("copy-uid", "r2").tokens);
                finished_tx.send(()).unwrap();
            });
            finished_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("refresh could not acquire the source session during relocation");
            worker.join().unwrap();
            // Model a missed best-effort mirror: the vault holds R2, while
            // the scoped source partition still has R1. A generic second
            // copy after the native transaction would overwrite R2 with R1.
            set_user_credential_for(
                &open_or_init_cli().unwrap(),
                &root_key,
                source_id,
                &vault::session_key(Some("copy-source")),
                "proton_native_session",
                &stale,
            )
            .unwrap();
        }
    });
    let copied = relocate("copy-source", "copy-target", false).unwrap();
    clear_hook();
    assert!(copied.inserted && !copied.moved);
    assert_pair(&target_raw("copy-target"), "copy-uid", "r2");
    assert_pair(
        &store.get(&vault::session_key(Some("copy-target"))).unwrap(),
        "copy-uid",
        "r2",
    );
    assert_eq!(
        vault::load_session(owner, Some("copy-source"))
            .unwrap()
            .unwrap()
            .tokens
            .access_token
            .expose_secret(),
        "access-r2"
    );
    assert_pair(
        &get_user_credential_for(
            &conn,
            &root_key,
            source.id,
            &vault::session_key(Some("copy-source")),
        )
        .unwrap()
        .unwrap(),
        "copy-uid",
        "r1",
    );
    assert!(source_has("copy-source"));
    assert_eq!(active_user_id(&conn).unwrap(), Some(active));
    assert!(get_user_credential_for(
        &conn,
        &root_key,
        active,
        &vault::session_key(Some("copy-target"))
    )
    .unwrap()
    .is_none());
    let dedup = relocate("copy-source", "copy-dedup", false).unwrap();
    assert!(dedup.already_present && !dedup.inserted);
    assert!(!target_has("copy-dedup"));
    assert!(matches!(
        store.get(&vault::session_key(Some("copy-dedup"))),
        Err(CredentialError::NotFound(_))
    ));

    // Move waits for a refresher which already owns the UID lease. During the
    // wait neither profile list has changed. R2 is committed before source
    // removal and cannot be resurrected by the completed refresher.
    let saved = seed("move-source", "move-uid");
    let (held_tx, held_rx) = mpsc::channel();
    let (captured_tx, captured_rx) = mpsc::channel();
    let refresh = std::thread::spawn(move || {
        let refresher = vault::VaultSession::new(owner, Some("move-source"), &saved);
        let _lease = runtime().block_on(refresher.lock()).unwrap();
        held_tx.send(()).unwrap();
        captured_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let db = open_or_init_cli().unwrap();
        let root = user_crypto::secret_key_from_bytes(&root_key);
        let source_tag = user_crypto::metadata_tag(&root, b"profile-uid", "move-source").unwrap();
        let target_tag = user_crypto::metadata_tag(&root, b"profile-uid", "move-target").unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM server_profiles WHERE user_id=?1 AND profile_uid=?2",
                params![source_id, source_tag],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM server_profiles WHERE profile_uid=?1",
                [target_tag],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        refresher.save(&session("move-uid", "r2").tokens);
    });
    held_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    hook(move |stage| {
        if stage == "captured" {
            captured_tx.send(()).unwrap();
        }
    });
    relocate("move-source", "move-target", true).unwrap();
    clear_hook();
    refresh.join().unwrap();
    assert!(!source_has("move-source"));
    assert_pair(&target_raw("move-target"), "move-uid", "r2");
    assert_pair(
        &store.get(&vault::session_key(Some("move-target"))).unwrap(),
        "move-uid",
        "r2",
    );
    assert!(get_user_credential_for(
        &conn,
        &root_key,
        source.id,
        &vault::session_key(Some("move-source"))
    )
    .unwrap()
    .is_none());
    assert!(matches!(
        store.get(&vault::session_key(Some("move-source"))),
        Err(CredentialError::NotFound(_))
    ));

    // A different process tries to replace the source while Move owns its key
    // guard. It completes after Move and its replacement UID is never deleted.
    seed("replacement-source", "before-replacement-uid");
    let child = Arc::new(Mutex::new(None));
    let child_slot = Arc::clone(&child);
    let marker = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("replacement-probe");
    let child_marker = marker.clone();
    hook(move |stage| {
        if stage == "captured" {
            let process = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .env(MODE, "replacement")
                .env("AEROFTP_RELOCATION_SOURCE_USER", source_id.to_string())
                .env("AEROFTP_RELOCATION_PROBE", &child_marker)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !child_marker.with_extension("started").is_file() {
                assert!(Instant::now() < deadline, "replacement child did not start");
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(200));
            assert!(
                !child_marker.with_extension("finished").exists(),
                "replacement bypassed relocation's key guard"
            );
            *child_slot.lock().unwrap() = Some(process);
        }
    });
    relocate("replacement-source", "replacement-target", true).unwrap();
    clear_hook();
    let output = child
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_pair(
        &target_raw("replacement-target"),
        "before-replacement-uid",
        "r1",
    );
    assert_pair(
        &store
            .get(&vault::session_key(Some("replacement-source")))
            .unwrap(),
        "replacement-uid",
        "replacement",
    );
    assert!(!source_has("replacement-source"));

    // A source held only in its partition still adopts a locally rotated pair
    // from another saved copy of the same UID (e.g. after a prior vault error).
    seed("partition-only-source", "partition-only-uid");
    vault::store_session(
        owner,
        Some("partition-only-shared"),
        &session("partition-only-uid", "r2"),
    )
    .unwrap();
    store
        .delete(&vault::session_key(Some("partition-only-source")))
        .unwrap();
    relocate("partition-only-source", "partition-only-target", true).unwrap();
    assert_pair(
        &target_raw("partition-only-target"),
        "partition-only-uid",
        "r2",
    );
    assert_pair(
        &store
            .get(&vault::session_key(Some("partition-only-target")))
            .unwrap(),
        "partition-only-uid",
        "r2",
    );

    // Scoped source ownership wins over a global key belonging to another
    // user's same-id profile. Move leaves that different UID untouched.
    seed("shared-id-source", "scoped-source-uid");
    vault::store_session(
        vault::SessionOwner::for_user(Some(active)),
        Some("shared-id-source"),
        &session("other-owner-uid", "other"),
    )
    .unwrap();
    relocate("shared-id-source", "shared-id-target", true).unwrap();
    assert_pair(&target_raw("shared-id-target"), "scoped-source-uid", "r1");
    assert_pair(
        &store
            .get(&vault::session_key(Some("shared-id-source")))
            .unwrap(),
        "other-owner-uid",
        "other",
    );

    // Password, key-lock, native parse and SQLite errors precede the commit.
    // In particular the credential INSERT trigger runs AFTER both profile
    // lists were edited, proving that the transaction rolls all of them back.
    let saved = seed("fault-source", "fault-uid");
    let absent_password = cli_relocate_server_profile(
        &store,
        source.id,
        target.id,
        "fault-source",
        "password-fault-target",
        None,
        true,
    )
    .unwrap_err();
    assert_eq!(absent_password, "TARGET_PASSPHRASE_REQUIRED");
    assert!(source_has("fault-source") && !target_has("password-fault-target"));
    let locks = crate::portable::aeroftp_data_root()
        .unwrap()
        .join("proton_session_locks");
    let saved_locks = locks.with_extension("saved");
    std::fs::rename(&locks, &saved_locks).unwrap();
    std::fs::write(&locks, b"not a directory").unwrap();
    assert!(relocate("fault-source", "key-fault-target", true).is_err());
    std::fs::remove_file(&locks).unwrap();
    std::fs::rename(&saved_locks, &locks).unwrap();
    assert!(source_has("fault-source") && !target_has("key-fault-target"));
    let target_id = target.id;
    hook(move |stage| {
        if stage == "captured" {
            open_or_init_cli()
                .unwrap()
                .execute(
                    "UPDATE users SET dek_verifier=?1 WHERE id=?2",
                    params![vec![0u8; 32], target_id],
                )
                .unwrap();
        }
    });
    assert_eq!(
        relocate("fault-source", "dek-fault-target", true).unwrap_err(),
        "DEK_VERIFIER_MISMATCH"
    );
    clear_hook();
    conn.execute(
        "UPDATE users SET dek_verifier=?1 WHERE id=?2",
        params![
            user_crypto::compute_dek_verifier(&target_dek)
                .unwrap()
                .to_vec(),
            target.id
        ],
    )
    .unwrap();
    assert!(source_has("fault-source") && !target_has("dek-fault-target"));
    set_user_credential_for(
        &conn,
        &root_key,
        source.id,
        &vault::session_key(Some("fault-source")),
        "proton_native_session",
        "invalid-json",
    )
    .unwrap();
    assert!(relocate("fault-source", "parse-fault-target", true).is_err());
    assert!(source_has("fault-source") && !target_has("parse-fault-target"));
    vault::store_session(owner, Some("fault-source"), &saved).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_native_relocation BEFORE INSERT ON user_credentials
        WHEN NEW.credential_id='proton_native_session_sql-fault-target'
        BEGIN SELECT RAISE(ABORT, 'synthetic native credential failure'); END;",
    )
    .unwrap();
    let error = relocate("fault-source", "sql-fault-target", true).unwrap_err();
    assert!(error.contains("synthetic native credential failure"));
    conn.execute_batch("DROP TRIGGER fail_native_relocation")
        .unwrap();
    assert!(source_has("fault-source") && !target_has("sql-fault-target"));
    assert_pair(
        &store
            .get(&vault::session_key(Some("fault-source")))
            .unwrap(),
        "fault-uid",
        "r1",
    );
    assert!(read_credential_with_dek(
        &conn,
        target.id,
        &target_dek,
        &vault::session_key(Some("sql-fault-target"))
    )
    .unwrap()
    .is_none());
    clear_user_session();
    assert_eq!(
        relocate("fault-source", "locked-fault-target", true).unwrap_err(),
        "USER_LOCKED"
    );
    unlock_user_transient(&conn, &root_key, source.id, Some(SOURCE_PASSWORD)).unwrap();
    assert!(source_has("fault-source") && !target_has("locked-fault-target"));

    // Force the actual vault write to fail AFTER SQLite has committed. The
    // returned error explicitly reports committed state. The valid target
    // partition credential survives, and the source vault copy is retained.
    seed("vault-fault-source", "vault-fault-uid");
    let vault_path = crate::portable::credential_store_dir()
        .unwrap()
        .join(crate::credential_store::VAULT_FILENAME);
    let backup = vault_path.with_extension("saved");
    let fail_path = vault_path.clone();
    let fail_backup = backup.clone();
    hook(move |stage| {
        if stage == "committed" {
            std::fs::rename(&fail_path, &fail_backup).unwrap();
            std::fs::create_dir(&fail_path).unwrap();
        }
    });
    let error = relocate("vault-fault-source", "vault-fault-target", true).unwrap_err();
    clear_hook();
    std::fs::remove_dir(&vault_path).unwrap();
    std::fs::rename(&backup, &vault_path).unwrap();
    assert!(error.starts_with("RELOCATION_COMMITTED:"));
    assert!(!source_has("vault-fault-source") && target_has("vault-fault-target"));
    assert_pair(&target_raw("vault-fault-target"), "vault-fault-uid", "r1");
    assert_pair(
        &store
            .get(&vault::session_key(Some("vault-fault-source")))
            .unwrap(),
        "vault-fault-uid",
        "r1",
    );
    assert!(matches!(
        store.get(&vault::session_key(Some("vault-fault-target"))),
        Err(CredentialError::NotFound(_))
    ));
    assert_eq!(active_user_id(&conn).unwrap(), Some(active));
}
