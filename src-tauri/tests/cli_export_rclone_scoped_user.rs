// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `export rclone` binds every credential lookup to the account that read the
//! profile list, not to whichever account is active when the lookups run.
//!
//! `read_vault_profiles` resolves `--user` (or the pinned active account) and
//! returns that account's id; the OAuth/Filen/Proton/OneDrive injectors read
//! for it. Before that, `cmd_export_rclone` captured `active_user_for_save()`
//! separately: `--user A` with B active exported B's token under A's profile,
//! and with accounts present but none registered active the lookups fell back
//! to an unscoped vault read. The vault and the partition databases are set
//! up in-process against the same isolated root the CLI subprocess uses.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;
#[path = "support/scoped_env.rs"]
mod scoped_env;

use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::Mutex;

use ftp_client_gui_lib::credential_store::CredentialStore;
use ftp_client_gui_lib::user_partitions;
use ftp_client_gui_lib::user_partitions::UserMetadata;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const MASTER: &str = "export scoped user probe vault";
const PROFILE_ID: &str = "shareddod";
const TOKEN_KEY: &str = "oauth_onedrive_shareddod";
const TOKEN_A: &str =
    r#"{"access_token":"token-of-A","refresh_token":"ra","token_type":"Bearer","expires_at":null}"#;
const TOKEN_B: &str =
    r#"{"access_token":"token-of-B","refresh_token":"rb","token_type":"Bearer","expires_at":null}"#;

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The CLI on the isolated vault in `root`, stdin closed and stderr piped.
fn cli(root: &Path, args: &[&str]) -> Output {
    isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
        .args(args)
        .env("AEROFTP_MASTER_PASSWORD", MASTER)
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run aeroftp-cli")
}

/// Point this process's vault resolution at `root` for the length of the
/// returned guards. The CLI subprocess gets the same root from
/// `isolated_cli::command`.
fn point_env_at(root: &Path) -> Vec<scoped_env::ScopedEnv> {
    [
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "APPDATA",
        "LOCALAPPDATA",
        "AEROFTP_MASTER_PASSWORD",
    ]
    .into_iter()
    .map(|name| scoped_env::ScopedEnv::set(name, root))
    .collect()
}

fn shared_profile() -> serde_json::Value {
    serde_json::json!({
        "id": PROFILE_ID,
        "name": "Shared OD",
        "protocol": "onedrive",
        "host": "graph.microsoft.com",
        "port": 443,
        "username": "user@example.com",
        "options": {}
    })
}

/// A fresh vault with the default owner account. The caller must hold
/// [`ENV_LOCK`] and have pointed the environment at the vault's root: the
/// vault path is process-wide state.
fn vault_with_owner() -> (CredentialStore, UserMetadata) {
    CredentialStore::clear_cache();
    CredentialStore::bootstrap_master_password(MASTER).expect("bootstrap vault");
    CredentialStore::unlock_with_master(MASTER, None).expect("unlock vault");
    let store = CredentialStore::from_cache().expect("credential store");
    user_partitions::init_or_migrate_cli(&store).expect("init partitions");
    let owner = user_partitions::cli_get_active_user(&store)
        .expect("active user")
        .expect("a default account exists");
    (store, owner)
}

fn give_shared_profile_and_token(store: &CredentialStore, user: &UserMetadata, token: &str) {
    user_partitions::cli_update_server_profiles_for_user(store, user.id, |profiles| {
        profiles.push(shared_profile());
        true
    })
    .unwrap_or_else(|e| panic!("profiles for {}: {e}", user.name));
    user_partitions::store_credential_for_user_dual(store, user.id, TOKEN_KEY, token)
        .unwrap_or_else(|e| panic!("token for {}: {e}", user.name));
}

fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("copy target dir");
    for entry in std::fs::read_dir(src).expect("copy source dir") {
        let entry = entry.expect("copy entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("entry type").is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// The CLI subprocess runs portable: `isolated_cli` plants `portable.marker`
/// beside its hard-linked binary, so its data root is `<root>/cli/data/
/// aeroftp-dev`, not the XDG tree this process resolved. Copy the vault and
/// partition databases the in-process setup just wrote into that portable
/// root. Must run while the environment still points at `root`.
fn stage_portable_vault(root: &Path) {
    let src = ftp_client_gui_lib::portable::cli_app_config_dir().expect("config dir");
    let leaf = ftp_client_gui_lib::portable::aeroftp_data_leaf_for_debug(cfg!(debug_assertions));
    let dst = root.join("cli").join("data").join(leaf);
    copy_dir_all(&src, &dst);
}

#[test]
fn export_rclone_with_user_reads_that_accounts_token_while_another_is_active() {
    let root = isolated_cli::root();
    let root = root.path();
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = point_env_at(root);
    let (store, _owner) = vault_with_owner();
    // The rclone remote needs the BYO app pair next to the token; those live
    // in the shared vault, not in a partition.
    store
        .store("oauth_onedrive_client_id", "cid-probe")
        .expect("store client id");
    store
        .store("oauth_onedrive_client_secret", "csec-probe")
        .expect("store client secret");

    // Two non-owner accounts hold the same profile id with different tokens;
    // B is the active one, A is the `--user` target.
    let user_a = user_partitions::cli_create_user(&store, "Alpha", None, None, None)
        .expect("create account A");
    let user_b = user_partitions::cli_create_user(&store, "Bravo", None, None, None)
        .expect("create account B");
    give_shared_profile_and_token(&store, &user_a, TOKEN_A);
    give_shared_profile_and_token(&store, &user_b, TOKEN_B);
    user_partitions::cli_set_active_user(&store, user_b.id).expect("activate B");
    drop(store);
    stage_portable_vault(root);
    drop(_env);
    drop(_lock);

    let out_path = root.join("export.conf");
    let out = cli(
        root,
        &[
            "--user",
            "Alpha",
            "export",
            "rclone",
            "-o",
            out_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let exported = std::fs::read_to_string(&out_path).expect("export file");
    assert!(
        exported.contains("token-of-A"),
        "the export must carry account A's token:\n{exported}"
    );
    assert!(
        !exported.contains("token-of-B"),
        "account B's token must not leak into A's export:\n{exported}"
    );
}

fn jottacloud_profile() -> serde_json::Value {
    serde_json::json!({
        "id": "sharedjotta",
        "name": "Shared Jotta",
        "protocol": "jottacloud",
        "host": "jottacloud.com",
        "port": 443,
        "username": "a@example.com",
        "options": {}
    })
}

fn ftp_profile() -> serde_json::Value {
    serde_json::json!({
        "id": "sharedftp",
        "name": "Shared FTP",
        "protocol": "ftp",
        "host": "ftp.example.com",
        "port": 21,
        "username": "u",
        "options": {}
    })
}

fn give_profile_and_secret(
    store: &CredentialStore,
    user: &UserMetadata,
    profile: serde_json::Value,
    secret_key: &str,
    secret: &str,
) {
    user_partitions::cli_update_server_profiles_for_user(store, user.id, |profiles| {
        profiles.push(profile);
        true
    })
    .unwrap_or_else(|e| panic!("profiles for {}: {e}", user.name));
    user_partitions::store_credential_for_user_dual(store, user.id, secret_key, secret)
        .unwrap_or_else(|e| panic!("secret for {}: {e}", user.name));
}

/// Two accounts hold the same profile ids with different secrets; B is
/// active, A is the `--user` target. Returns after staging the portable
/// vault, with `ENV_LOCK` and the env guards released.
fn staged_two_account_vault(
    root: &Path,
    seed: impl Fn(&CredentialStore, &UserMetadata, &UserMetadata),
) {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = point_env_at(root);
    let (store, _owner) = vault_with_owner();
    let user_a = user_partitions::cli_create_user(&store, "Alpha", None, None, None)
        .expect("create account A");
    let user_b = user_partitions::cli_create_user(&store, "Bravo", None, None, None)
        .expect("create account B");
    seed(&store, &user_a, &user_b);
    user_partitions::cli_set_active_user(&store, user_b.id).expect("activate B");
    drop(store);
    stage_portable_vault(root);
    drop(_env);
    drop(_lock);
}

#[test]
fn export_rclone_with_user_reads_that_accounts_jottacloud_chain() {
    let root = isolated_cli::root();
    let root = root.path();
    staged_two_account_vault(root, |store, user_a, user_b| {
        for (user, refresh) in [(user_a, "refresh-of-A"), (user_b, "refresh-of-B")] {
            give_profile_and_secret(
                store,
                user,
                jottacloud_profile(),
                "jottacloud_refresh_sharedjotta",
                &format!(
                    r#"{{"refresh_token":"{refresh}","token_endpoint":"https://idp.jottacloud.com/token","username":"a@example.com"}}"#
                ),
            );
        }
    });

    let out_path = root.join("export-jotta.conf");
    let out = cli(
        root,
        &[
            "--user",
            "Alpha",
            "export",
            "rclone",
            "-o",
            out_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let exported = std::fs::read_to_string(&out_path).expect("export file");
    assert!(
        exported.contains("refresh-of-A"),
        "the export must carry account A's Jotta chain:\n{exported}"
    );
    assert!(
        !exported.contains("refresh-of-B"),
        "account B's Jotta chain must not leak into A's export:\n{exported}"
    );
}

#[test]
fn export_lftp_with_user_reads_that_accounts_server_password() {
    let root = isolated_cli::root();
    let root = root.path();
    staged_two_account_vault(root, |store, user_a, user_b| {
        for (user, password) in [(user_a, "pw-of-A"), (user_b, "pw-of-B")] {
            give_profile_and_secret(
                store,
                user,
                ftp_profile(),
                "server_sharedftp",
                &format!(r#"{{"password":"{password}"}}"#),
            );
        }
    });

    let out_path = root.join("export.lftp");
    let out = cli(
        root,
        &[
            "--user",
            "Alpha",
            "export",
            "lftp",
            "-o",
            out_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let exported = std::fs::read_to_string(&out_path).expect("export file");
    assert!(
        exported.contains("pw-of-A"),
        "the export must carry account A's password:\n{exported}"
    );
    assert!(
        !exported.contains("pw-of-B"),
        "account B's password must not leak into A's export:\n{exported}"
    );
}

#[test]
fn export_with_user_and_no_active_account_reads_that_account_not_the_vault() {
    let root = isolated_cli::root();
    let root = root.path();
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = point_env_at(root);
    let (store, _owner) = vault_with_owner();
    let user_a = user_partitions::cli_create_user(&store, "Alpha", None, None, None)
        .expect("create account A");
    user_partitions::cli_create_user(&store, "Bravo", None, None, None).expect("create account B");
    give_profile_and_secret(
        &store,
        &user_a,
        jottacloud_profile(),
        "jottacloud_refresh_sharedjotta",
        r#"{"refresh_token":"refresh-of-partition-A","token_endpoint":"https://idp.jottacloud.com/token","username":"a@example.com"}"#,
    );
    give_profile_and_secret(
        &store,
        &user_a,
        ftp_profile(),
        "server_sharedftp",
        r#"{"password":"pw-of-partition-A"}"#,
    );
    // No active account: `--user Alpha` is the only scope. The shared vault
    // holds different values on the same keys, so a read that fell back to
    // it (or to `active_credential_user_id`, which is None here) exports
    // the vault's secrets or nothing, never A's.
    let conn = user_partitions::open_or_init_cli().expect("cli db");
    conn.execute("DELETE FROM global_state WHERE key = 'active_user_id'", [])
        .expect("clear active user");
    drop(conn);
    store
        .store(
            "jottacloud_refresh_sharedjotta",
            r#"{"refresh_token":"refresh-of-vault","token_endpoint":"https://idp.jottacloud.com/token","username":"v@example.com"}"#,
        )
        .expect("vault jottacloud chain");
    store
        .store("server_sharedftp", r#"{"password":"pw-of-vault"}"#)
        .expect("vault server password");
    drop(store);
    stage_portable_vault(root);
    drop(_env);
    drop(_lock);

    let out_path = root.join("export-noactive.conf");
    let out = cli(
        root,
        &[
            "--user",
            "Alpha",
            "export",
            "rclone",
            "-o",
            out_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let exported = std::fs::read_to_string(&out_path).expect("export file");
    assert!(
        exported.contains("refresh-of-partition-A"),
        "the export must carry account A's Jotta chain:\n{exported}"
    );
    assert!(
        !exported.contains("refresh-of-vault"),
        "the unscoped vault chain must not leak into A's export:\n{exported}"
    );

    let lftp_path = root.join("export-noactive.lftp");
    let out = cli(
        root,
        &[
            "--user",
            "Alpha",
            "export",
            "lftp",
            "-o",
            lftp_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let exported = std::fs::read_to_string(&lftp_path).expect("lftp export file");
    assert!(
        exported.contains("pw-of-partition-A"),
        "the export must carry account A's password:\n{exported}"
    );
    assert!(
        !exported.contains("pw-of-vault"),
        "the unscoped vault password must not leak into A's export:\n{exported}"
    );
}

#[test]
fn export_rclone_without_user_and_no_active_account_reads_no_vault() {
    let root = isolated_cli::root();
    let root = root.path();
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = point_env_at(root);
    let (store, owner) = vault_with_owner();
    give_shared_profile_and_token(&store, &owner, TOKEN_A);
    user_partitions::cli_create_user(&store, "Bravo", None, None, None)
        .expect("create second account");
    // Accounts exist but no active one is registered: the export has no scope
    // to read credentials for and must fail closed instead of touching the
    // shared vault.
    let conn = user_partitions::open_or_init_cli().expect("cli db");
    conn.execute("DELETE FROM global_state WHERE key = 'active_user_id'", [])
        .expect("clear active user");
    drop(conn);
    drop(store);
    stage_portable_vault(root);
    drop(_env);
    drop(_lock);

    let out_path = root.join("export.conf");
    let out = cli(
        root,
        &[
            "export",
            "rclone",
            "-o",
            out_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "an unscoped export must not succeed:\n{}",
        describe(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("NO_ACTIVE_USER")
            || String::from_utf8_lossy(&out.stderr).contains("NO_ACTIVE_USER"),
        "the export must fail on the missing scope, not on a vault read:\n{}",
        describe(&out)
    );
    assert!(!out_path.exists(), "no export file may be written");
}

/// Environment guards whose drop is slow enough for an observer to see the
/// process environment while `ENV_LOCK` is already free but the variables
/// are not all restored yet: the window a fixture that dropped the lock
/// first would open.
struct SlowEnv(Vec<scoped_env::ScopedEnv>);

impl Drop for SlowEnv {
    fn drop(&mut self) {
        for guard in self.0.drain(..) {
            drop(guard);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

/// The fixtures hold `ENV_LOCK` while they point the process environment at
/// their root and release it only after the `ScopedEnv` guards are dropped.
/// A test that dropped the lock first would leave a window in which
/// `ENV_LOCK` is free but `XDG_CONFIG_HOME` still names the fixture's root:
/// the next test to take the lock would read an environment that is not its
/// own. An observer thread spins on `ENV_LOCK` and reads the variable the
/// moment the lock opens; with the guards dropped first it can only see the
/// value the process had before.
#[test]
fn the_fixture_restores_its_environment_before_the_lock_opens() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = isolated_cli::root();
    let before = std::env::var_os("XDG_CONFIG_HOME");

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let observer = {
        let seen = seen.clone();
        std::thread::spawn(move || {
            ready_tx.send(()).expect("observer ready");
            loop {
                let guard = match ENV_LOCK.try_lock() {
                    Ok(guard) => guard,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::hint::spin_loop();
                        continue;
                    }
                    Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                };
                *seen.lock().unwrap() = Some(std::env::var_os("XDG_CONFIG_HOME"));
                drop(guard);
                return;
            }
        })
    };
    ready_rx.recv().expect("observer thread started");

    let env = SlowEnv(point_env_at(root.path()));
    // The fixture order: the environment guards first, the lock second.
    drop(env);
    drop(_lock);

    observer.join().expect("observer thread");
    assert_eq!(
        seen.lock().unwrap().as_ref(),
        Some(&before),
        "the environment must already be restored when ENV_LOCK opens"
    );
}
