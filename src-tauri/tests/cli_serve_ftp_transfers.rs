//! What `serve ftp` sends and keeps, seen by an FTP client. A download could
//! not be resumed (REST answered 502), an append (APPE) replaced the file with
//! the appended bytes alone, and a download went out at full speed whatever
//! `--limit-rate` said, after the whole file was read into memory.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

const USER: &str = "u";
const PASSWORD: &str = "serve-ftp-transfers-test";

fn cli() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
}

/// A loopback port nobody listens on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind a free port")
        .local_addr()
        .unwrap()
        .port()
}

/// The bytes of a served file: a pattern that shows a shifted or truncated
/// copy.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// `serve ftp` of `folder` on its own port, killed when dropped.
struct Server {
    child: Child,
    port: u16,
    _config: tempfile::TempDir,
}

impl Server {
    fn start(folder: &Path, global_args: &[&str]) -> Self {
        let config = isolated_cli::root();
        let port = free_port();
        let child = isolated_cli::command(cli(), config.path())
            .args(global_args)
            .args(["serve", "ftp"])
            .arg(folder)
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .env("AEROFTP_SERVE_USER", USER)
            .env("AEROFTP_SERVE_PASSWORD", PASSWORD)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn aeroftp-cli serve ftp");
        Self {
            child,
            port,
            _config: config,
        }
    }

    /// A logged-in binary session, once the server answers.
    fn session(&self) -> Ftp {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(stream) = TcpStream::connect(("127.0.0.1", self.port)) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .unwrap();
                let mut ftp = Ftp {
                    reader: BufReader::new(stream.try_clone().unwrap()),
                    writer: stream,
                };
                if ftp.reply().starts_with("220") {
                    assert!(ftp.send(&format!("USER {USER}")).starts_with("331"));
                    assert!(ftp.send(&format!("PASS {PASSWORD}")).starts_with("230"));
                    assert!(ftp.send("TYPE I").starts_with("200"));
                    return ftp;
                }
            }
            assert!(Instant::now() < deadline, "serve ftp did not answer");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Ftp {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Ftp {
    /// One reply; of a multi-line one, its last line.
    fn reply(&mut self) -> String {
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("a reply line");
            assert!(!line.is_empty(), "the server closed the control connection");
            if line.len() >= 4 && line.as_bytes()[3] == b' ' {
                return line;
            }
        }
    }

    fn send(&mut self, command: &str) -> String {
        self.writer
            .write_all(format!("{command}\r\n").as_bytes())
            .unwrap();
        self.reply()
    }

    /// A data connection opened with PASV.
    fn passive(&mut self) -> TcpStream {
        let reply = self.send("PASV");
        assert!(reply.starts_with("227"), "PASV: {reply}");
        let inside = &reply[reply.find('(').unwrap() + 1..reply.find(')').unwrap()];
        let numbers: Vec<u16> = inside
            .split(',')
            .map(|n| n.trim().parse().unwrap())
            .collect();
        let port = numbers[4] * 256 + numbers[5];
        TcpStream::connect(("127.0.0.1", port)).expect("the data connection")
    }

    /// `command` (RETR) and everything its data connection carried.
    fn download(&mut self, command: &str) -> Vec<u8> {
        let mut data = self.passive();
        let reply = self.send(command);
        assert!(
            reply.starts_with("150") || reply.starts_with("125"),
            "{command}: {reply}"
        );
        let mut received = Vec::new();
        data.read_to_end(&mut received).unwrap();
        let reply = self.reply();
        assert!(reply.starts_with("226"), "{command} ended with: {reply}");
        received
    }

    /// `command` (STOR, APPE) sending `bytes`; the reply that ended it.
    fn upload(&mut self, command: &str, bytes: &[u8]) -> String {
        let mut data = self.passive();
        let reply = self.send(command);
        if !(reply.starts_with("150") || reply.starts_with("125")) {
            return reply;
        }
        let _ = data.write_all(bytes);
        drop(data);
        self.reply()
    }
}

#[test]
fn a_download_resumed_with_rest_sends_the_rest_of_the_file() {
    let folder = tempfile::tempdir().unwrap();
    let content = pattern(300_000);
    std::fs::write(folder.path().join("big.bin"), &content).unwrap();
    let server = Server::start(folder.path(), &[]);
    let mut ftp = server.session();

    let reply = ftp.send("REST 1000");
    assert!(reply.starts_with("350"), "REST: {reply}");
    assert!(ftp.download("RETR big.bin") == content[1000..]);
    // A REST at the end sends nothing; the next download is whole again.
    assert!(ftp.send("REST 300000").starts_with("350"));
    assert!(ftp.download("RETR big.bin").is_empty());
    assert!(ftp.download("RETR big.bin") == content);
}

#[test]
fn an_upload_after_the_start_keeps_the_bytes_before_it() {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("big.bin");
    let content = pattern(1000);
    std::fs::write(&file, &content).unwrap();
    let server = Server::start(folder.path(), &[]);
    let mut ftp = server.session();

    // APPE: written after what the file holds.
    let reply = ftp.upload("APPE big.bin", b"appended");
    assert!(reply.starts_with("226"), "APPE: {reply}");
    let mut expected = content.clone();
    expected.extend_from_slice(b"appended");
    assert!(std::fs::read(&file).unwrap() == expected, "after APPE");

    // REST then STOR: written from the offset, the file ends where the
    // upload does.
    assert!(ftp.send("REST 100").starts_with("350"));
    let reply = ftp.upload("STOR big.bin", b"resumed");
    assert!(reply.starts_with("226"), "REST + STOR: {reply}");
    let mut expected = content[..100].to_vec();
    expected.extend_from_slice(b"resumed");
    assert!(
        std::fs::read(&file).unwrap() == expected,
        "after REST + STOR"
    );

    // An offset past the end of the file would leave a hole: refused, and
    // the file stays as it was.
    assert!(ftp.send("REST 5000").starts_with("350"));
    let reply = ftp.upload("STOR big.bin", b"past the end");
    assert!(reply.starts_with('5'), "a resume past the end: {reply}");
    assert!(
        std::fs::read(&file).unwrap() == expected,
        "after a refused resume"
    );

    // A plain STOR still replaces the file. libunftp clears a REST offset
    // only when a transfer succeeds, so the refused one still holds 5000.
    assert!(ftp.send("REST 0").starts_with("350"));
    let reply = ftp.upload("STOR big.bin", b"new");
    assert!(reply.starts_with("226"), "STOR: {reply}");
    assert_eq!(std::fs::read(&file).unwrap(), b"new");
}

/// 4 MiB at 1 MiB/s: the limiter's burst is one second of the cap, so the
/// download cannot end in less than three. It ended in a fraction of one.
#[test]
fn a_served_download_holds_the_speed_limit() {
    let folder = tempfile::tempdir().unwrap();
    let content = pattern(4 * 1024 * 1024);
    std::fs::write(folder.path().join("big.bin"), &content).unwrap();
    let server = Server::start(folder.path(), &["--limit-rate", "1M"]);
    let mut ftp = server.session();

    let started = Instant::now();
    let received = ftp.download("RETR big.bin");
    let elapsed = started.elapsed();
    assert!(received == content, "the download differs");
    assert!(
        elapsed >= Duration::from_millis(2_800),
        "4 MiB under a 1 MiB/s cap came down in {elapsed:?}"
    );
}
