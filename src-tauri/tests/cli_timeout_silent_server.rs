// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A WebDAV server that accepts the connection and never answers held `ls`
//! for the providers' 1800 s read timeout, and the CLI had no flag to wait
//! less. With the global `--timeout`, the command gives up when it passes and
//! reports a timeout (exit 8).

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

#[test]
fn ls_on_a_server_that_never_answers_ends_at_the_global_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a mute listener");
    let port = listener.local_addr().expect("listener address").port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });

    let root = isolated_cli::root();
    let url = format!("webdav://127.0.0.1:{port}/");
    let started = Instant::now();
    let out = isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root.path())
        .args(["--timeout", "2", "ls", &url])
        .output()
        .expect("run aeroftp-cli");
    let elapsed = started.elapsed();

    assert_eq!(
        out.status.code(),
        Some(8),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Without the flag the listing waits 120 s, the connect probe as long.
    assert!(
        elapsed < Duration::from_secs(60),
        "ls took {elapsed:?} with --timeout 2"
    );
}
