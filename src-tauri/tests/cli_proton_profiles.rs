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
