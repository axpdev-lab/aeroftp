//! `serve` on a non-loopback address generates a login when none is given,
//! and with `--quiet` it printed nothing at all in text output: the server ran
//! and nobody could know its password. A generated credential is now shown
//! even under `--quiet`, alone, on stderr.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Start `aeroftp-cli --quiet serve <mode> <folder>` on `0.0.0.0:0` and return
/// the first stderr line it prints, or `None` if it prints nothing in time.
fn first_quiet_stderr_line(mode: &str) -> Option<String> {
    let folder = tempfile::tempdir().expect("served folder");
    let config = tempfile::tempdir().expect("isolated config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["--quiet", "serve", mode])
        .arg(folder.path())
        .args(["--addr", "0.0.0.0:0", "--allow-remote-bind"])
        .env("XDG_CONFIG_HOME", config.path())
        .env("HOME", config.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aeroftp-cli");
    let stderr = child.stderr.take().expect("stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stderr).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(30)).ok();
    let _ = child.kill();
    let _ = child.wait();
    line.filter(|l| !l.is_empty())
}

#[test]
fn quiet_serve_webdav_shows_the_token_it_generated() {
    let line = first_quiet_stderr_line("webdav").expect("a line on stderr");
    assert!(line.starts_with("Generated password/token: "), "{line:?}");
    assert!(
        line.trim_end().len() > "Generated password/token: ".len(),
        "{line:?}"
    );
}

#[test]
fn quiet_serve_sftp_shows_the_login_it_generated() {
    let line = first_quiet_stderr_line("sftp").expect("a line on stderr");
    assert!(
        line.starts_with("Generated login: user aeroftp, password "),
        "{line:?}"
    );
}
