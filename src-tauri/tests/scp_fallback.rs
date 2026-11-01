//! SCP as a protocol, and SFTP going on over SCP when its server has no SFTP,
//! against a russh server whose exec channels run this machine's `sh`, `ls`,
//! `find` and OpenSSH `scp` (`support/ssh_exec_server.rs`).
//!
//! The server without SFTP comes in the two shapes measured on real ones: the
//! subsystem refused (OpenSSH with no `Subsystem sftp` line), and accepted
//! then ended with exit status 127 (OpenWrt's Dropbear, whose
//! `/usr/libexec/sftp-server` the stock image lacks). Before the fallback the
//! second one hung 10 s and failed with "Timeout".
//!
//! Linux only: the listing needs an `ls` with `--full-time` (GNU, BusyBox,
//! uutils), which macOS does not ship; and `HOME` is redirected so an accepted
//! host key cannot reach the real `known_hosts`.
#![cfg(target_os = "linux")]

#[path = "support/scoped_env.rs"]
mod scoped_env;
#[path = "support/ssh_exec_server.rs"]
mod ssh_exec_server;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{
    ProviderConfig, ProviderError, ProviderFactory, ProviderTransferExecutorKind, ProviderType,
    StorageProvider,
};
use russh::keys::ssh_key::LineEnding;
use scoped_env::ScopedEnv;
use ssh_exec_server::{ed25519, start, Options, Sftp};

/// `HOME` is process-wide: one test at a time changes it.
static HOME_LOCK: Mutex<()> = Mutex::new(());

const PASSWORD: &str = "pw-7f3a-not-in-any-log";
const PASSPHRASE: &str = "pp-91c2-not-in-any-log";

struct Fixture {
    root: PathBuf,
    _home: ScopedEnv,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("scp-fallback-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home/.ssh")).unwrap();
        std::fs::create_dir_all(root.join("remote")).unwrap();
        std::fs::create_dir_all(root.join("local")).unwrap();
        let home = ScopedEnv::set("HOME", root.join("home"));
        Self {
            root,
            _home: home,
            _lock: lock,
        }
    }

    fn remote(&self) -> PathBuf {
        self.root.join("remote")
    }

    fn local(&self, name: &str) -> PathBuf {
        self.root.join("local").join(name)
    }

    fn options(&self, sftp: Sftp) -> Options {
        Options {
            cwd: self.remote(),
            path: std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
            sftp,
            accept_none: false,
            password: Some(PASSWORD.to_string()),
            key: None,
            host_key: ed25519(),
            restricted: None,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

fn config(port: u16, root: &Path) -> SftpConfig {
    SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "root".to_string(),
        password: Some(secrecy::SecretString::from(PASSWORD.to_string())),
        private_key_path: None,
        key_passphrase: None,
        initial_path: Some(s(root)),
        timeout_secs: 20,
        trust_unknown_hosts: true,
    }
}

/// `connect`, with a cap so a hang fails the test; returns the time it took.
async fn timed_connect(p: &mut dyn StorageProvider) -> (Result<(), ProviderError>, Duration) {
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(30), p.connect())
        .await
        .expect("connect hung");
    (result, started.elapsed())
}

/// Names a shell would split, expand or take for options.
const HOSTILE: &[&str] = &[
    "a b.txt",
    "it's.txt",
    "-dash.txt",
    "$(touch pwned).txt",
    "back`tick`.txt",
    "semi;colon & amp.txt",
    "ünïcödé €.txt",
];

/// List, upload, download, rename and delete under hostile names, through
/// whatever the provider speaks, and check the remote folder on disk.
async fn exercise(p: &mut dyn StorageProvider, fx: &Fixture) {
    let remote = fx.remote();
    p.mkdir("dir with space").await.expect("mkdir");
    assert!(remote.join("dir with space").is_dir());
    for (i, name) in HOSTILE.iter().enumerate() {
        let local = fx.local(&format!("up-{i}"));
        let body = format!("{i}:{name}").repeat(100 + i);
        std::fs::write(&local, &body).unwrap();
        let target = format!("dir with space/{name}");
        let progress = Arc::new(Mutex::new(Vec::new()));
        let seen = progress.clone();
        p.upload(
            &s(&local),
            &target,
            Some(Box::new(move |done, total| {
                seen.lock().unwrap().push((done, total))
            })),
        )
        .await
        .unwrap_or_else(|e| panic!("upload {name}: {e}"));
        assert_eq!(
            std::fs::read_to_string(remote.join(&target)).unwrap(),
            body,
            "{name}"
        );
        let last = *progress.lock().unwrap().last().expect("progress reported");
        assert_eq!(last, (body.len() as u64, body.len() as u64), "{name}");

        let back = fx.local(&format!("down-{i}"));
        p.download(&target, &s(&back), None)
            .await
            .unwrap_or_else(|e| panic!("download {name}: {e}"));
        assert_eq!(std::fs::read_to_string(&back).unwrap(), body, "{name}");
        assert_eq!(p.size(&target).await.unwrap(), body.len() as u64);
    }
    let listed = p.list("dir with space").await.expect("list");
    let mut names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();
    names.sort_unstable();
    let mut want = HOSTILE.to_vec();
    want.sort_unstable();
    assert_eq!(names, want);
    assert!(listed.iter().all(|e| !e.is_dir && e.size > 0));

    p.rename("dir with space/a b.txt", "dir with space/renamed it's.txt")
        .await
        .expect("rename");
    assert!(remote.join("dir with space/renamed it's.txt").is_file());
    assert!(!remote.join("dir with space/a b.txt").exists());
    // A rename onto an existing name is refused, as over SFTP.
    assert!(p
        .rename("dir with space/it's.txt", "dir with space/-dash.txt")
        .await
        .is_err());
    p.delete("dir with space/-dash.txt").await.expect("delete");
    assert!(!remote.join("dir with space/-dash.txt").exists());
    assert!(!p.exists("dir with space/-dash.txt").await.unwrap());
    p.rmdir_recursive("dir with space").await.expect("rmdir -r");
    assert!(!remote.join("dir with space").exists());
    assert!(
        !remote.join("pwned").exists() && !fx.root.join("pwned").exists(),
        "a name ran as a command"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_sftp_profile_reaches_a_server_without_sftp_over_scp() {
    for mode in [Sftp::Refuse, Sftp::EndAtOnce] {
        let fx = Fixture::new(&format!("fallback-{mode:?}"));
        let (port, seen) = start(fx.options(mode)).await;
        let mut p = SftpProvider::new(config(port, &fx.remote()));
        let (result, took) = timed_connect(&mut p).await;
        result.unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        assert!(p.in_scp_mode(), "{mode:?}");
        assert!(took < Duration::from_secs(5), "{mode:?} took {took:?}");
        assert_eq!(seen.subsystems(), 1, "SFTP asked once");
        assert_eq!(p.provider_type(), ProviderType::Sftp);
        assert!(p.server_info().await.unwrap().starts_with("SCP over SSH"));
        assert_eq!(p.pwd().await.unwrap(), s(&fx.remote()));
        // SCP mode offers what its delegate does, and nothing it cannot
        // do: it goes on with transfers and reads ranges (the server has
        // `dd`, `cat` and `head`), and it has no delta sync.
        assert!(p.supports_resume() && p.supports_resume_upload_append());
        assert!(p.transfer_optimization_hints().supports_range_download);
        assert!(!p.transfer_optimization_hints().supports_delta_sync);
        assert!(!p.supports_delta_sync());
        exercise(&mut p, &fx).await;
        p.disconnect().await.unwrap();
        assert!(!p.is_connected());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_the_fallback_off_the_error_names_the_cause_at_once() {
    for mode in [Sftp::Refuse, Sftp::EndAtOnce] {
        let fx = Fixture::new(&format!("off-{mode:?}"));
        let (port, seen) = start(fx.options(mode)).await;
        let mut p = SftpProvider::new(config(port, &fx.remote()));
        p.set_scp_fallback(false);
        let (result, took) = timed_connect(&mut p).await;
        match result {
            Err(ProviderError::NotSupported(why)) => {
                assert!(why.contains("openssh-sftp-server"), "{why}");
                assert!(why.contains("SCP fallback"), "{why}");
                let cause = match mode {
                    Sftp::Refuse => "refused the SFTP subsystem",
                    _ => "has no SFTP server program",
                };
                assert!(why.contains(cause), "{why}");
            }
            other => panic!("{mode:?}: {other:?}"),
        }
        assert!(took < Duration::from_secs(5), "{mode:?} took {took:?}");
        assert_eq!(seen.execs(), 0, "nothing ran on the server");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_with_sftp_stays_on_sftp() {
    let fx = Fixture::new("serve");
    let (port, seen) = start(fx.options(Sftp::Serve)).await;
    let mut p = SftpProvider::new(config(port, &fx.remote()));
    let (result, _) = timed_connect(&mut p).await;
    result.expect("connect");
    assert!(!p.in_scp_mode());
    assert_eq!(seen.execs(), 0, "no SCP probe on an SFTP server");
    assert!(p.server_info().await.unwrap().starts_with("SFTP Server"));
}

fn scp_profile(port: u16, root: &Path, extra: &[(&str, String)]) -> ProviderConfig {
    ProviderConfig {
        name: "router".to_string(),
        provider_type: ProviderType::Scp,
        host: "127.0.0.1".to_string(),
        port: Some(port),
        username: Some("root".to_string()),
        password: None,
        initial_path: Some(s(root)),
        extra: extra
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .chain([("trust_unknown_hosts".to_string(), "true".to_string())])
            .collect(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_scp_protocol_logs_in_with_a_password_and_with_a_key() {
    // Password, through the factory as the GUI, CLI and MCP build it.
    let fx = Fixture::new("scp-password");
    let (port, seen) = start(fx.options(Sftp::Serve)).await;
    let mut profile = scp_profile(port, &fx.remote(), &[]);
    profile.password = Some(PASSWORD.to_string());
    let mut p = ProviderFactory::create(&profile).expect("factory");
    assert_eq!(p.provider_type(), ProviderType::Scp);
    let (result, _) = timed_connect(p.as_mut()).await;
    result.expect("connect with a password");
    assert_eq!(seen.subsystems(), 0, "SCP never asks for SFTP");
    exercise(p.as_mut(), &fx).await;
    drop(fx);

    // An encrypted key, and no password at all.
    let fx = Fixture::new("scp-key");
    let client = ed25519();
    let key_path = fx.root.join("home/.ssh/id_test");
    let encrypted = client
        .encrypt(&mut rand_010::rng(), PASSPHRASE)
        .expect("encrypt key");
    std::fs::write(&key_path, encrypted.to_openssh(LineEnding::LF).unwrap()).unwrap();
    let mut options = fx.options(Sftp::Refuse);
    options.password = None;
    options.key = Some(client.public_key().clone());
    let (port, _) = start(options).await;
    let profile = scp_profile(
        port,
        &fx.remote(),
        &[
            ("private_key_path", s(&key_path)),
            ("key_passphrase", PASSPHRASE.to_string()),
        ],
    );
    let mut p = ProviderFactory::create(&profile).expect("factory");
    let (result, _) = timed_connect(p.as_mut()).await;
    result.expect("connect with a key");
    exercise(p.as_mut(), &fx).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scp_checks_the_host_key_as_sftp_does() {
    let fx = Fixture::new("hostkey");
    let host_key = ed25519();
    let mut options = fx.options(Sftp::Refuse);
    options.host_key = host_key.clone();
    let (port, seen) = start(options).await;

    // Unknown key, not trusted: refused before any login.
    let mut profile = scp_profile(port, &fx.remote(), &[]);
    profile.password = Some(PASSWORD.to_string());
    profile.extra.remove("trust_unknown_hosts");
    let mut p = ProviderFactory::create(&profile).unwrap();
    match timed_connect(p.as_mut()).await.0 {
        Err(ProviderError::HostKeyRejected(why)) => {
            assert!(why.contains("has no key saved"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(seen.execs(), 0);

    // A different key saved for this host and port: refused, and the saved
    // key is left as it was.
    let other = ed25519();
    let known_hosts = fx.root.join("home/.ssh/known_hosts");
    let saved = format!(
        "[127.0.0.1]:{port} {}\n",
        other.public_key().to_openssh().unwrap()
    );
    std::fs::write(&known_hosts, &saved).unwrap();
    profile
        .extra
        .insert("trust_unknown_hosts".to_string(), "true".to_string());
    let mut p = ProviderFactory::create(&profile).unwrap();
    match timed_connect(p.as_mut()).await.0 {
        Err(ProviderError::HostKeyRejected(why)) => assert!(why.contains("different key"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&known_hosts).unwrap(), saved);
    assert_eq!(seen.execs(), 0);

    // The same refusal on the fallback path: SCP mode starts only after the
    // host key and the login were accepted.
    let mut sftp = SftpProvider::new(config(port, &fx.remote()));
    match timed_connect(&mut sftp).await.0 {
        Err(ProviderError::HostKeyRejected(why)) => assert!(why.contains("different key"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(seen.execs(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restricted_account_is_refused_with_a_clear_error() {
    let fx = Fixture::new("restricted");
    let mut options = fx.options(Sftp::Refuse);
    options.restricted = Some("This account is restricted by rssh.".to_string());
    let (port, _) = start(options).await;

    let mut profile = scp_profile(port, &fx.remote(), &[]);
    profile.password = Some(PASSWORD.to_string());
    let mut p = ProviderFactory::create(&profile).unwrap();
    match timed_connect(p.as_mut()).await.0 {
        Err(ProviderError::NotSupported(why)) => {
            assert!(why.contains("restricted shell"), "{why}");
            assert!(why.contains("restricted by rssh"), "{why}");
        }
        other => panic!("{other:?}"),
    }

    let mut sftp = SftpProvider::new(config(port, &fx.remote()));
    match timed_connect(&mut sftp).await.0 {
        Err(ProviderError::NotSupported(why)) => {
            assert!(why.contains("refused the SFTP subsystem"), "{why}");
            assert!(why.contains("SCP cannot work there either"), "{why}");
            assert!(why.contains("restricted shell"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!sftp.is_connected());
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for Capture {
    type Writer = Capture;
    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

const LOG_CHILD: &str = "AEROFTP_SCP_LOG_CAPTURE_CHILD";

/// tracing caches each call site's interest for the whole process. While a
/// thread-local subscriber is the only one registered, a call site first
/// reached by another test's thread is cached as "never" from that thread's
/// empty default, and this thread then loses its lines too: the capture came
/// back empty once in four runs of the whole file. So the test runs alone in
/// a child process, with the capture as the process-wide subscriber.
#[test]
fn no_secret_reaches_the_log() {
    if std::env::var_os(LOG_CHILD).is_some() {
        return logged_session();
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "no_secret_reaches_the_log", "--nocapture"])
        .env(LOG_CHILD, "1")
        .output()
        .expect("run the test alone");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{report}");
    assert!(
        report.contains("1 passed"),
        "the child ran no test: {report}"
    );
}

#[tokio::main(flavor = "current_thread")]
async fn logged_session() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let fx = Fixture::new("logs");
    let (port, _) = start(fx.options(Sftp::EndAtOnce)).await;
    let mut p = SftpProvider::new(config(port, &fx.remote()));
    p.connect().await.expect("connect");
    assert!(p.in_scp_mode());
    exercise(&mut p, &fx).await;

    let client = ed25519();
    let key_path = fx.root.join("home/.ssh/id_log");
    let encrypted = client.encrypt(&mut rand_010::rng(), PASSPHRASE).unwrap();
    std::fs::write(&key_path, encrypted.to_openssh(LineEnding::LF).unwrap()).unwrap();
    let mut options = fx.options(Sftp::Refuse);
    options.password = None;
    options.key = Some(client.public_key().clone());
    let (port, _) = start(options).await;
    let profile = scp_profile(
        port,
        &fx.remote(),
        &[
            ("private_key_path", s(&key_path)),
            ("key_passphrase", PASSPHRASE.to_string()),
        ],
    );
    let mut p = ProviderFactory::create(&profile).unwrap();
    p.connect().await.expect("connect with a key");
    p.list("").await.expect("list");

    let log = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(log.contains("SCP"), "the capture saw the provider: {log}");
    assert!(!log.contains(PASSWORD), "password in the log");
    assert!(!log.contains(PASSPHRASE), "passphrase in the log");
}

/// Bytes that are not text and do not repeat within a chunk.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// An SCP session on a server with a password, connected.
async fn scp_session(fx: &Fixture) -> Box<dyn StorageProvider> {
    let (port, _) = start(fx.options(Sftp::Serve)).await;
    let mut profile = scp_profile(port, &fx.remote(), &[]);
    profile.password = Some(PASSWORD.to_string());
    let mut p = ProviderFactory::create(&profile).expect("factory");
    timed_connect(p.as_mut()).await.0.expect("connect");
    p
}

type Progress = Option<Box<dyn Fn(u64, u64) + Send>>;
type Calls = Arc<Mutex<Vec<(u64, u64)>>>;

/// A progress callback and what it was called with.
fn recorder() -> (Progress, Calls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    (
        Some(Box::new(move |done, total| {
            seen.lock().unwrap().push((done, total))
        })),
        calls,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replace_puts_a_file_in_place_and_refuses_across_kinds() {
    let fx = Fixture::new("scp-replace");
    let mut p = scp_session(&fx).await;
    let r = fx.remote();
    assert!(p.supports_atomic_replace().await.unwrap());

    std::fs::write(r.join("target it's.txt"), b"old").unwrap();
    std::fs::write(r.join("staged $(x).tmp"), b"new").unwrap();
    p.replace("staged $(x).tmp", "target it's.txt")
        .await
        .expect("a file in place of a file");
    assert_eq!(std::fs::read(r.join("target it's.txt")).unwrap(), b"new");
    assert!(!r.join("staged $(x).tmp").exists());

    // A file in place of a folder: refused, and nothing changed.
    std::fs::create_dir(r.join("a folder")).unwrap();
    std::fs::write(r.join("a folder/keep"), b"k").unwrap();
    std::fs::write(r.join("f.tmp"), b"f").unwrap();
    let err = p.replace("f.tmp", "a folder").await.unwrap_err();
    assert!(matches!(err, ProviderError::AlreadyExists(_)), "{err}");
    assert!(r.join("a folder/keep").is_file() && r.join("f.tmp").is_file());

    // A folder replaces an empty folder only, as rename(2) does.
    std::fs::create_dir(r.join("new folder")).unwrap();
    std::fs::write(r.join("new folder/n"), b"n").unwrap();
    assert!(p.replace("new folder", "a folder").await.is_err());
    assert!(r.join("a folder/keep").is_file() && r.join("new folder/n").is_file());
    std::fs::remove_file(r.join("a folder/keep")).unwrap();
    p.replace("new folder", "a folder")
        .await
        .expect("a folder in place of an empty one");
    assert!(r.join("a folder/n").is_file() && !r.join("new folder").exists());

    // A symlink is replaced itself; what it pointed to is left alone.
    std::fs::write(r.join("real.txt"), b"real").unwrap();
    std::os::unix::fs::symlink(r.join("real.txt"), r.join("link.txt")).unwrap();
    std::fs::write(r.join("l.tmp"), b"via link").unwrap();
    p.replace("l.tmp", "link.txt")
        .await
        .expect("over a symlink");
    let link = std::fs::symlink_metadata(r.join("link.txt")).unwrap();
    assert!(!link.file_type().is_symlink());
    assert_eq!(std::fs::read(r.join("link.txt")).unwrap(), b"via link");
    assert_eq!(std::fs::read(r.join("real.txt")).unwrap(), b"real");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replace_that_fails_leaves_the_target_as_it_was() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new("scp-replace-fails");
    let mut p = scp_session(&fx).await;
    let r = fx.remote();
    std::fs::write(r.join("real.txt"), b"real").unwrap();
    std::fs::create_dir(r.join("real dir")).unwrap();
    symlink(r.join("real.txt"), r.join("to file")).unwrap();
    symlink(r.join("nowhere"), r.join("dangling")).unwrap();
    symlink(r.join("real dir"), r.join("to dir")).unwrap();
    let is_link = |name: &str| {
        std::fs::symlink_metadata(r.join(name))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    };

    // The staged file is gone: each link stays.
    for link in ["to file", "dangling", "to dir"] {
        assert!(p.replace("gone.tmp", link).await.is_err(), "{link}");
        assert!(is_link(link), "{link} was removed");
    }
    // A name in place of itself.
    let _ = p.replace("to file", "to file").await;
    assert!(is_link("to file"), "a link in place of itself was removed");
    assert_eq!(std::fs::read(r.join("real.txt")).unwrap(), b"real");

    // A folder cannot go inside itself: the empty folder there stays.
    std::fs::create_dir_all(r.join("outer/inner")).unwrap();
    assert!(p.replace("outer", "outer/inner").await.is_err());
    assert!(
        r.join("outer/inner").is_dir(),
        "the empty folder was removed"
    );

    // With nothing removed first, a folder still replaces an empty folder.
    std::fs::create_dir(r.join("staged dir")).unwrap();
    std::fs::write(r.join("staged dir/n"), b"n").unwrap();
    std::fs::create_dir(r.join("empty")).unwrap();
    p.replace("staged dir", "empty")
        .await
        .expect("a folder in place of an empty one");
    assert!(r.join("empty/n").is_file() && !r.join("staged dir").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_symlink_reports_its_file_and_a_read_to_memory_keeps_its_cap() {
    let fx = Fixture::new("scp-size-cap");
    let (port, seen) = start(fx.options(Sftp::Serve)).await;
    let mut profile = scp_profile(port, &fx.remote(), &[]);
    profile.password = Some(PASSWORD.to_string());
    let mut p = ProviderFactory::create(&profile).expect("factory");
    timed_connect(p.as_mut()).await.0.expect("connect");
    let r = fx.remote();

    // `size` follows a link, as SFTP's stat does: the file, not the link.
    std::fs::write(r.join("target it's.bin"), pattern(12_345)).unwrap();
    std::os::unix::fs::symlink("target it's.bin", r.join("link")).unwrap();
    assert_eq!(p.size("link").await.unwrap(), 12_345);

    // Under the cap the file comes back whole, through the link too.
    assert_eq!(
        p.download_to_bytes_capped("link", 12_345).await.unwrap(),
        pattern(12_345)
    );
    // Over it, the size is refused before any transfer: one command (the
    // stat), no `scp -f`, and nothing read into memory.
    let before = seen.execs();
    let refused = p.download_to_bytes_capped("link", 12_344).await;
    assert!(
        matches!(&refused, Err(ProviderError::TransferFailed(m)) if m.contains("too large")),
        "{refused:?}"
    );
    assert_eq!(seen.execs() - before, 1, "only the stat ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn free_space_digests_and_search_come_from_the_server() {
    use md5::Md5;
    use sha2::{Digest, Sha256};

    let fx = Fixture::new("scp-df-sum-find");
    let mut p = scp_session(&fx).await;
    let r = fx.remote();

    let space = p.storage_info().await.expect("df");
    assert!(space.total > 0 && space.free <= space.total && space.used <= space.total);

    let body = pattern(100_000);
    std::fs::write(r.join("sum me it's.bin"), &body).unwrap();
    assert_eq!(
        p.checksum_capability("sum me it's.bin").algorithms,
        ["sha256", "md5"]
    );
    let sums = p.checksum("sum me it's.bin").await.unwrap();
    assert_eq!(sums["sha256"], hex::encode(Sha256::digest(&body)));
    assert_eq!(sums["md5"], hex::encode(Md5::digest(&body)));

    std::fs::create_dir_all(r.join("logs/deep $(x)")).unwrap();
    std::fs::write(r.join("logs/a.log"), b"a").unwrap();
    std::fs::write(r.join("logs/deep $(x)/b it's.log"), b"bb").unwrap();
    std::fs::write(r.join("logs/c.txt"), b"c").unwrap();
    std::fs::write(r.join("outside.log"), b"o").unwrap();
    assert!(p.supports_find());
    let mut found: Vec<(String, String, u64, bool)> = p
        .find("logs", "*.log")
        .await
        .expect("find")
        .into_iter()
        .map(|e| (e.name, e.path, e.size, e.is_dir))
        .collect();
    found.sort();
    let at = |rel: &str| s(&r.join(rel));
    assert_eq!(
        found,
        [
            ("a.log".to_string(), at("logs/a.log"), 1, false),
            (
                "b it's.log".to_string(),
                at("logs/deep $(x)/b it's.log"),
                2,
                false
            ),
        ]
    );
    assert!(!r.join("pwned").exists() && !r.join("x").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_download_goes_on_from_its_partial_and_the_limit_paces_it() {
    use ftp_client_gui_lib::providers::atomic_write::resumable_part_len;

    let fx = Fixture::new("scp-resume-down");
    let mut p = scp_session(&fx).await;
    let body = pattern(2 * 1024 * 1024);
    std::fs::write(fx.remote().join("big it's.bin"), &body).unwrap();
    let local = fx.local("big it's.bin");
    assert!(p.supports_resume());

    // Slow enough (4 s for the whole file) to be stopped part way, once the
    // partial holds 256 KiB.
    p.set_speed_limit(0, 512).await.unwrap();
    assert_eq!(p.get_speed_limit().await.unwrap(), (0, 512));
    let started = Instant::now();
    {
        let local_path = s(&local);
        let download = p.download("big it's.bin", &local_path, None);
        tokio::pin!(download);
        loop {
            tokio::select! {
                done = &mut download => panic!("finished before it was stopped: {done:?}"),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    let temp = local.with_file_name("big it's.bin.aerotmp");
                    if std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0) >= 256 * 1024 {
                        break;
                    }
                }
            }
        }
    }
    // 256 KiB at 512 KiB/s: half a second at the least.
    assert!(started.elapsed() >= Duration::from_millis(400), "not paced");
    let part = resumable_part_len(&local).await;
    assert!(
        part >= 256 * 1024 && part < body.len() as u64,
        "part {part}"
    );

    p.set_speed_limit(0, 0).await.unwrap();
    let (progress, calls) = recorder();
    p.download("big it's.bin", &s(&local), progress)
        .await
        .expect("go on");
    assert_eq!(std::fs::read(&local).unwrap(), body);
    let calls = calls.lock().unwrap();
    // The first bytes reported are after the partial, not from zero.
    assert!(calls[0].0 > part, "{:?} after a part of {part}", calls[0]);
    assert_eq!(
        *calls.last().unwrap(),
        (body.len() as u64, body.len() as u64)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_given_up_part_way_closes_its_channel() {
    let fx = Fixture::new("scp-give-up");
    let (port, seen) = start(fx.options(Sftp::Serve)).await;
    let mut profile = scp_profile(port, &fx.remote(), &[]);
    profile.password = Some(PASSWORD.to_string());
    let mut p = ProviderFactory::create(&profile).expect("factory");
    timed_connect(p.as_mut()).await.0.expect("connect");
    std::fs::write(fx.remote().join("big.bin"), pattern(2 * 1024 * 1024)).unwrap();
    let local = fx.local("big.bin");

    // Paced so that it is still running when it is given up.
    p.set_speed_limit(0, 512).await.unwrap();
    let ran = seen.execs();
    let closed = seen.closes();
    {
        let local_path = s(&local);
        let download = p.download("big.bin", &local_path, None);
        tokio::pin!(download);
        loop {
            tokio::select! {
                done = &mut download => panic!("finished before it was given up: {done:?}"),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    let temp = local.with_file_name("big.bin.aerotmp");
                    if std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0) >= 128 * 1024 {
                        break;
                    }
                }
            }
        }
    }
    assert!(seen.execs() > ran, "the download ran no command");
    // A command that ends closes its own channel from the server's side; the
    // one given up is still running there, so only the client can close it,
    // or it keeps one of the server's sessions for as long as we stay
    // connected.
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.closes() == closed && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        seen.closes(),
        closed + 1,
        "the abandoned channel was left open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_upload_goes_on_by_appending_and_the_limit_paces_it() {
    let fx = Fixture::new("scp-resume-up");
    let mut p = scp_session(&fx).await;
    let r = fx.remote();
    let body = pattern(300_000);
    let local = fx.local("up.bin");
    std::fs::write(&local, &body).unwrap();
    assert!(p.supports_resume_upload_append());

    std::fs::write(r.join("up it's.bin"), &body[..100_000]).unwrap();
    let (progress, calls) = recorder();
    p.resume_upload(&s(&local), "up it's.bin", 100_000, progress)
        .await
        .expect("append");
    assert_eq!(std::fs::read(r.join("up it's.bin")).unwrap(), body);
    assert!(calls.lock().unwrap()[0].0 > 100_000);

    // An offset past the remote's end: the tail starts where the remote
    // ends, never past it.
    std::fs::write(r.join("up it's.bin"), &body[..50_000]).unwrap();
    p.resume_upload(&s(&local), "up it's.bin", 200_000, None)
        .await
        .expect("append from the remote's end");
    assert_eq!(std::fs::read(r.join("up it's.bin")).unwrap(), body);

    // A remote longer than the local file is no part of it: sent whole.
    std::fs::write(r.join("up it's.bin"), pattern(400_000)).unwrap();
    p.resume_upload(&s(&local), "up it's.bin", 400_000, None)
        .await
        .expect("whole");
    assert_eq!(std::fs::read(r.join("up it's.bin")).unwrap(), body);

    // 512 KiB at 256 KiB/s: two seconds, 1.5 at the least.
    let big = fx.local("paced.bin");
    std::fs::write(&big, pattern(512 * 1024)).unwrap();
    p.set_speed_limit(256, 0).await.unwrap();
    let started = Instant::now();
    p.upload(&s(&big), "paced.bin", None).await.expect("upload");
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "not paced"
    );
    assert_eq!(
        std::fs::read(r.join("paced.bin")).unwrap().len(),
        512 * 1024
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sftp_in_scp_mode_keeps_its_speed_limit_and_offers_the_same_operations() {
    let fx = Fixture::new("fallback-ops");
    let (port, seen) = start(fx.options(Sftp::Refuse)).await;
    let mut p = SftpProvider::new(config(port, &fx.remote()));
    // Set before connecting, as the GUI does from a saved profile.
    p.set_speed_limit(256, 0).await.unwrap();
    timed_connect(&mut p).await.0.expect("connect");
    assert!(p.in_scp_mode());
    assert!(p.storage_info().await.is_ok());
    assert!(p.supports_atomic_replace().await.unwrap() && p.supports_find());

    let big = fx.local("paced.bin");
    std::fs::write(&big, pattern(512 * 1024)).unwrap();
    let started = Instant::now();
    p.upload(&s(&big), "paced.tmp", None).await.expect("upload");
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "not paced"
    );
    std::fs::write(fx.remote().join("paced.bin"), b"old").unwrap();
    p.replace("paced.tmp", "paced.bin").await.expect("replace");
    assert_eq!(
        std::fs::read(fx.remote().join("paced.bin")).unwrap().len(),
        512 * 1024
    );

    // Its parallel workers ride the connection the fallback went on with.
    assert_eq!(
        p.transfer_executor_kind(),
        ProviderTransferExecutorKind::SftpConnectionPool
    );
    let mut worker = p.clone_for_transfer().expect("worker");
    assert_eq!(
        worker.read_range("paced.bin", 1000, 64).await.unwrap(),
        pattern(512 * 1024)[1000..1064]
    );
    assert_eq!(seen.connections(), 1, "a worker dialled");
}

/// Workers for parallel transfers share the one connection, a ranged read
/// returns exactly its window, and a segmented download comes back whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workers_share_the_connection_and_a_segmented_download_is_whole() {
    use ftp_client_gui_lib::provider_transfer_executor::{
        provider_segmented_download_eligible, run_provider_segmented_download, SegmentCutoff,
    };
    use ftp_client_gui_lib::transfer_dag::Capability;

    let fx = Fixture::new("pool");
    let (port, seen) = start(fx.options(Sftp::Serve)).await;
    let mut profile = scp_profile(port, &fx.remote(), &[]);
    profile.password = Some(PASSWORD.to_string());
    let mut p = ProviderFactory::create(&profile).expect("factory");
    timed_connect(p.as_mut()).await.0.expect("connect");

    let caps = p.transfer_capabilities();
    assert_eq!(caps.file_parallel, Capability::Supported);
    assert_eq!(caps.session_pool, Capability::Supported);
    assert_eq!(caps.strict_concurrent_range_download, Capability::Supported);
    assert_eq!(caps.max_file_slots, Some(8));

    let big = pattern(3 * 1024 * 1024 + 17);
    std::fs::write(fx.remote().join("big.bin"), &big).unwrap();
    for i in 0..4 {
        std::fs::write(fx.remote().join(format!("f{i}.bin")), pattern(100_000 + i)).unwrap();
    }

    // Four downloads side by side, one worker each.
    let mut workers: Vec<_> = (0..4)
        .map(|_| p.clone_for_transfer().expect("worker"))
        .collect();
    let jobs = workers.iter_mut().enumerate().map(|(i, w)| {
        let dest = fx.local(&format!("f{i}.bin"));
        async move {
            w.download(&format!("f{i}.bin"), &s(&dest), None)
                .await
                .map(|()| dest)
        }
    });
    for (i, done) in futures_util::future::join_all(jobs)
        .await
        .into_iter()
        .enumerate()
    {
        let dest = done.expect("parallel download");
        assert_eq!(std::fs::read(dest).unwrap(), pattern(100_000 + i));
    }

    let w = &mut workers[0];
    assert_eq!(
        w.read_range("big.bin", 1000, 5000).await.unwrap(),
        big[1000..6000]
    );
    let len = big.len() as u64;
    assert_eq!(
        w.read_range("big.bin", len - 10, 100).await.unwrap(),
        big[big.len() - 10..]
    );
    assert!(w
        .read_range("big.bin", len + 5, 100)
        .await
        .unwrap()
        .is_empty());

    let segments = provider_segmented_download_eligible(
        p.as_ref(),
        len,
        4,
        4,
        SegmentCutoff::Explicit(1024 * 1024),
    )
    .expect("eligible for a segmented download");
    assert!(segments >= 2, "{segments}");
    let dest = fx.local("big.bin");
    let execs = seen.execs();
    run_provider_segmented_download(
        p.as_mut(),
        "big.bin",
        &s(&dest),
        len,
        segments,
        None,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("segmented download");
    assert_eq!(std::fs::read(&dest).unwrap(), big);
    // One ranged read per window at least: a single stream would be one scp.
    assert!(seen.execs() - execs >= segments, "not read in windows");

    assert_eq!(seen.connections(), 1, "a worker dialled");
    // A worker that ends leaves the connection to its owner.
    for mut w in workers {
        w.disconnect().await.unwrap();
    }
    p.list("").await.expect("the session is still connected");
}

/// `--multi-thread-streams` on a plain download: at or above the cutoff the
/// file is read in windows on the one connection, both on an SCP profile
/// tuned after connecting and on an SFTP profile tuned before it went on over
/// SCP; with one stream it is one `scp -f`.
#[tokio::test]
async fn a_plain_download_above_the_cutoff_is_read_in_windows() {
    for case in ["scp profile", "sftp profile over scp"] {
        let fx = Fixture::new(&format!("mt-{}", case.len()));
        let mode = if case == "scp profile" {
            Sftp::Serve
        } else {
            Sftp::Refuse
        };
        let (port, seen) = start(fx.options(mode)).await;
        let mut p: Box<dyn StorageProvider> = if case == "scp profile" {
            let mut profile = scp_profile(port, &fx.remote(), &[]);
            profile.password = Some(PASSWORD.to_string());
            ProviderFactory::create(&profile).expect("factory")
        } else {
            let mut p = SftpProvider::new(config(port, &fx.remote()));
            p.set_multi_thread_download(4, 1024 * 1024);
            Box::new(p)
        };
        timed_connect(p.as_mut()).await.0.expect(case);
        let big = pattern(3 * 1024 * 1024 + 17);
        std::fs::write(fx.remote().join("big.bin"), &big).unwrap();
        let len = big.len() as u64;

        if case == "scp profile" {
            let one = fx.local("one.bin");
            let before = seen.execs();
            p.download("big.bin", &s(&one), None).await.expect(case);
            assert_eq!(std::fs::read(&one).unwrap(), big, "{case}");
            assert_eq!(p.planned_download_segments(len), 0, "{case}");
            assert_eq!(seen.execs() - before, 2, "{case}: one stream ran windows");
            p.set_multi_thread_download(4, 1024 * 1024);
        }

        let segments = p.planned_download_segments(len);
        assert!(segments >= 2, "{case}: {segments}");
        let dest = fx.local("windows.bin");
        let reached = Arc::new(Mutex::new(0u64));
        let progress = {
            let reached = reached.clone();
            Box::new(move |done: u64, _total: u64| {
                let mut r = reached.lock().unwrap();
                *r = (*r).max(done);
            })
        };
        let before = seen.execs();
        p.download("big.bin", &s(&dest), Some(progress))
            .await
            .expect(case);
        assert_eq!(std::fs::read(&dest).unwrap(), big, "{case}");
        // The stat, then one ranged read per window at least; one stream is
        // the stat and one `scp -f`.
        assert!(
            seen.execs() - before > segments,
            "{case}: not read in windows ({} commands)",
            seen.execs() - before
        );
        assert_eq!(*reached.lock().unwrap(), len, "{case}: progress");
        assert_eq!(seen.connections(), 1, "{case}: a window dialled");
        p.disconnect().await.unwrap();
    }
}

/// A caller that saw the object move asks for the time of what the next
/// download read, so the local file does not take a listing that described
/// another object: `scp -p` sends that time with the bytes. A plain download
/// keeps the caller's; after `expect_moved_source` the download reads one
/// stream, even with windows asked for, and hands its time over. On an SCP
/// profile and on an SFTP profile over SCP.
#[tokio::test]
async fn a_download_after_a_refusal_hands_over_the_time_it_read() {
    for case in ["scp profile", "sftp profile over scp"] {
        let fx = Fixture::new(&format!("time-{}", case.len()));
        let mode = if case == "scp profile" {
            Sftp::Serve
        } else {
            Sftp::Refuse
        };
        let (port, seen) = start(fx.options(mode)).await;
        let mut p: Box<dyn StorageProvider> = if case == "scp profile" {
            let mut profile = scp_profile(port, &fx.remote(), &[]);
            profile.password = Some(PASSWORD.to_string());
            ProviderFactory::create(&profile).expect("factory")
        } else {
            Box::new(SftpProvider::new(config(port, &fx.remote())))
        };
        timed_connect(p.as_mut()).await.0.expect(case);
        let big = pattern(3 * 1024 * 1024 + 17);
        let file = fx.remote().join("big.bin");
        std::fs::write(&file, &big).unwrap();
        // 2021-01-02 03:04:05 UTC
        let when = std::time::UNIX_EPOCH + Duration::from_secs(1_609_556_645);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(when)
            .unwrap();
        let sent = Some("2021-01-02 03:04:05Z");

        p.download("big.bin", &s(&fx.local("plain.bin")), None)
            .await
            .expect(case);
        assert!(!p.download_was_refused(), "{case}");
        assert_eq!(
            p.take_download_time(false),
            None,
            "{case}: a plain download"
        );
        assert_eq!(p.take_download_time(true).as_deref(), sent, "{case}");

        p.set_multi_thread_download(4, 1024 * 1024);
        p.expect_moved_source();
        let dest = fx.local("moved.bin");
        let before = seen.execs();
        p.download("big.bin", &s(&dest), None).await.expect(case);
        assert_eq!(std::fs::read(&dest).unwrap(), big, "{case}");
        assert!(p.download_was_refused(), "{case}");
        assert_eq!(seen.execs() - before, 2, "{case}: windows after a refusal");
        assert_eq!(p.take_download_time(false).as_deref(), sent, "{case}");
        assert_eq!(p.take_download_time(false), None, "{case}: taken once");
        p.disconnect().await.unwrap();
    }
}
