//! A command given a URL that names a file or folder, and no path of its own,
//! acts on what the URL names. It acted on the folder holding it: `rm
//! ftp://host/c/d/full -r --force` removed all of `/c/d`, and `purge` and
//! `rmdirs` did the same. Seen through `serve ftp`, `serve webdav` and `serve
//! sftp`, so every command runs the real CLI against a real server and the
//! folder is checked on disk.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const USER: &str = "u";

fn cli() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind a free port")
        .local_addr()
        .unwrap()
        .port()
}

fn spawn(
    proto: Proto,
    folder: &Path,
    config: &Path,
    port: u16,
    password: &str,
    log: &tempfile::NamedTempFile,
) -> Child {
    let stderr = std::fs::OpenOptions::new()
        .append(true)
        .open(log.path())
        .expect("server log");
    isolated_cli::command(cli(), config)
        .args(["serve", proto.serve()])
        .arg(folder)
        .args(["--addr", &format!("127.0.0.1:{port}")])
        .env("AEROFTP_SERVE_USER", USER)
        .env("AEROFTP_SERVE_PASSWORD", password)
        .env("AEROFTP_SERVE_AUTH_TOKEN", password)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .expect("spawn aeroftp-cli serve")
}

#[derive(Clone, Copy, Debug)]
enum Proto {
    Ftp,
    WebDav,
    Sftp,
}

/// SFTP saves the served host key in `~/.ssh/known_hosts`. On Unix that is
/// below the redirected `HOME`; Windows finds the profile folder through the
/// Known Folder API whatever `USERPROFILE` says, so there the key would land
/// in the real file.
#[cfg(unix)]
const PROTOS: &[Proto] = &[Proto::Ftp, Proto::WebDav, Proto::Sftp];
#[cfg(not(unix))]
const PROTOS: &[Proto] = &[Proto::Ftp, Proto::WebDav];

impl Proto {
    fn serve(self) -> &'static str {
        match self {
            Proto::Ftp => "ftp",
            Proto::WebDav => "webdav",
            Proto::Sftp => "sftp",
        }
    }
}

/// `serve <proto>` of a fresh folder holding:
///
/// ```text
/// c/sib.txt
/// c/d/f.txt
/// c/d/inner.txt      (a decoy: named like the file inside sub/)
/// c/d/empty/
/// c/d/full/x
/// c/d/sub/inner.txt
/// ```
struct Server {
    proto: Proto,
    child: Child,
    port: u16,
    folder: tempfile::TempDir,
    config: tempfile::TempDir,
    /// Only this server takes it, so a sign-in proves the port is its own.
    password: String,
    /// What the server wrote to stderr, shown when it does not answer.
    log: tempfile::NamedTempFile,
}

impl Server {
    fn start(proto: Proto) -> Self {
        let folder = tempfile::tempdir().expect("served folder");
        let d = folder.path().join("c/d");
        for dir in ["empty", "full", "sub"] {
            std::fs::create_dir_all(d.join(dir)).unwrap();
        }
        std::fs::write(folder.path().join("c/sib.txt"), b"s").unwrap();
        std::fs::write(d.join("f.txt"), b"f").unwrap();
        std::fs::write(d.join("inner.txt"), b"decoy").unwrap();
        std::fs::write(d.join("full/x"), b"x").unwrap();
        std::fs::write(d.join("sub/inner.txt"), b"i").unwrap();

        static SERVERS: AtomicUsize = AtomicUsize::new(0);
        let password = format!(
            "cli-url-target-{}-{}",
            std::process::id(),
            SERVERS.fetch_add(1, Ordering::Relaxed)
        );
        let config = isolated_cli::root();
        let log = tempfile::NamedTempFile::new().expect("server log");
        let port = free_port();
        let child = spawn(proto, folder.path(), config.path(), port, &password, &log);
        let mut server = Self {
            proto,
            child,
            port,
            folder,
            config,
            password,
            log,
        };
        for _ in 0..5 {
            if server.wait_ready() {
                return server;
            }
            // Another process took the port between `free_port` and the bind.
            server.port = free_port();
            server.child = spawn(
                proto,
                server.folder.path(),
                server.config.path(),
                server.port,
                &server.password,
                &server.log,
            );
        }
        panic!("serve {proto:?} kept exiting:\n{}", server.log());
    }

    /// True once this server answers a sign-in; false if it exited.
    fn wait_ready(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self.child.try_wait().expect("serve status").is_some() {
                return false;
            }
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok()
                && self.run(&["ls", &self.url("/c/")]).status.success()
            {
                return true;
            }
            assert!(
                Instant::now() < deadline,
                "serve {:?} did not answer:\n{}",
                self.proto,
                self.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.log.path()).unwrap_or_default()
    }

    /// `<proto>://u@127.0.0.1:<port><path>`, `path` as typed after the host.
    fn url(&self, path: &str) -> String {
        format!(
            "{}://{USER}@127.0.0.1:{}{path}",
            self.proto.serve(),
            self.port
        )
    }

    /// The CLI with `args`, against this server.
    fn run(&self, args: &[&str]) -> Output {
        let mut command = isolated_cli::command(cli(), self.config.path());
        if let Proto::Sftp = self.proto {
            command.arg("--trust-host-key");
        }
        command
            .args(args)
            .env("AEROFTP_PASSWORD", &self.password)
            .stdin(Stdio::null())
            .output()
            .expect("run aeroftp-cli")
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.folder.path().join(relative)
    }

    /// Every entry below `c`, sorted, as `c/...` paths.
    fn tree(&self) -> Vec<String> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
                if path.is_dir() {
                    walk(&path, root, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self.folder.path(), self.folder.path(), &mut out);
        out.sort();
        out
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn ok(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The tree of the fixture without `gone` and what is below it.
fn all_but(gone: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = [
        "c",
        "c/d",
        "c/d/empty",
        "c/d/f.txt",
        "c/d/full",
        "c/d/full/x",
        "c/d/inner.txt",
        "c/d/sub",
        "c/d/sub/inner.txt",
        "c/sib.txt",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    all.retain(|entry| {
        !gone
            .iter()
            .any(|g| entry == g || entry.starts_with(&format!("{g}/")))
    });
    all
}

#[test]
fn rm_recursive_removes_the_folder_the_url_names_and_nothing_beside_it() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        let out = server.run(&["rm", &server.url("/c/d/full"), "-r", "--force"]);
        ok(&out, &format!("{proto:?} rm -r"));
        assert_eq!(server.tree(), all_but(&["c/d/full"]), "{proto:?}");
    }
}

#[test]
fn rm_removes_the_file_the_url_names() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        let out = server.run(&["rm", &server.url("/c/d/f.txt")]);
        ok(&out, &format!("{proto:?} rm"));
        assert_eq!(server.tree(), all_but(&["c/d/f.txt"]), "{proto:?}");
    }
}

#[test]
fn purge_removes_the_folder_the_url_names_and_nothing_beside_it() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        let out = server.run(&["purge", &server.url("/c/d/full"), "--force"]);
        ok(&out, &format!("{proto:?} purge"));
        assert_eq!(server.tree(), all_but(&["c/d/full"]), "{proto:?}");
    }
}

#[test]
fn rmdirs_scans_the_folder_the_url_names_and_not_its_parent() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        // `full` holds a file and the empty folder `hollow`, which the scan
        // removes. Its parent holds `empty`, which a scan of the parent
        // removed.
        std::fs::create_dir(server.path("c/d/full/hollow")).unwrap();
        let out = server.run(&["rmdirs", &server.url("/c/d/full"), "--force"]);
        ok(&out, &format!("{proto:?} rmdirs"));
        assert_eq!(server.tree(), all_but(&[]), "{proto:?}");
    }
}

#[test]
fn rmdir_removes_the_empty_folder_the_url_names() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        let out = server.run(&["rmdir", &server.url("/c/d/empty")]);
        ok(&out, &format!("{proto:?} rmdir"));
        assert_eq!(server.tree(), all_but(&["c/d/empty"]), "{proto:?}");
    }
}

#[test]
fn a_path_given_beside_a_folder_url_is_inside_that_folder() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        let out = server.run(&["rm", &server.url("/c/d/sub"), "inner.txt"]);
        ok(&out, &format!("{proto:?} rm with a path"));
        assert_eq!(server.tree(), all_but(&["c/d/sub/inner.txt"]), "{proto:?}");
    }
}

#[test]
fn stat_and_ls_read_what_the_url_names() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        let out = server.run(&["stat", &server.url("/c/d/f.txt"), "--json"]);
        ok(&out, &format!("{proto:?} stat"));
        let stat: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stat --json");
        assert_eq!(stat["entry"]["name"], "f.txt", "{proto:?} {stat}");
        assert_eq!(stat["entry"]["size"], 1, "{proto:?} {stat}");

        let out = server.run(&["ls", &server.url("/c/d/full"), "--json"]);
        ok(&out, &format!("{proto:?} ls"));
        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(listing.contains("\"x\""), "{proto:?} {listing}");
        assert!(
            !listing.contains("\"empty\""),
            "{proto:?} listed the parent: {listing}"
        );
    }
}

#[test]
fn a_name_the_url_gives_is_decoded_and_created_as_named() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        for (path, name) in [
            ("/c/a%20b", "a b"),
            ("/c/100%25", "100%"),
            // A `%` typed as is, not followed by two hex digits.
            ("/c/100% done", "100% done"),
            ("/c/d/new%20one", "d/new one"),
        ] {
            let out = server.run(&["mkdir", &server.url(path)]);
            ok(&out, &format!("{proto:?} mkdir {path}"));
            assert!(
                server.path(&format!("c/{name}")).is_dir(),
                "{proto:?}: {path} did not make c/{name}: {:?}",
                server.tree()
            );
        }
        // No folder spelled with its escapes.
        assert!(!server.path("c/a%20b").exists(), "{proto:?}");
    }
}

/// A URL whose decoded path climbs out (`d%2F..%2Fsib.txt` is
/// `d/../sib.txt`) is refused as a positional `..` is, and a WebDAV URL whose
/// last name encodes a `/` is refused rather than read as the name after it
/// (`x%2Fd` acted on `c/d`).
#[test]
fn a_url_that_encodes_a_parent_or_a_slash_is_refused_and_nothing_moves() {
    for &proto in PROTOS {
        let server = Server::start(proto);
        for path in ["/c/d%2F..%2Fsib.txt", "/c/x%2Fd"] {
            let out = server.run(&["rm", &server.url(path), "-r", "--force"]);
            // Over FTP and SFTP `x%2Fd` is `x/d`, absent: nothing to do.
            if path.contains("..") || matches!(proto, Proto::WebDav) {
                assert!(
                    !out.status.success(),
                    "{proto:?} rm {path} succeeded: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            assert_eq!(server.tree(), all_but(&[]), "{proto:?} rm {path}");
        }
    }
}

/// `serve` of a URL that names a file stops at once: as the root, a file
/// lists nothing and every request below it misses.
#[test]
fn serve_refuses_a_url_that_names_a_file() {
    let server = Server::start(Proto::Ftp);
    let mut child = isolated_cli::command(cli(), server.config.path())
        .args(["serve", "webdav", &server.url("/c/sib.txt")])
        .args(["--addr", &format!("127.0.0.1:{}", free_port())])
        .env("AEROFTP_PASSWORD", &server.password)
        .env("AEROFTP_SERVE_USER", USER)
        .env("AEROFTP_SERVE_PASSWORD", &server.password)
        .env("AEROFTP_SERVE_AUTH_TOKEN", &server.password)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().expect("serve").is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("serve started with a file as its root");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = child.wait_with_output().expect("serve");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(5), "{stderr}");
    assert!(stderr.contains("not a folder"), "{stderr}");
}

/// With `--profile` the path goes in the URL slot; a second one was dropped
/// without a word, so `rm --profile P /c/d full -r --force` removed all of
/// `/c/d`. A deleting command now refuses it and touches nothing.
#[test]
fn a_deleting_command_with_a_profile_refuses_an_argument_it_would_drop() {
    let server = Server::start(Proto::Ftp);
    let mut add = isolated_cli::command(cli(), server.config.path())
        .args(["profile-add", "--name", "served", "--protocol", "ftp"])
        .args(["--host", "127.0.0.1", "--port", &server.port.to_string()])
        .args(["--tls", "none", "--username", USER, "--password-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("profile-add");
    std::io::Write::write_all(
        &mut add.stdin.take().expect("stdin"),
        format!("{}\n", server.password).as_bytes(),
    )
    .expect("password");
    ok(&add.wait_with_output().expect("profile-add"), "profile-add");

    for args in [
        &["rm", "/c/d", "full", "-r", "--force"][..],
        &["purge", "/c/d", "full", "--force"],
        &["rmdirs", "/c/d", "full", "--force"],
        &["rmdir", "/c/d", "empty"],
    ] {
        let mut full = vec!["--profile", "served"];
        full.extend_from_slice(args);
        let out = server.run(&full);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("unexpected argument"), "{args:?}: {stderr}");
        assert_eq!(server.tree(), all_but(&[]), "{args:?} touched the folder");
    }

    // The profile itself works: the same command with one path removes
    // just what it names.
    let out = server.run(&["--profile", "served", "rm", "/c/d/full", "-r", "--force"]);
    ok(&out, "rm with a profile");
    assert_eq!(server.tree(), all_but(&["c/d/full"]));
}
