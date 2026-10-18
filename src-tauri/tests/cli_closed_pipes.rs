//! A peer that hangs up must not end the CLI. The CLI restored the default
//! action of SIGPIPE at startup, so a write to a socket whose client had
//! closed it killed the process: one client of `serve ftp` that connected and
//! hung up at once ended the server (exit 141, signal 13). SIGPIPE now stays
//! ignored, and a closed pipe on the CLI's own output still ends it quietly,
//! as `aeroftp-cli ls | head` expects.

#![cfg(unix)]

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

fn cli() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
}

/// A loopback port nobody listens on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind a free port")
        .local_addr()
        .unwrap()
        .port()
}

/// The greeting line of an FTP server on `port`, once it answers.
fn greeting(port: u16, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok() && !line.is_empty() {
                return Some(line);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[test]
fn serve_ftp_outlives_clients_that_hang_up_at_once() {
    let folder = tempfile::tempdir().expect("served folder");
    let config = isolated_cli::root();
    let port = free_port();
    let mut server = isolated_cli::command(cli(), config.path())
        .args(["serve", "ftp"])
        .arg(folder.path())
        .args(["--addr", &format!("127.0.0.1:{port}")])
        .env("AEROFTP_SERVE_USER", "u")
        .env("AEROFTP_SERVE_PASSWORD", "closed-pipes-test")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn aeroftp-cli serve ftp");
    let first = greeting(port, Duration::from_secs(60));
    assert!(
        first.as_deref().is_some_and(|line| line.starts_with("220")),
        "the server did not greet: {first:?}"
    );

    // Each client sends a command and hangs up before the server answers it:
    // the server's next writes go to a socket its peer closed.
    for _ in 0..20 {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.write_all(b"NOOP\r\nNOOP\r\nNOOP\r\n");
        }
    }
    std::thread::sleep(Duration::from_millis(500));

    let status = server.try_wait().expect("poll the server");
    let after = greeting(port, Duration::from_secs(5));
    let _ = server.kill();
    let _ = server.wait();
    assert!(
        status.is_none(),
        "a client that hung up ended the server: {status:?} (signal {:?})",
        status.and_then(|s| s.signal())
    );
    assert!(
        after.as_deref().is_some_and(|line| line.starts_with("220")),
        "the server stopped greeting: {after:?}"
    );
}

/// `ls | head`: the listing stops writing once its reader is gone and the
/// CLI ends the way a process SIGPIPE kills does, printing nothing.
#[test]
fn a_listing_into_a_closed_pipe_ends_quietly() {
    let folder = tempfile::tempdir().expect("listed folder");
    for n in 0..3000 {
        std::fs::write(folder.path().join(format!("file-{n:05}.txt")), b"x").unwrap();
    }
    let config = isolated_cli::root();
    let mut ls = isolated_cli::command(cli(), config.path())
        .arg("ls")
        .arg(folder.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aeroftp-cli ls");
    let mut stdout = BufReader::new(ls.stdout.take().expect("stdout"));
    let mut line = String::new();
    stdout
        .read_line(&mut line)
        .expect("the first line of the listing");
    drop(stdout);
    let mut stderr = String::new();
    ls.stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .unwrap();
    let status = ls.wait().expect("wait for ls");
    assert!(!line.is_empty(), "the listing printed nothing");
    assert_eq!(
        status.signal(),
        Some(libc::SIGPIPE),
        "{status:?}, stderr: {stderr}"
    );
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "a closed output pipe printed: {stderr}"
    );
}
