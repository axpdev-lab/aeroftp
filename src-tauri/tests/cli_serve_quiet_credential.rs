//! `serve` on a non-loopback address generates a login when none is given,
//! and with `--quiet` it printed nothing at all in text output: the server ran
//! and nobody could know its password. A generated credential is now shown
//! even under `--quiet`, alone, on stderr. A secret the user set is never
//! shown: the banner printed it, into every terminal and log it reached.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

/// Start `aeroftp-cli --quiet serve <mode> <folder>` on `0.0.0.0:0` and return
/// the first stderr line it prints, or `None` if it prints nothing in time.
fn first_quiet_stderr_line(mode: &str) -> Option<String> {
    let folder = tempfile::tempdir().expect("served folder");
    let config = isolated_cli::root();
    let mut child = isolated_cli::command(
        std::path::Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")),
        config.path(),
    )
    .args(["--quiet", "serve", mode])
    .arg(folder.path())
    .args(["--addr", "0.0.0.0:0", "--allow-remote-bind"])
    // No inherited log filter: the first stderr line must be the CLI's own.
    .env_remove("RUST_LOG")
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

const MODES: [&str; 4] = ["http", "webdav", "ftp", "sftp"];

/// What `aeroftp-cli serve <mode> <folder> --addr <addr>` prints on stderr
/// up to "Press Ctrl+C", with `env` set.
fn banner(mode: &str, addr: &str, extra: &[&str], env: &[(&str, &str)]) -> String {
    let folder = tempfile::tempdir().expect("served folder");
    let config = isolated_cli::root();
    let mut command = isolated_cli::command(
        std::path::Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")),
        config.path(),
    );
    command
        .args(["serve", mode])
        .arg(folder.path())
        .args(["--addr", addr])
        .args(extra)
        .env_remove("RUST_LOG")
        .env_remove("AEROFTP_SERVE_AUTH_TOKEN")
        .env_remove("AEROFTP_SERVE_USER")
        .env_remove("AEROFTP_SERVE_PASSWORD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("spawn aeroftp-cli");
    let stderr = child.stderr.take().expect("stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            text.push_str(&line);
            text.push('\n');
            if line.contains("Press Ctrl+C") {
                break;
            }
        }
        let _ = tx.send(text);
    });
    let text = rx.recv_timeout(Duration::from_secs(60)).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();
    text
}

#[test]
fn a_serve_banner_does_not_show_a_secret_the_user_set() {
    let secret = "banner-secret-set-by-the-user";
    for mode in MODES {
        let env: &[(&str, &str)] = if matches!(mode, "http" | "webdav") {
            &[("AEROFTP_SERVE_AUTH_TOKEN", secret)]
        } else {
            &[
                ("AEROFTP_SERVE_USER", "u"),
                ("AEROFTP_SERVE_PASSWORD", secret),
            ]
        };
        let text = banner(mode, "127.0.0.1:0", &[], env);
        assert!(text.contains("Authentication: enabled"), "{mode}: {text}");
        assert!(!text.contains(secret), "{mode} printed the secret: {text}");
    }
}

#[test]
fn a_serve_banner_shows_a_secret_it_generated() {
    for mode in MODES {
        let text = banner(mode, "0.0.0.0:0", &["--allow-remote-bind"], &[]);
        let shown = text
            .lines()
            .find_map(|line| {
                line.strip_prefix("Password: ")
                    .or_else(|| line.strip_prefix("Use any username and this password/token: "))
            })
            .unwrap_or_else(|| panic!("{mode} showed no secret: {text}"));
        assert!(
            shown.len() >= 16 && !shown.contains(' '),
            "{mode} showed {shown:?}"
        );
    }
}
