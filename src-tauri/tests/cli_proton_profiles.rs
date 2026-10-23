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
    let preview = ftp_client_gui_lib::import_server_profiles_core_filtered(
        export.to_string_lossy().into_owned(),
        "synthetic alias export".into(),
        Some(std::collections::HashSet::new()),
    )
    .await
    .unwrap();
    let saved = preview["servers"]
        .as_array()
        .unwrap()
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
    let preview = ftp_client_gui_lib::import_server_profiles_core_filtered(
        export.to_string_lossy().into_owned(),
        "synthetic switch export".into(),
        Some(std::collections::HashSet::new()),
    )
    .await
    .unwrap();
    preview["servers"]
        .as_array()
        .unwrap()
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
