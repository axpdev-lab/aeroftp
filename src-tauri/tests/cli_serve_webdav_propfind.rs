// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `aeroftp-cli serve webdav` answered a PROPFIND on a path that does not
//! exist with `207` and a `200 OK` collection, because a `stat` that found
//! nothing was taken for "a folder this backend cannot stat". Every client
//! then saw any missing path as an existing folder: `aeroftp-cli stat`
//! reported a missing file as a directory, and `crypt init --with-header`
//! was refused as "already exists". A missing path is now `404`, as RFC 4918
//! section 9.1 wants, and as `GET` on the same server already answered.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long any one step may take: the server coming up, a PROPFIND with its
/// body, a CLI run. A server that accepts a connection and never answers must
/// fail the test, not hang it.
const LIMIT: Duration = Duration::from_secs(30);

struct Served {
    child: Child,
    port: u16,
    _folder: tempfile::TempDir,
    config: tempfile::TempDir,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Serve a folder holding `f.txt`, `docs/a.txt` and an empty `empty/`.
fn serve() -> Served {
    let folder = tempfile::tempdir().expect("served folder");
    std::fs::write(folder.path().join("f.txt"), b"file").expect("f.txt");
    std::fs::create_dir(folder.path().join("docs")).expect("docs");
    std::fs::write(folder.path().join("docs").join("a.txt"), b"a").expect("a.txt");
    std::fs::create_dir(folder.path().join("empty")).expect("empty");
    let config = tempfile::tempdir().expect("isolated config");
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let child = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["serve", "webdav"])
        .arg(folder.path())
        .args(["--addr", &format!("127.0.0.1:{port}")])
        .env("XDG_CONFIG_HOME", config.path())
        .env("HOME", config.path())
        .env("APPDATA", config.path())
        .env("LOCALAPPDATA", config.path())
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("serve webdav");
    let deadline = Instant::now() + LIMIT;
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "serve webdav did not listen");
        std::thread::sleep(Duration::from_millis(50));
    }
    Served {
        child,
        port,
        _folder: folder,
        config,
    }
}

async fn propfind(port: u16, path: &str, depth: &str) -> (u16, String) {
    let response = reqwest::Client::builder()
        // A total timeout: connection, request and the whole response body.
        .timeout(LIMIT)
        .build()
        .expect("client")
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").expect("method"),
            format!("http://127.0.0.1:{port}{path}"),
        )
        .header("Depth", depth)
        .send()
        .await
        .expect("PROPFIND");
    let status = response.status().as_u16();
    (status, response.text().await.expect("body"))
}

/// Read a pipe to its end on a thread of its own.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// `Command::output`, bounded by `LIMIT`: a child still running then is
/// killed and the test fails with what it printed so far.
fn output_within_limit(command: &mut Command, what: &str) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    // Both pipes are read while waiting, so a child that fills one is not
    // taken for a hung one.
    let stdout = drain(child.stdout.take().expect("stdout"));
    let stderr = drain(child.stderr.take().expect("stderr"));
    let deadline = Instant::now() + LIMIT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "{what} did not finish in {} s\nstdout: {}\nstderr: {}",
                LIMIT.as_secs(),
                String::from_utf8_lossy(&stdout.join().unwrap_or_default()),
                String::from_utf8_lossy(&stderr.join().unwrap_or_default())
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    Output {
        status,
        stdout: stdout.join().expect("stdout"),
        stderr: stderr.join().expect("stderr"),
    }
}

#[tokio::test]
async fn a_propfind_on_a_missing_path_is_not_found() {
    let served = serve();
    for (path, depth) in [
        ("/missing.txt", "0"),
        ("/missing/", "0"),
        ("/missing/", "1"),
        ("/missing/inner.txt", "0"),
        ("/docs/missing.txt", "0"),
    ] {
        let (status, body) = propfind(served.port, path, depth).await;
        assert_eq!(status, 404, "PROPFIND {path} Depth {depth}: {body}");
    }
}

#[tokio::test]
async fn a_propfind_still_describes_what_is_there() {
    let served = serve();

    let (status, body) = propfind(served.port, "/f.txt", "0").await;
    assert_eq!(status, 207, "{body}");
    assert!(!body.contains("<D:collection/>"), "{body}");
    assert!(body.contains("<D:getcontentlength>4<"), "{body}");

    let (status, body) = propfind(served.port, "/empty/", "1").await;
    assert_eq!(status, 207, "{body}");
    assert!(body.contains("<D:collection/>"), "{body}");

    let (status, body) = propfind(served.port, "/docs/", "1").await;
    assert_eq!(status, 207, "{body}");
    assert!(body.contains("/docs/a.txt"), "{body}");

    let (status, body) = propfind(served.port, "/", "1").await;
    assert_eq!(status, 207, "{body}");
    assert!(body.contains("/f.txt") && body.contains("/docs/"), "{body}");
}

/// What a client of the server sees: a missing file is not found, not a
/// directory.
#[test]
fn the_cli_does_not_see_a_missing_file_as_a_directory() {
    let served = serve();
    let out = output_within_limit(
        Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
            .args(["--format", "json", "stat"])
            .arg(format!("webdav://127.0.0.1:{}/", served.port))
            .arg("/missing.txt")
            .env("XDG_CONFIG_HOME", served.config.path())
            .env("HOME", served.config.path())
            .env("APPDATA", served.config.path())
            .env("LOCALAPPDATA", served.config.path())
            .env_remove("RUST_LOG")
            .stdin(Stdio::null()),
        "aeroftp-cli stat",
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
