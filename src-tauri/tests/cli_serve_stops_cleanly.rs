//! A server asked to stop (SIGTERM, as systemd and Docker ask) ends with
//! status 0 and says nothing about a panic. `serve sftp` panicked instead:
//! its provider runtime was dropped from async code, where tokio refuses to
//! wait for a runtime's threads.

#![cfg(unix)]

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::io::{BufRead, BufReader, Read};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn every_server_stops_cleanly_on_sigterm() {
    for mode in ["http", "webdav", "ftp", "sftp"] {
        let folder = tempfile::tempdir().expect("served folder");
        let config = isolated_cli::root();
        let mut server = isolated_cli::command(
            std::path::Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")),
            config.path(),
        )
        .args(["serve", mode])
        .arg(folder.path())
        .args(["--addr", "127.0.0.1:0"])
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aeroftp-cli serve");

        // Up to "Press Ctrl+C": the server is listening.
        let mut stderr = BufReader::new(server.stderr.take().expect("stderr"));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut banner = String::new();
            loop {
                let mut line = String::new();
                if stderr.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                banner.push_str(&line);
                if line.contains("Press Ctrl+C") {
                    break;
                }
            }
            let mut rest = String::new();
            let ready = banner.contains("Press Ctrl+C");
            let _ = tx.send((ready, banner));
            let _ = stderr.read_to_string(&mut rest);
            let _ = tx.send((ready, rest));
        });
        let (ready, banner) = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the banner");
        assert!(ready, "{mode} did not start: {banner}");

        unsafe {
            libc::kill(server.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = server.try_wait().expect("poll the server") {
                break status;
            }
            if Instant::now() > deadline {
                let _ = server.kill();
                panic!("{mode} did not stop within 30 s of SIGTERM");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let (_, rest) = rx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
        assert!(
            status.success() && !rest.contains("panicked"),
            "{mode} on SIGTERM: {status:?}, stderr after the banner: {rest}"
        );
    }
}
