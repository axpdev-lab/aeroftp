// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `profile-import --on-existing` on an isolated vault: re-importing a backup
//! of a profile the vault still holds skips it by default, overwrites it in
//! place under the same id, or adds a copy under a new id and the next free
//! name, and `--dry-run` reports the plan without writing it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Exit code, stdout and stderr of a run, for an assertion message.
fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The CLI on the vault in `config`, with nothing of this machine's.
fn cli(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(args)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", config)
        .env("HOME", config)
        .env("APPDATA", config)
        .env("LOCALAPPDATA", config)
        .env(
            "AEROFTP_MASTER_PASSWORD",
            "existing profile import probe vault",
        )
        .env_remove("RUST_LOG")
        .env_remove("AEROFTP_PROFILE_PASSWORD")
        .output()
        .expect("run aeroftp-cli")
}

/// The JSON a successful run printed.
fn json(out: &Output) -> serde_json::Value {
    assert!(out.status.success(), "{}", describe(out));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(out)))
}

/// The saved profiles as (id, name), in order.
fn saved(config: &Path) -> Vec<(String, String)> {
    let listed = json(&cli(config, &["profiles", "--json"]));
    listed
        .get("profiles")
        .and_then(|p| p.as_array())
        .or_else(|| listed.as_array())
        .unwrap_or_else(|| panic!("no profile list in {listed}"))
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap_or_default().to_string(),
                p["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

const PASSWORD: &str = "existing profile import probe export";

/// `profile-import --json` of `file` with `extra` flags, as JSON.
fn import(config: &Path, file: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec![
        "profile-import",
        "-i",
        file.to_str().unwrap(),
        "--password",
        PASSWORD,
        "--json",
    ];
    args.extend_from_slice(extra);
    json(&cli(config, &args))
}

/// The saved profiles whole (`profiles --json` lists no host or options),
/// read back through an export without restoring anything.
async fn saved_whole(config: &Path) -> Vec<serde_json::Value> {
    let file = config.join(format!("{}.aeroftp", uuid::Uuid::new_v4()));
    let out = cli(
        config,
        &[
            "profile-export",
            "-o",
            file.to_str().unwrap(),
            "--password",
            PASSWORD,
        ],
    );
    assert!(out.status.success(), "{}", describe(&out));
    ftp_client_gui_lib::preview_server_profiles(
        &file.to_string_lossy(),
        PASSWORD,
        ftp_client_gui_lib::profile_import_plan::ExistingProfileMode::Skip,
        &[],
    )
    .unwrap()
    .servers
}

/// Skip leaves a profile still held alone, Overwrite applies the backup's
/// fields to it in place (same id and name) unless the backup points it at
/// another server, and Duplicate adds a copy under a
/// new id and the next free name, which its dry run already reports.
#[tokio::test]
async fn a_backup_of_a_profile_still_held_is_skipped_overwritten_or_duplicated() {
    let config = tempfile::tempdir().unwrap();
    let config = config.path();
    let conf = config.join("rclone.conf");
    std::fs::write(
        &conf,
        "[lab]\ntype = sftp\nhost = sftp.example.invalid\nuser = smoke\n",
    )
    .unwrap();
    json(&cli(
        config,
        &[
            "import",
            "rclone",
            conf.to_str().unwrap(),
            "--apply",
            "--json",
        ],
    ));
    let file = config.join("profiles.aeroftp");
    let exported = cli(
        config,
        &[
            "profile-export",
            "-o",
            file.to_str().unwrap(),
            "--password",
            PASSWORD,
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));
    let original = saved(config);
    assert_eq!(original.len(), 1);
    let id = original[0].0.clone();

    let skip = import(config, &file, &[]);
    assert_eq!(skip["on_existing"], "skip", "{skip}");
    assert_eq!(skip["imported"], 0, "{skip}");
    assert_eq!(skip["skipped"], 1, "{skip}");
    assert_eq!(saved(config), original);

    // A backup of the same profile (same id) whose folder differs from the one
    // saved: an Overwrite must apply it. Its host differs in `moved`, which an
    // Overwrite must refuse, so a stored password cannot reach another server.
    let mut restored = saved_whole(config).await.remove(0);
    assert_eq!(restored["host"], "sftp.example.invalid");
    restored["initialPath"] = "/restored".into();
    let write_backup = |name: &str, profile: &serde_json::Value| {
        let path = config.join(name);
        let servers = serde_json::json!([profile]).to_string();
        let target = path.to_string_lossy().into_owned();
        async move {
            ftp_client_gui_lib::export_server_profiles_core(
                servers,
                PASSWORD.into(),
                false,
                target,
            )
            .await
            .unwrap();
            path
        }
    };
    let mut moved = restored.clone();
    moved["host"] = "other.example.invalid".into();
    let moved_file = write_backup("moved.aeroftp", &moved).await;
    let refused = import(config, &moved_file, &["--on-existing", "overwrite"]);
    assert_eq!(refused["overwritten"], 0, "{refused}");
    assert_eq!(refused["skipped"], 1, "{refused}");
    assert_eq!(refused["target_changed"][0], "lab", "{refused}");
    assert_eq!(saved_whole(config).await[0]["host"], "sftp.example.invalid");
    let backup = write_backup("restored.aeroftp", &restored).await;
    let overwrite = import(config, &backup, &["--on-existing", "overwrite"]);
    assert_eq!(overwrite["imported"], 1, "{overwrite}");
    assert_eq!(overwrite["overwritten"], 1, "{overwrite}");
    assert_eq!(overwrite["similar"], 0, "{overwrite}");
    assert_eq!(
        overwrite["target_changed"],
        serde_json::json!([]),
        "{overwrite}"
    );
    assert_eq!(
        saved(config),
        original,
        "replaced in place, same id and name"
    );
    assert_eq!(saved_whole(config).await[0]["initialPath"], "/restored");

    // Duplicated from the backup, whose account (the restored host) the saved
    // profile now shares, so the copy is reported as similar.
    let whole_before = saved_whole(config).await;
    let preview = import(
        config,
        &backup,
        &["--on-existing", "duplicate", "--dry-run"],
    );
    assert_eq!(preview["dry_run"], true, "{preview}");
    assert_eq!(preview["would_duplicate"], 1, "{preview}");
    assert_eq!(preview["duplicated"][0], "lab (1)", "{preview}");
    assert_eq!(saved(config), original, "a dry run writes nothing");
    assert_eq!(
        saved_whole(config).await,
        whole_before,
        "a dry run writes nothing"
    );

    // Restored secrets also reach the shared vault key, so a --user that is not
    // the active account is refused; a dry run writes nothing and may preview it.
    let added = cli(config, &["users", "add", "other"]);
    assert!(added.status.success(), "{}", describe(&added));
    let file_arg = file.to_str().unwrap();
    let foreign = cli(
        config,
        &[
            "--user",
            "other",
            "profile-import",
            "-i",
            file_arg,
            "--password",
            PASSWORD,
            "--json",
        ],
    );
    assert_eq!(foreign.status.code(), Some(1), "{}", describe(&foreign));
    assert!(
        String::from_utf8_lossy(&foreign.stdout).contains("must be the active one")
            || String::from_utf8_lossy(&foreign.stderr).contains("must be the active one"),
        "{}",
        describe(&foreign)
    );
    let preview_other = json(&cli(
        config,
        &[
            "--user",
            "other",
            "profile-import",
            "-i",
            file_arg,
            "--password",
            PASSWORD,
            "--dry-run",
            "--json",
        ],
    ));
    assert_eq!(preview_other["dry_run"], true, "{preview_other}");
    assert_eq!(saved(config), original, "the refusal wrote nothing");

    let duplicate = import(config, &backup, &["--on-existing", "duplicate"]);
    assert_eq!(duplicate["duplicated"], 1, "{duplicate}");
    assert_eq!(duplicate["similar"], 1, "{duplicate}");
    assert_eq!(duplicate["renamed"][0]["to"], "lab (1)", "{duplicate}");
    let after = saved(config);
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], original[0]);
    assert_eq!(after[1].1, "lab (1)");
    assert_ne!(after[1].0, id);
}

/// Not a test of its own: the other process of the lock test, which holds the
/// lock of `AEROFTP_TEST_LOCK_DB` until it is killed.
#[test]
#[ignore = "started by the lock test"]
fn lock_holder_process() {
    let Ok(db) = std::env::var("AEROFTP_TEST_LOCK_DB") else {
        return;
    };
    let _lock = ftp_client_gui_lib::user_partitions::ProfileListLock::acquire_within(
        Path::new(&db),
        Duration::from_secs(30),
    )
    .unwrap();
    println!("holding the lock");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(300));
}

/// Two real processes: while one holds the lock the other gives up within its
/// bounded wait, a holder killed outright leaves no lock behind, and another
/// config directory never waited on it.
#[test]
fn the_profile_list_lock_holds_across_processes_and_dies_with_its_holder() {
    use ftp_client_gui_lib::user_partitions::{ProfileListLock, PROFILE_LIST_BUSY};

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("a").join("partitions.db");
    let mut holder = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_holder_process", "--ignored", "--nocapture"])
        .env("AEROFTP_TEST_LOCK_DB", &db)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(holder.stdout.take().unwrap()).lines();
    assert!(
        lines.any(|line| line.is_ok_and(|line| line.contains("holding the lock"))),
        "the other process never took the lock"
    );

    let refused = ProfileListLock::acquire_within(&db, Duration::from_millis(300));
    assert_eq!(refused.err().as_deref(), Some(PROFILE_LIST_BUSY));
    let elsewhere = dir.path().join("b").join("partitions.db");
    assert!(ProfileListLock::acquire_within(&elsewhere, Duration::ZERO).is_ok());

    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(ProfileListLock::acquire_within(&db, Duration::from_secs(30)).is_ok());
}

/// Two imports of one id at once, each backup with its own folder and credential:
/// whichever lands last, the list and the secret come from the same backup.
#[tokio::test]
async fn parallel_imports_leave_the_list_and_the_secret_of_one_backup() {
    use ftp_client_gui_lib::profile_export::{export_profiles, import_profiles};

    let config = tempfile::tempdir().unwrap();
    let config = config.path();
    let conf = config.join("rclone.conf");
    std::fs::write(
        &conf,
        "[lab]\ntype = sftp\nhost = sftp.example.invalid\nuser = smoke\n",
    )
    .unwrap();
    json(&cli(
        config,
        &[
            "import",
            "rclone",
            conf.to_str().unwrap(),
            "--apply",
            "--json",
        ],
    ));
    let base = config.join("base.aeroftp");
    let exported = cli(
        config,
        &[
            "profile-export",
            "-o",
            base.to_str().unwrap(),
            "--password",
            PASSWORD,
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));
    let (servers, _, _) = import_profiles(&base, PASSWORD).unwrap();

    let backups: Vec<_> = ["a", "b"]
        .iter()
        .map(|side| {
            let mut server = servers[0].clone();
            server.initial_path = Some(format!("/{side}"));
            server.credential = Some(format!("password-{side}"));
            server.has_stored_credential = Some(true);
            let file = config.join(format!("{side}.aeroftp"));
            export_profiles(vec![server], HashMap::new(), PASSWORD, &file).unwrap();
            file
        })
        .collect();

    let imports: Vec<_> = backups
        .iter()
        .map(|file| {
            let (config, file) = (config.to_path_buf(), file.clone());
            std::thread::spawn(move || {
                cli(
                    &config,
                    &[
                        "profile-import",
                        "-i",
                        file.to_str().unwrap(),
                        "--password",
                        PASSWORD,
                        "--on-existing",
                        "overwrite",
                        "--json",
                    ],
                )
            })
        })
        .collect();
    for import in imports {
        let overwrite = json(&import.join().unwrap());
        assert_eq!(overwrite["overwritten"], 1, "{overwrite}");
    }

    let check = config.join("check.aeroftp");
    let exported = cli(
        config,
        &[
            "profile-export",
            "-o",
            check.to_str().unwrap(),
            "--password",
            PASSWORD,
            "--include-credentials",
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));
    let (stored, _, _) = import_profiles(&check, PASSWORD).unwrap();
    assert_eq!(stored.len(), 1);
    let side = stored[0]
        .initial_path
        .as_deref()
        .and_then(|path| path.strip_prefix('/'))
        .unwrap();
    assert!(["a", "b"].contains(&side), "{side}");
    assert_eq!(
        stored[0].credential.as_deref(),
        Some(format!("password-{side}").as_str())
    );
}

/// The file called `name` somewhere under `dir`.
fn find_file(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == name) {
            Some(path)
        } else if path.is_dir() {
            find_file(&path, name)
        } else {
            None
        }
    })
}

/// An account store that cannot be opened stops the import with what to do next:
/// nothing falls back to the legacy blob, whose list would never reach the store.
#[test]
fn an_unreadable_account_store_refuses_the_import_and_says_what_to_do() {
    let config = tempfile::tempdir().unwrap();
    let config = config.path();
    let conf = config.join("rclone.conf");
    std::fs::write(
        &conf,
        "[lab]\ntype = sftp\nhost = sftp.example.invalid\nuser = smoke\n",
    )
    .unwrap();
    json(&cli(
        config,
        &[
            "import",
            "rclone",
            conf.to_str().unwrap(),
            "--apply",
            "--json",
        ],
    ));
    let file = config.join("profiles.aeroftp");
    let exported = cli(
        config,
        &[
            "profile-export",
            "-o",
            file.to_str().unwrap(),
            "--password",
            PASSWORD,
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));

    let db = find_file(config, "user_partitions.db").expect("the account store");
    std::fs::write(
        &db,
        b"not a database, long enough to be read as a broken header",
    )
    .unwrap();

    let refused = cli(
        config,
        &[
            "profile-import",
            "-i",
            file.to_str().unwrap(),
            "--password",
            PASSWORD,
            "--json",
        ],
    );
    assert_eq!(refused.status.code(), Some(6), "{}", describe(&refused));
    assert!(
        describe(&refused).contains("Check that the AeroFTP data folder is writable"),
        "{}",
        describe(&refused)
    );
}

/// An rclone import writes a crypt remote's secrets before the list transaction,
/// so they also reach the partition row the connect path reads first: written
/// inside it, the row was refused as busy and lost, silently.
/// A token is worth nothing on a fresh device without the app it refreshes with,
/// and two remotes with different apps must not leave the id of one with the
/// secret of the other.
#[test]
fn an_rclone_import_keeps_one_whole_oauth_app() {
    let config = tempfile::tempdir().unwrap();
    let config = config.path();
    let conf = config.join("rclone.conf");
    let token = r#"{"access_token":"a","token_type":"Bearer","refresh_token":"r","expiry":"2030-01-01T00:00:00Z"}"#;
    std::fs::write(
        &conf,
        format!(
            "[one]\ntype = drive\nclient_id = app-one\nclient_secret = secret-one\ntoken = {token}\n\n\
             [two]\ntype = drive\nclient_id = app-two\nclient_secret = secret-two\ntoken = {token}\n"
        ),
    )
    .unwrap();
    json(&cli(
        config,
        &[
            "import",
            "rclone",
            conf.to_str().unwrap(),
            "--apply",
            "--json",
        ],
    ));

    let check = config.join("apps.aeroftp");
    let exported = cli(
        config,
        &[
            "profile-export",
            "-o",
            check.to_str().unwrap(),
            "--password",
            PASSWORD,
            "--include-credentials",
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));
    let (servers, secrets, _) =
        ftp_client_gui_lib::profile_export::import_profiles(&check, PASSWORD).unwrap();
    assert_eq!(servers.len(), 2);
    for server in &servers {
        let app = secrets.get(&server.id).expect("the exported secrets");
        let pair = (
            app.oauth_client_id.as_deref(),
            app.oauth_client_secret.as_deref(),
        );
        assert!(
            pair == (Some("app-one"), Some("secret-one"))
                || pair == (Some("app-two"), Some("secret-two")),
            "a mixed or missing app: {pair:?}"
        );
    }
}

#[test]
fn an_rclone_crypt_import_stores_its_secrets_in_the_partition() {
    let config = tempfile::tempdir().unwrap();
    let config = config.path();
    let conf = config.join("rclone.conf");
    std::fs::write(
        &conf,
        "[base]\ntype = sftp\nhost = sftp.example.invalid\nuser = smoke\n\n\
         [vault]\ntype = crypt\nremote = base:\n\
         password = Z5gL8_HnB9SyJT5RjJtYcEBwiBrSN8h0fHelLigK\n\
         password2 = V98yILELzRn3G-7rg4ymQ3B0X0zgFAbWo74vIapw-YnWja9Jn3k\n",
    )
    .unwrap();
    json(&cli(
        config,
        &[
            "import",
            "rclone",
            conf.to_str().unwrap(),
            "--apply",
            "--json",
        ],
    ));
    let db = find_file(config, "user_partitions.db").expect("the account store");
    let rows: i64 = rusqlite::Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM user_credentials WHERE credential_id LIKE 'aerocrypt_overlay_pw_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);

    // The value, as the export reads it back: the password the remote revealed.
    let check = config.join("check.aeroftp");
    let exported = cli(
        config,
        &[
            "profile-export",
            "-o",
            check.to_str().unwrap(),
            "--password",
            PASSWORD,
            "--include-credentials",
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));
    let (_, secrets, _) =
        ftp_client_gui_lib::profile_export::import_profiles(&check, PASSWORD).unwrap();
    assert!(
        secrets
            .values()
            .any(|s| s.aerocrypt_overlay_pw.as_deref() == Some("crypt-pass-954")),
        "the crypt password was not stored"
    );
    assert!(
        secrets
            .values()
            .any(|s| s.aerocrypt_overlay_salt.as_deref() == Some("hD1lB5uyIChoDFqhaHOsUg")),
        "the crypt salt was not stored"
    );
}
