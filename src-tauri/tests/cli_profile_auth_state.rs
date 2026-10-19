// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `auth_state` in the listings agents read first. It was derived from the
//! protocol alone, so a profile that connects with no password saved (here an
//! anonymous FTP site) read `no_credentials`, which tells an agent "not signed
//! in, skip it". Nothing here reaches a server.

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

fn cli(root: &Path, args: &[&str]) -> Output {
    isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run aeroftp-cli")
}

fn json(out: &Output) -> serde_json::Value {
    assert_eq!(out.status.code(), Some(0), "{}", describe(out));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(out)))
}

fn add_ftp(root: &Path, name: &str, username: Option<&str>) -> String {
    let mut args = vec![
        "--json",
        "profile-add",
        "--name",
        name,
        "--protocol",
        "ftp",
        "--host",
        "127.0.0.1",
    ];
    if let Some(username) = username {
        args.extend(["--username", username]);
    }
    json(&cli(root, &args))["id"]
        .as_str()
        .expect("id")
        .to_string()
}

fn state_of(servers: &serde_json::Value, id: &str) -> String {
    servers
        .as_array()
        .expect("servers")
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {servers}"))["auth_state"]
        .as_str()
        .expect("auth_state")
        .to_string()
}

#[test]
fn an_anonymous_site_is_ready_and_a_missing_password_is_not() {
    let root = isolated_cli::root();
    let root = root.path();
    let anonymous = add_ftp(root, "anonymous ftp", None);
    let alice = add_ftp(root, "alice ftp", Some("alice"));

    let profiles = json(&cli(root, &["--json", "profiles"]));
    assert_eq!(state_of(&profiles, &anonymous), "valid");
    assert_eq!(state_of(&profiles, &alice), "no_credentials");

    let bootstrap = json(&cli(root, &["--json", "agent-bootstrap"]));
    let servers = &bootstrap["profiles"]["servers"];
    assert_eq!(state_of(servers, &anonymous), "valid");
    assert_eq!(state_of(servers, &alice), "no_credentials");
}
#[test]
fn manifest_github_auth_modes_reject_server_seeds_before_persistence() {
    let root = isolated_cli::root();
    let root = root.path();
    // Initialize only the private vault. Failed seeding must not add a profile.
    assert!(json(&cli(root, &["--json", "profiles"]))
        .as_array()
        .expect("profiles")
        .is_empty());
    for key in ["githubAuthMode", "github_auth_mode"] {
        for mode in ["app", "authorize"] {
            let options = serde_json::json!({(key): mode}).to_string();
            let out = cli(
                root,
                &[
                    "--json",
                    "profile-add",
                    "--name",
                    "refused GitHub seed",
                    "--protocol",
                    "github",
                    "--options-json",
                    &options,
                    "--credential-json",
                    r#"{"access_token":"synthetic-unused-token"}"#,
                ],
            );
            assert_eq!(out.status.code(), Some(5), "{}", describe(&out));
            assert!(
                String::from_utf8_lossy(&out.stderr)
                    .contains("GitHub App and Device Flow use profile-auth"),
                "{key}={mode}: {}",
                describe(&out)
            );
            assert!(json(&cli(root, &["--json", "profiles"]))
                .as_array()
                .expect("profiles")
                .is_empty());
        }
    }
    let created = json(&cli(
        root,
        &[
            "--json",
            "profile-add",
            "--name",
            "GitHub PAT",
            "--protocol",
            "github",
            "--options-json",
            r#"{"githubAuthMode":"pat"}"#,
            "--credential-json",
            r#"{"access_token":"synthetic-pat"}"#,
        ],
    ));
    let id = created["id"].as_str().expect("id");
    let profiles = json(&cli(root, &["--json", "profiles"]));
    assert_eq!(state_of(&profiles, id), "valid");
    let bootstrap = json(&cli(root, &["--json", "agent-bootstrap"]));
    assert_eq!(state_of(&bootstrap["profiles"]["servers"], id), "valid");
}
