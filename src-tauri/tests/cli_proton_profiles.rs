// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A Proton Drive profile is created and signed in from the CLI, on either
//! client. `profile-add --protocol proton` was refused as an unknown protocol,
//! so no CLI command made a Proton profile, and the native client signed in
//! only from the GUI. Nothing here reaches Proton: every sign-in case stops
//! before the network, and the sign-out and status have no session to read.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::path::Path;
use std::process::{Output, Stdio};

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The CLI on the isolated vault in `root`, stdin closed and stderr piped (so
/// never a terminal).
fn cli(root: &Path, args: &[&str]) -> Output {
    isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run aeroftp-cli")
}

/// `profile-add --json` for a Proton profile: its id and the client it reports.
fn add_proton(root: &Path, name: &str, mode: Option<&str>) -> (String, String) {
    let mut args = vec![
        "--json",
        "profile-add",
        "--name",
        name,
        "--protocol",
        "proton",
    ];
    if let Some(mode) = mode {
        args.extend(["--proton-mode", mode]);
    }
    let out = cli(root, &args);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let added: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(&out)));
    (
        added["id"].as_str().expect("id").to_string(),
        added["proton_mode"]
            .as_str()
            .expect("proton_mode")
            .to_string(),
    )
}

#[test]
fn profile_add_saves_the_client_and_sign_in_reads_it_back() {
    let root = isolated_cli::root();
    let root = root.path();

    let (cli_id, cli_mode) = add_proton(root, "Proton CLI", Some("cli"));
    assert_eq!(cli_mode, "cli");
    // The sign-in reads the saved profile: a CLI profile is sent to the
    // official CLI's own sign-in.
    let refused = cli(root, &["proton", "sign-in", &cli_id]);
    assert_eq!(refused.status.code(), Some(5), "{}", describe(&refused));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("proton-drive auth login"),
        "{}",
        describe(&refused)
    );

    let (default_id, default_mode) = add_proton(root, "Proton default", None);
    if cfg!(feature = "proton-native") {
        assert_eq!(default_mode, "native", "the GUI form opens on its API tab");
        let (native_id, native_mode) = add_proton(root, "Proton API", Some("native"));
        assert_eq!(native_mode, "native");
        for id in [&native_id, &default_id] {
            // Accepted as native, then stopped before the network: a
            // sign-in address may only go to a terminal, and stderr is not one.
            let out = cli(root, &["proton", "sign-in", id, "--no-browser"]);
            assert_eq!(out.status.code(), Some(5), "{}", describe(&out));
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(text.contains("--no-browser"), "{}", describe(&out));
            assert!(!text.contains("https://"), "{}", describe(&out));
        }
        // No session saved yet: the sign-out has nothing to end and says so.
        let out = cli(root, &["--json", "proton", "sign-out", &native_id]);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        let report: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", describe(&out)));
        assert_eq!(report["signed_out"], serde_json::json!(false), "{report}");
        let out = cli(root, &["--json", "proton", "status", &native_id]);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        let status: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", describe(&out)));
        assert_eq!(status["signed_in"], serde_json::json!(false), "{status}");
    } else {
        assert_eq!(default_mode, "cli", "a build without the native client");
        let out = cli(
            root,
            &[
                "profile-add",
                "--name",
                "x",
                "--protocol",
                "proton",
                "--proton-mode",
                "native",
            ],
        );
        assert_eq!(out.status.code(), Some(5), "{}", describe(&out));
    }
}

/// An rclone import restores the session of a `protondrive` remote in the
/// active account only while no other account on this device holds it:
/// imported again into a second account, the profile is reported to sign in
/// again and holds no session, and the first account keeps its own. Nothing
/// reaches Proton: no sign-in, no sign-out.
#[cfg(feature = "proton-native")]
#[test]
fn an_rclone_import_into_a_second_account_leaves_the_session_to_the_first() {
    let root = isolated_cli::root();
    let root = root.path();
    let conf = root.join("rclone.conf");
    std::fs::write(
        &conf,
        "[pd]\ntype = protondrive\nusername = someone@example.test\n\
         client_uid = rclone-uid\nclient_access_token = access-1\n\
         client_refresh_token = refresh-1\nclient_salted_key_pass = a2V5LXBhc3N3b3JkLTE=\n",
    )
    .unwrap();
    let import = || {
        let out = cli(
            root,
            &[
                "import",
                "rclone",
                conf.to_str().unwrap(),
                "--apply",
                "--json",
            ],
        );
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        let imported = json(&out);
        let id = imported["servers"][0]["id"]
            .as_str()
            .expect("id")
            .to_string();
        (id, imported)
    };
    let signed_in = |id: &str| {
        let out = cli(root, &["--json", "proton", "status", id]);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        json(&out)["signed_in"].clone()
    };
    let switch = |user: &str| {
        let out = cli(root, &["users", "switch", user]);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    };

    // Whether the import's `warnings` say a session is now shared with rclone.
    let says_shared = |imported: &serde_json::Value| {
        imported["warnings"].as_array().is_some_and(|warnings| {
            warnings.iter().any(|warning| {
                warning["reason"] == ftp_client_gui_lib::rclone_import::RCLONE_SESSION_SHARED
            })
        })
    };
    let (first_id, imported) = import();
    assert_eq!(signed_in(&first_id), serde_json::json!(true), "imported");
    assert!(
        says_shared(&imported),
        "a stored session is reported shared: {imported}"
    );
    let listed = json(&cli(root, &["--json", "users", "list"]));
    let first_user = listed["users"][0]["name"]
        .as_str()
        .expect("the account that imported first")
        .to_string();
    let added = cli(root, &["users", "add", "second"]);
    assert_eq!(added.status.code(), Some(0), "{}", describe(&added));
    switch("second");

    let (second_id, imported) = import();
    let warnings = imported["appliedSummary"]["signInWarnings"].clone();
    // The reason is that another account may hold the session, never another
    // failure that names the profile too.
    assert!(
        warnings
            .as_array()
            .is_some_and(
                |warnings| warnings
                    .iter()
                    .any(|warning| warning.as_str().is_some_and(|warning| warning
                        .contains(&second_id)
                        && warning.contains(
                            ftp_client_gui_lib::providers::proton_native::vault::IMPORT_WITHHELD
                        )))
            ),
        "the profile is reported to sign in again: {imported}"
    );
    assert!(
        !says_shared(&imported),
        "a session left out is not reported imported: {imported}"
    );
    assert_eq!(
        signed_in(&second_id),
        serde_json::json!(false),
        "an rclone import does not bring a session another account holds"
    );
    switch(&first_user);
    assert_eq!(signed_in(&first_id), serde_json::json!(true));
}

#[test]
fn proton_mode_and_sign_in_refuse_what_is_not_a_proton_profile() {
    let root = isolated_cli::root();
    let root = root.path();

    let out = cli(
        root,
        &[
            "profile-add",
            "--name",
            "ftp",
            "--protocol",
            "ftp",
            "--proton-mode",
            "cli",
        ],
    );
    assert_eq!(out.status.code(), Some(5), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("only valid for --protocol proton"),
        "{}",
        describe(&out)
    );

    let out = cli(
        root,
        &[
            "profile-add",
            "--name",
            "ftp",
            "--protocol",
            "ftp",
            "--host",
            "127.0.0.1",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let out = cli(root, &["proton", "sign-in", "ftp"]);
    assert_eq!(out.status.code(), Some(5), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a Proton Drive profile"),
        "{}",
        describe(&out)
    );

    let out = cli(root, &["proton", "sign-out", "no such profile"]);
    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
}

/// The Proton alias keeps the manifest options and the explicitly chosen client
/// once saved, as an export of the profile shows.
#[tokio::test]
async fn proton_alias_preserves_manifest_options_and_explicit_client_after_saving() {
    let root = isolated_cli::root();
    let root = root.path();
    let out = cli(
        root,
        &[
            "--json",
            "profile-add",
            "--name",
            "alias options",
            "--protocol",
            "protondrive",
            "--proton-mode",
            "cli",
            "--options-json",
            r#"{"protonMode":"native","saveSession":true}"#,
            "--manual-total",
            "123456",
            "--initial-path",
            "/synthetic",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let added: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(added["protocol"], "proton");
    assert_eq!(added["proton_mode"], "cli");

    let text = cli(
        root,
        &[
            "profile-add",
            "--name",
            "alias text",
            "--protocol",
            "protondrive",
            "--proton-mode",
            "cli",
        ],
    );
    assert_eq!(text.status.code(), Some(0), "{}", describe(&text));
    assert!(
        String::from_utf8_lossy(&text.stderr).contains("proton-drive auth login"),
        "{}",
        describe(&text)
    );

    let export = root.join("alias.aeroftp");
    let out = isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
        .args(["profile-export", "--output", export.to_str().unwrap()])
        .env("AEROFTP_PROFILE_PASSWORD", "synthetic alias export")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let preview = ftp_client_gui_lib::preview_server_profiles(
        &export.to_string_lossy(),
        "synthetic alias export",
        ftp_client_gui_lib::profile_import_plan::ExistingProfileMode::Skip,
        &ftp_client_gui_lib::profile_security::RelaxationPolicy::KeepAll,
        &[],
    )
    .unwrap();
    let saved = preview
        .servers
        .iter()
        .find(|profile| profile["id"] == added["id"])
        .unwrap();
    assert_eq!(saved["protocol"], "proton");
    assert_eq!(saved["initialPath"], "/synthetic");
    let options = saved["options"].as_object().unwrap();
    assert_eq!(options["proton_mode"], "cli");
    assert!(!options.contains_key("protonMode"));
    assert_eq!(
        options
            .get("saveSession")
            .or_else(|| options.get("save_session"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(options["manualTotalBytes"], 123456);
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(out)))
}

/// The saved profile `id` whole, options included, read back through an
/// export (`profiles --json` lists no options).
async fn saved(root: &Path, id: &str) -> serde_json::Value {
    let export = root.join(format!("{}.aeroftp", uuid::Uuid::new_v4()));
    let out = isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
        .args(["profile-export", "--output", export.to_str().unwrap()])
        .env("AEROFTP_PROFILE_PASSWORD", "synthetic switch export")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let preview = ftp_client_gui_lib::preview_server_profiles(
        &export.to_string_lossy(),
        "synthetic switch export",
        ftp_client_gui_lib::profile_import_plan::ExistingProfileMode::Skip,
        &ftp_client_gui_lib::profile_security::RelaxationPolicy::KeepAll,
        &[],
    )
    .unwrap();
    preview
        .servers
        .iter()
        .find(|profile| profile["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} not exported"))
}

/// Requested 79: `profile-convert-mode` switches a Proton Drive profile
/// between its clients in place (same id) and moves its initial folder to the
/// other spelling of My Files, in both directions; `profile-duplicate-mode`
/// adds a copy with the folder translated and keeps the source; `pwd` gives
/// My Files as the start folder of a CLI-mode profile that saves no folder.
#[tokio::test]
async fn proton_mode_switch_translates_the_initial_folder_both_ways() {
    let root = isolated_cli::root();
    let root = root.path();
    let out = cli(
        root,
        &[
            "--json",
            "profile-add",
            "--name",
            "Switch",
            "--protocol",
            "proton",
            "--proton-mode",
            "cli",
            "--initial-path",
            "/my-files/docs",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let id = json(&out)["id"].as_str().unwrap().to_string();

    let native = cli(
        root,
        &[
            "--json",
            "profile-convert-mode",
            &id,
            "--mode",
            "native",
            "--yes",
        ],
    );
    assert_eq!(native.status.code(), Some(0), "{}", describe(&native));
    let report = json(&native);
    assert_eq!(report["id"], id.as_str());
    assert_eq!(report["translated"][0]["from"], "/my-files/docs");
    assert_eq!(report["translated"][0]["to"], "/docs");
    let profile = saved(root, &id).await;
    assert_eq!(profile["initialPath"], "/docs");
    assert_eq!(profile["options"]["proton_mode"], "native");

    let back = cli(
        root,
        &[
            "--json",
            "profile-convert-mode",
            &id,
            "--mode",
            "cli",
            "--yes",
        ],
    );
    assert_eq!(back.status.code(), Some(0), "{}", describe(&back));
    let profile = saved(root, &id).await;
    assert_eq!(profile["initialPath"], "/my-files/docs");
    assert!(profile["options"].get("proton_mode").is_none());

    // A section with no native equivalent is kept and reported, not dropped.
    let shared = cli(
        root,
        &[
            "--json",
            "profile-add",
            "--name",
            "Shared",
            "--protocol",
            "proton",
            "--proton-mode",
            "cli",
            "--initial-path",
            "/shared-with-me/team",
        ],
    );
    let shared_id = json(&shared)["id"].as_str().unwrap().to_string();
    let copy = cli(
        root,
        &[
            "--json",
            "profile-duplicate-mode",
            &shared_id,
            "--mode",
            "native",
        ],
    );
    assert_eq!(copy.status.code(), Some(0), "{}", describe(&copy));
    let copy = json(&copy);
    assert_eq!(
        copy["without_equivalent"][0]["from"],
        "/shared-with-me/team"
    );
    let copied = saved(root, copy["id"].as_str().unwrap()).await;
    assert_eq!(copied["initialPath"], "/shared-with-me/team");
    assert_eq!(copied["options"]["proton_mode"], "native");
    assert_eq!(
        saved(root, &shared_id).await["initialPath"],
        "/shared-with-me/team"
    );

    // Native into a new CLI copy: `/docs` becomes `/my-files/docs`.
    let copy = cli(
        root,
        &["--json", "profile-duplicate-mode", &id, "--mode", "native"],
    );
    let native_copy = json(&copy)["id"].as_str().unwrap().to_string();
    let cli_copy = cli(
        root,
        &[
            "--json",
            "profile-duplicate-mode",
            &native_copy,
            "--mode",
            "cli",
        ],
    );
    assert_eq!(cli_copy.status.code(), Some(0), "{}", describe(&cli_copy));
    let cli_copy = saved(root, json(&cli_copy)["id"].as_str().unwrap()).await;
    assert_eq!(cli_copy["initialPath"], "/my-files/docs");

    // pwd: a CLI-mode profile without a folder starts in My Files.
    let bare = cli(
        root,
        &[
            "--json",
            "profile-add",
            "--name",
            "Bare",
            "--protocol",
            "proton",
            "--proton-mode",
            "cli",
        ],
    );
    let bare_id = json(&bare)["id"].as_str().unwrap().to_string();
    let pwd = cli(root, &["--json", "pwd", &bare_id]);
    assert_eq!(pwd.status.code(), Some(0), "{}", describe(&pwd));
    // The root stays the sections; relative paths start in My Files.
    assert_eq!(json(&pwd)["resolved_root"], "/");
    assert_eq!(json(&pwd)["start_folder"], "/my-files");
}

/// Whether a directory named `name` exists anywhere under `dir`.
#[cfg(feature = "proton-native")]
fn has_directory(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let path = entry.path();
            path.is_dir() && (entry.file_name() == name || has_directory(&path, name))
        })
    })
}

/// The delete of a profile that is not a Proton one takes no Proton session
/// lock (a filesystem without file locks would refuse that lock, and with it
/// the delete), while a native Proton profile's delete still goes through the
/// session's lock to end its session.
#[cfg(feature = "proton-native")]
#[test]
fn a_profile_delete_takes_a_proton_session_lock_only_for_proton() {
    let root = isolated_cli::root();
    let root = root.path();
    let out = cli(
        root,
        &[
            "--json",
            "profile-add",
            "--name",
            "plain",
            "--protocol",
            "ftp",
            "--host",
            "127.0.0.1",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let out = cli(root, &["profile-delete", "plain", "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    assert!(
        !has_directory(root, "proton_session_locks"),
        "an FTP profile's delete takes no Proton session lock"
    );

    // The native profile's session lock, held here: the delete waits for it
    // (up to 45 s) and goes on once it is released.
    let (native_id, _) = add_proton(root, "native", Some("native"));
    let data = find_file(root, "user_partitions.db")
        .expect("the account database")
        .parent()
        .unwrap()
        .to_path_buf();
    let locks = data.join("proton_session_locks");
    std::fs::create_dir_all(&locks).unwrap();
    let lock = {
        use sha2::{Digest, Sha256};
        let key = format!("proton_native_session_{native_id}");
        locks.join(format!(
            "{}.lock",
            hex::encode(Sha256::digest(key.as_bytes()))
        ))
    };
    let held = std::fs::File::create(&lock).unwrap();
    held.try_lock()
        .expect("the session lock is free before the delete");
    let mut delete = isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
        .args(["profile-delete", &native_id, "--yes"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run aeroftp-cli");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        delete.try_wait().unwrap().is_none(),
        "a native Proton profile's delete waits for its session lock"
    );
    assert!(
        listed_ids(root).contains(&native_id),
        "the profile stays saved while its session lock is held"
    );
    held.unlock().unwrap();
    let out = delete.wait_with_output().expect("aeroftp-cli output");
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    assert!(
        !listed_ids(root).contains(&native_id),
        "the delete goes on once the lock is released"
    );
}

/// The ids `profiles --json` lists.
fn listed_ids(root: &Path) -> Vec<String> {
    let listed = json(&cli(root, &["profiles", "--json"]));
    listed
        .get("profiles")
        .and_then(|p| p.as_array())
        .or_else(|| listed.as_array())
        .unwrap_or_else(|| panic!("no profile list in {listed}"))
        .iter()
        .filter_map(|profile| profile["id"].as_str().map(str::to_string))
        .collect()
}

/// The first file named `name` under `dir`, at any depth.
fn find_file(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        if path.is_dir() {
            find_file(&path, name)
        } else {
            (entry.file_name() == name).then_some(path)
        }
    })
}

/// A profile delete whose credential row cannot be removed says so: the
/// profile leaves the list, and the secret still saved under its id is named,
/// instead of a plain "deleted" over a row every reader still takes first.
#[test]
fn a_profile_delete_names_the_secret_it_could_not_remove() {
    let root = isolated_cli::root();
    let root = root.path();
    let mut add = isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
        .args([
            "--json",
            "profile-add",
            "--name",
            "kept-secret",
            "--protocol",
            "ftp",
            "--host",
            "127.0.0.1",
            "--password-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run aeroftp-cli");
    {
        use std::io::Write;
        add.stdin.take().unwrap().write_all(b"hunter2\n").unwrap();
    }
    let out = add.wait_with_output().expect("aeroftp-cli output");
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let added: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(&out)));
    let id = added["id"].as_str().expect("id").to_string();
    let db = find_file(root, "user_partitions.db").expect("the account database");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER keep_rows BEFORE DELETE ON user_credentials
             BEGIN SELECT RAISE(FAIL, 'disk full'); END;",
        )
        .unwrap();
    let out = cli(root, &["--json", "profile-delete", &id, "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let deleted: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(&out)));
    assert_eq!(deleted["status"], "deleted", "{}", describe(&out));
    assert_eq!(
        deleted["secrets_not_removed"],
        serde_json::json!([format!("server_{id}")]),
        "{}",
        describe(&out)
    );
    assert!(
        !listed_ids(root).contains(&id),
        "the profile leaves the list"
    );
    let rows: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM user_credentials WHERE credential_id = 'server_' || ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        rows, 1,
        "the row the delete could not remove is still there"
    );
}
