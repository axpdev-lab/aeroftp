// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A server that accepts the connection and never answers held a command for
//! the provider's own limit: 1800 s of silence on a WebDAV listing, 60 s on a
//! SITE reply, and the CLI had no global flag to wait less. With `--timeout`,
//! each command gives up when it passes and reports a timeout (exit 8).

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Past this the test stops the CLI itself, so a regression fails the test
/// instead of holding it for the provider's own limit.
const DEADLINE: Duration = Duration::from_secs(90);

/// Read `pipe` to its end on a thread of its own.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// Run the CLI with `args`, kill it if it passes [`DEADLINE`], and return
/// its output with how long it ran. Its stdout and stderr are drained while
/// it runs, so a child that writes more than a pipe holds is not blocked.
fn run_bounded(args: &[&str]) -> (Output, Duration) {
    let root = isolated_cli::root();
    let started = Instant::now();
    let mut child =
        isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root.path())
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn aeroftp-cli");
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    while child.try_wait().expect("poll aeroftp-cli").is_none() {
        if started.elapsed() >= DEADLINE {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let status = child.wait().expect("reap aeroftp-cli");
    let out = Output {
        status,
        stdout: stdout.join().expect("drain stdout"),
        stderr: stderr.join().expect("drain stderr"),
    };
    (out, started.elapsed())
}

fn describe(out: &Output, elapsed: Duration) -> String {
    format!(
        "after {elapsed:?}, exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

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

    let url = format!("webdav://127.0.0.1:{port}/");
    let (out, elapsed) = run_bounded(&["--timeout", "2", "ls", &url]);
    assert_eq!(out.status.code(), Some(8), "{}", describe(&out, elapsed));
    // Without the flag the listing waits 120 s, the connect probe as long.
    assert!(
        elapsed < Duration::from_secs(60),
        "{}",
        describe(&out, elapsed)
    );
}

/// An FTP server that logs anyone in and then never answers a SITE command,
/// holding the connection open.
fn ftp_server_silent_on_site() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an FTP listener");
    let port = listener.local_addr().expect("listener address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut write = stream.try_clone().expect("clone the control stream");
                let _ = write.write_all(b"220 Test FTP ready\r\n");
                let mut held = Vec::new();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    let verb = line
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase();
                    let reply: &[u8] = match verb.as_str() {
                        "SITE" => {
                            held.push(line);
                            continue;
                        }
                        "USER" => b"331 Password required\r\n",
                        "PASS" => b"230 Logged in\r\n",
                        "AUTH" => b"502 Not implemented\r\n",
                        "FEAT" => b"211-Features:\r\n UTF8\r\n211 End\r\n",
                        "PWD" => b"257 \"/\" is the current directory\r\n",
                        "QUIT" => {
                            let _ = write.write_all(b"221 Goodbye\r\n");
                            return;
                        }
                        _ => b"200 Ok\r\n",
                    };
                    if write.write_all(reply).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

#[test]
fn site_whose_reply_never_comes_ends_at_the_global_timeout() {
    let port = ftp_server_silent_on_site();
    let url = format!("ftp://test:test@127.0.0.1:{port}/");
    let (out, elapsed) = run_bounded(&["--timeout", "2", "site", &url, "WHO"]);
    assert_eq!(out.status.code(), Some(8), "{}", describe(&out, elapsed));
    // Two seconds of reply wait plus start-up and login; without the flag the
    // wait is 60 s.
    assert!(
        elapsed < Duration::from_secs(10),
        "{}",
        describe(&out, elapsed)
    );
}
