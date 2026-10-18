// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A crypt marker write that the server cannot do in one step is refused
//! before anything is written, and the refusal names no way around it. It
//! used the text of a refused file replace, which sends the user to upload
//! over the file with `put`: for a marker there is nothing to upload (the
//! new one exists only inside the command), and a marker rewritten in place
//! and cut short cannot open the vault.
//!
//! The server is `aeroftp-cli serve webdav`. A WebDAV replace sets the old
//! file aside, so it is not one step, and over an existing marker the
//! publish of `crypt migrate-v4` (shared with the slot commands) is refused,
//! naming the opt-in. With `--allow-non-atomic` the marker is replaced
//! through the set-aside, and no copy of the old one is left behind.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

const PASSWORD: &str = "marker refusal probe: correct horse battery staple 2026";

/// A server process stopped on every way out of a test, a failed assertion
/// included: a dropped `Child` keeps running and keeps its port.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Every entry of the vault folder with its bytes, sorted.
fn snapshot(folder: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries: Vec<_> = std::fs::read_dir(folder)
        .expect("vault folder")
        .map(|entry| {
            let path = entry.expect("entry").path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            (name, std::fs::read(&path).unwrap_or_default())
        })
        .collect();
    entries.sort();
    entries
}

#[test]
fn a_marker_write_refused_by_the_server_names_no_way_around_it() {
    let served = tempfile::tempdir().expect("served folder");
    let config = isolated_cli::root();
    let kits = tempfile::tempdir().expect("kit folder");
    let cli = |args: &[&str]| {
        let mut cmd = isolated_cli::command(
            std::path::Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")),
            config.path(),
        );
        cmd.args(args)
            .env("AEROFTP_MASTER_PASSWORD", "marker refusal probe vault")
            .env_remove("AEROFTP_CRYPT_PASSWORD")
            .env_remove("RUST_LOG");
        cmd
    };

    // A v3 headed vault, written straight into the folder the server serves.
    let kit = kits.path().join("kit.txt");
    let init = cli(&["crypt", "init"])
        .arg(served.path())
        .args(["/vault", "--with-header", "--password", PASSWORD])
        .args(["--emergency-kit", kit.to_str().expect("utf-8 path")])
        .stdin(Stdio::null())
        .output()
        .expect("crypt init");
    assert_eq!(init.status.code(), Some(0), "{}", describe(&init));
    let vault = served.path().join("vault");
    let before = snapshot(&vault);
    assert!(
        before.iter().any(|(name, _)| name == ".aerocrypt.tsv"),
        "{before:?}"
    );

    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let server = Server(
        cli(&["serve", "webdav"])
            .arg(served.path())
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("serve webdav"),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "serve webdav did not listen");
        std::thread::sleep(Duration::from_millis(50));
    }

    let host = format!("http://127.0.0.1:{port}/");
    let mut add = cli(&[
        "profile-add",
        "--name",
        "dav",
        "--protocol",
        "webdav",
        "--host",
        &host,
        "--username",
        "u",
        "--password-stdin",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("profile-add");
    add.stdin
        .take()
        .expect("stdin")
        .write_all(b"unused\n")
        .expect("password");
    let added = add.wait_with_output().expect("profile-add");
    assert_eq!(added.status.code(), Some(0), "{}", describe(&added));

    let refused = cli(&[
        "--profile",
        "dav",
        "crypt",
        "migrate-v4",
        "/vault",
        "--password",
        PASSWORD,
    ])
    .stdin(Stdio::null())
    .output()
    .expect("crypt migrate-v4");

    assert_eq!(refused.status.code(), Some(7), "{}", describe(&refused));
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("Nothing was written"), "{stderr}");
    assert!(stderr.contains("crypt marker"), "{stderr}");
    assert!(
        !stderr.contains("`put`"),
        "a marker refusal must not send the user to upload over it: {stderr}"
    );
    assert!(
        stderr.contains("`--allow-non-atomic`"),
        "on a server whose replace sets the old file aside the refusal names the opt-in: \
         {stderr}"
    );
    assert_eq!(
        snapshot(&vault),
        before,
        "the vault folder must be exactly as it was"
    );

    // Policy C (owner, 2026-10-11): with the user's consent the marker is
    // replaced through the set-aside, and nothing of the old one is left.
    let crypt = |args: &[&str]| {
        let out = cli(&["--profile", "dav", "--json", "crypt"])
            .args(args)
            .args(["/vault", "--password", PASSWORD])
            .stdin(Stdio::null())
            .output()
            .expect("crypt command");
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", describe(&out));
        out
    };
    let no_copy_left = |step: &str| {
        let names: Vec<String> = snapshot(&vault).into_iter().map(|(name, _)| name).collect();
        assert!(
            !names
                .iter()
                .any(|name| name.starts_with(".aeroftp-replaced-")),
            "{step} left a set-aside copy: {names:?}"
        );
    };
    crypt(&["migrate-v4", "--allow-non-atomic"]);
    no_copy_left("migrate-v4");
    let marker = std::fs::read_to_string(vault.join(".aerocrypt.tsv")).expect("marker");
    assert!(marker.contains("\"version\":4"), "{marker}");

    crypt(&["add-slot", "--type", "recovery", "--allow-non-atomic"]);
    no_copy_left("add-slot");
    let slots = |out: Output| -> Vec<(u64, String)> {
        let listed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        listed["slots"]
            .as_array()
            .expect("slots")
            .iter()
            .map(|s| {
                (
                    s["id"].as_u64().unwrap(),
                    s["type"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let listed = slots(crypt(&["list-slots"]));
    let recovery = listed
        .iter()
        .find(|(_, kind)| kind == "recovery")
        .map(|(id, _)| id.to_string())
        .expect("a recovery slot after add-slot");

    crypt(&["remove-slot", "--slot-id", &recovery, "--allow-non-atomic"]);
    no_copy_left("remove-slot");
    let listed = slots(crypt(&["list-slots"]));
    assert!(
        listed.iter().all(|(_, kind)| kind != "recovery"),
        "{listed:?}"
    );
    drop(server);
}
