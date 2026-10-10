// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `crypt init` refuses to finish without the Emergency Kit: a path for it
//! (`--emergency-kit`), or, at a terminal, a YES after the kit is printed.
//! That gate ran after the overlay config was written, so a run it refused
//! left a marker on the server (or a config in the keystore) for a vault
//! whose kit nobody had, and the next run, with the kit, was refused as
//! "already exists" unless it passed `--force`. Nothing is written now until
//! the kit has somewhere to go.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const PASSWORD: &str = "kit gate probe: correct horse battery staple 2026";

/// `aeroftp-cli crypt init <remote> /vault --with-header` with every config
/// directory isolated and stdin closed, so the run is non-interactive.
fn crypt_init(remote: &Path, config: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["crypt", "init"])
        .arg(remote)
        .args(["/vault", "--with-header", "--password", PASSWORD])
        .args(extra)
        .env("XDG_CONFIG_HOME", config)
        .env("HOME", config)
        .env("APPDATA", config)
        .env("LOCALAPPDATA", config)
        .env_remove("AEROFTP_CRYPT_PASSWORD")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run aeroftp-cli")
}

fn marker(remote: &Path) -> std::path::PathBuf {
    remote.join("vault").join(".aerocrypt.tsv")
}

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn an_init_without_a_kit_path_writes_nothing_and_the_next_run_needs_no_force() {
    let remote = tempfile::tempdir().expect("remote folder");
    let config = tempfile::tempdir().expect("isolated config");

    for extra in [&[][..], &["--format", "json"][..]] {
        let refused = crypt_init(remote.path(), config.path(), extra);
        assert_eq!(refused.status.code(), Some(5), "{}", describe(&refused));
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&refused.stdout),
            String::from_utf8_lossy(&refused.stderr)
        );
        assert!(said.contains("--emergency-kit"), "{}", describe(&refused));
        assert!(
            !remote.path().join("vault").exists(),
            "a refused init left {:?} on the remote",
            std::fs::read_dir(remote.path().join("vault"))
                .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
        );
    }

    let kit = config.path().join("kit.txt");
    let done = crypt_init(
        remote.path(),
        config.path(),
        &["--emergency-kit", kit.to_str().expect("utf-8 path")],
    );
    assert_eq!(done.status.code(), Some(0), "{}", describe(&done));
    assert!(marker(remote.path()).is_file());
}

#[test]
fn an_init_whose_kit_path_cannot_be_written_writes_nothing() {
    let remote = tempfile::tempdir().expect("remote folder");
    let config = tempfile::tempdir().expect("isolated config");

    let unwritable = [
        config.path().join("no-such-folder").join("kit.txt"),
        // A folder, not a file.
        config.path().to_path_buf(),
    ];
    for kit in unwritable {
        let refused = crypt_init(
            remote.path(),
            config.path(),
            &["--emergency-kit", kit.to_str().expect("utf-8 path")],
        );
        assert_ne!(refused.status.code(), Some(0), "{}", describe(&refused));
        assert!(
            !remote.path().join("vault").exists(),
            "an init refused for kit path {kit:?} left the vault folder on the remote: {}",
            describe(&refused)
        );
    }
}

#[test]
fn the_kit_an_init_writes_describes_the_marker_it_wrote() {
    let remote = tempfile::tempdir().expect("remote folder");
    let config = tempfile::tempdir().expect("isolated config");
    let kit_folder = tempfile::tempdir().expect("kit folder");
    let kit = kit_folder.path().join("kit.txt");

    let done = crypt_init(
        remote.path(),
        config.path(),
        &["--emergency-kit", kit.to_str().expect("utf-8 path")],
    );
    assert_eq!(done.status.code(), Some(0), "{}", describe(&done));

    let marker = std::fs::read_to_string(marker(remote.path())).expect("marker");
    let kit = std::fs::read_to_string(&kit).expect("kit");
    let report = ftp_client_gui_lib::aerocrypt::emergency_kit::verify_against_active(&marker, &kit)
        .expect("verify");
    assert!(report.ok, "{report:?}");
    // Only the kit is left in its folder: nothing staged on the way.
    let left: Vec<_> = std::fs::read_dir(kit_folder.path())
        .expect("kit folder")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["kit.txt"]);
}

/// `--keyfile-gen` used to write its keyfile before any check, so a refused
/// run left a keyfile for a vault that does not exist, and the same command
/// run again was refused because the keyfile was already there.
#[test]
fn a_refused_init_leaves_no_generated_keyfile_and_no_kit() {
    let remote = tempfile::tempdir().expect("remote folder");
    let config = tempfile::tempdir().expect("isolated config");
    let kits = tempfile::tempdir().expect("kit folder");
    let keyfile = kits.path().join("vault.key");
    let keyfile_arg = keyfile.to_str().expect("utf-8 path");

    let no_kit = crypt_init(
        remote.path(),
        config.path(),
        &["--keyfile-gen", keyfile_arg],
    );
    assert_eq!(no_kit.status.code(), Some(5), "{}", describe(&no_kit));
    assert!(!keyfile.exists(), "{}", describe(&no_kit));

    let first_kit = kits.path().join("first.txt");
    let first = crypt_init(
        remote.path(),
        config.path(),
        &["--emergency-kit", first_kit.to_str().expect("utf-8 path")],
    );
    assert_eq!(first.status.code(), Some(0), "{}", describe(&first));

    let second_kit = kits.path().join("second.txt");
    let again = crypt_init(
        remote.path(),
        config.path(),
        &[
            "--keyfile-gen",
            keyfile_arg,
            "--emergency-kit",
            second_kit.to_str().expect("utf-8 path"),
        ],
    );
    assert_eq!(again.status.code(), Some(9), "{}", describe(&again));
    let mut left: Vec<_> = std::fs::read_dir(kits.path())
        .expect("kit folder")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["first.txt"], "{}", describe(&again));
}

/// A headerless init keeps its config in the local keystore, under the
/// profile: a refused run must not leave it there either. The profile is a
/// WebDAV one, served from a folder by `aeroftp-cli serve webdav`.
#[test]
fn a_refused_headerless_init_leaves_no_config_in_the_keystore() {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let served = tempfile::tempdir().expect("served folder");
    let config = tempfile::tempdir().expect("isolated config");
    let kits = tempfile::tempdir().expect("kit folder");
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let cli = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"));
        cmd.args(args)
            .env("XDG_CONFIG_HOME", config.path())
            .env("HOME", config.path())
            .env("APPDATA", config.path())
            .env("LOCALAPPDATA", config.path())
            .env("AEROFTP_MASTER_PASSWORD", "kit gate probe vault")
            .env_remove("AEROFTP_CRYPT_PASSWORD")
            .env_remove("RUST_LOG");
        cmd
    };

    let mut server = cli(&["serve", "webdav"])
        .arg(served.path())
        .args(["--addr", &format!("127.0.0.1:{port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("serve webdav");
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

    let init = [
        "--profile",
        "dav",
        "crypt",
        "init",
        "/vault",
        "--password",
        PASSWORD,
    ];
    let refused = cli(&init).stdin(Stdio::null()).output().expect("init");
    assert_eq!(refused.status.code(), Some(5), "{}", describe(&refused));

    let kit = kits.path().join("kit.txt");
    let done = cli(&init)
        .args(["--emergency-kit", kit.to_str().expect("utf-8 path")])
        .stdin(Stdio::null())
        .output()
        .expect("init");
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(done.status.code(), Some(0), "{}", describe(&done));
    assert!(kit.is_file());
}

/// At a terminal, without a path, the kit is printed and init asks for YES.
/// Answering anything else must leave the remote as it was.
#[cfg(unix)]
#[test]
fn declining_the_kit_at_a_terminal_writes_nothing() {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let remote = tempfile::tempdir().expect("remote folder");
    let config = tempfile::tempdir().expect("isolated config");

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 50,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_aeroftp-cli"));
    cmd.args(["crypt", "init"]);
    cmd.arg(remote.path());
    cmd.args(["/vault", "--with-header", "--password", PASSWORD]);
    for key in ["XDG_CONFIG_HOME", "HOME"] {
        cmd.env(key, config.path());
    }
    cmd.env_remove("AEROFTP_CRYPT_PASSWORD");
    cmd.env_remove("RUST_LOG");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn aeroftp-cli");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    while !seen.contains("Type YES") {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(chunk) => seen.push_str(&String::from_utf8_lossy(&chunk)),
            Err(_) => panic!("no YES prompt within 120s; the terminal showed:\n{seen}"),
        }
    }
    writer.write_all(b"no\r").expect("answer the prompt");
    writer.flush().expect("flush the answer");

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "init did not exit after the answer"
        );
        while let Ok(chunk) = rx.try_recv() {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.exit_code(), 5, "{seen}");
    assert!(
        !remote.path().join("vault").exists(),
        "a declined kit left the vault folder on the remote; the terminal showed:\n{seen}"
    );
}
