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
    drop(_lock);
    drop(_env);

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
    drop(_lock);
    drop(_env);

    let out = cli(root, &["export", "rclone", "--json"]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "an unscoped export must not succeed:\n{}",
        describe(&out)
    );
    assert!(
        !root.join("export.conf").exists(),
        "no export file may be written"
    );
}
