//! `ScpSession` against a russh server whose exec channels run the real
//! `sh`, `ls`, `find` and OpenSSH `scp -t`/`scp -f` of this machine in a
//! temporary folder, the way a router's Dropbear runs BusyBox.
//!
//! Linux only: the listing needs an `ls` with `--full-time` (GNU, BusyBox,
//! uutils), which macOS does not ship.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use ftp_client_gui_lib::providers::scp::listing::Kind;
use ftp_client_gui_lib::providers::scp::session::ScpSession;
use ftp_client_gui_lib::providers::ProviderError;
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Session};
use russh::{Channel, ChannelId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};
use tokio::sync::Mutex;

/// Runs every exec request as `sh -c <command>` in `cwd` with `path` as
/// `PATH`, wiring the channel to the process's stdin, stdout and stderr.
struct ExecServer {
    cwd: PathBuf,
    path: String,
    stdins: Arc<Mutex<HashMap<ChannelId, ChildStdin>>>,
}

impl Handler for ExecServer {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // By absolute path: `PATH` below is the server's, which a test narrows.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(OsStr::from_bytes(data))
            .current_dir(&self.cwd)
            .env("PATH", &self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh");
        self.stdins
            .lock()
            .await
            .insert(channel, child.stdin.take().unwrap());
        let _ = session.channel_success(channel);
        let handle = session.handle();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        tokio::spawn(async move {
            let h = handle.clone();
            let err_task = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let _ = h.extended_data(channel, 1, buf[..n].to_vec()).await;
                }
            });
            let mut buf = vec![0u8; 32768];
            while let Ok(n) = stdout.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let _ = handle.data(channel, buf[..n].to_vec()).await;
            }
            let _ = err_task.await;
            let status = child
                .wait()
                .await
                .map(|s| s.code().unwrap_or(255))
                .unwrap_or(255);
            let _ = handle.exit_status_request(channel, status as u32).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(stdin) = self.stdins.lock().await.get_mut(&channel) {
            let _ = stdin.write_all(data).await;
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Dropping the pipe is the process's end of input.
        self.stdins.lock().await.remove(&channel);
        Ok(())
    }
}

struct Client;

impl russh::client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

type Shared = Arc<tokio::sync::Mutex<russh::client::Handle<Client>>>;

/// A server in `cwd` with `path` as `PATH`, and a client authenticated to it.
async fn connect(cwd: &Path, path: &str) -> Shared {
    let key =
        russh::keys::PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519)
            .expect("ed25519 key");
    let config = Arc::new(russh::server::Config {
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (cwd, path) = (cwd.to_path_buf(), path.to_string());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let handler = ExecServer {
                cwd: cwd.clone(),
                path: path.clone(),
                stdins: Arc::default(),
            };
            let cfg = config.clone();
            tokio::spawn(async move {
                let _ = russh::server::run_stream(cfg, stream, handler).await;
            });
        }
    });
    let mut handle = russh::client::connect(
        Arc::new(russh::client::Config::default()),
        ("127.0.0.1", port),
        Client,
    )
    .await
    .expect("connect");
    let auth = handle.authenticate_none("root").await.expect("auth");
    assert!(matches!(auth, russh::client::AuthResult::Success));
    Arc::new(tokio::sync::Mutex::new(handle))
}

fn system_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string())
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scp-session-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

const IO: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_list_and_folder_operations_run_on_the_real_tools() {
    let root = temp_dir("ops");
    let scp = ScpSession::open(connect(&root, &system_path()).await, IO)
        .await
        .expect("probe");
    assert_eq!(scp.probe().home, s(&root));
    assert!(scp.probe().has("scp") && scp.probe().has("ls-full-time"));

    let names: Vec<&[u8]> = vec![
        b"a b",
        b"it's",
        b"-dash",
        b"new\nline",
        b"x -> y",
        b"$(touch pwned)",
    ];
    let dir = root.join("list");
    std::fs::create_dir(&dir).unwrap();
    for (i, name) in names.iter().enumerate() {
        std::fs::write(dir.join(OsStr::from_bytes(name)), vec![b'x'; i]).unwrap();
    }
    std::os::unix::fs::symlink("a b", dir.join("link")).unwrap();
    std::fs::create_dir(dir.join("sub")).unwrap();
    let entries = scp.list(&s(&dir)).await.expect("list");
    assert!(!root.join("pwned").exists() && !dir.join("pwned").exists());
    assert_eq!(entries.len(), names.len() + 2, "{entries:?}");
    for (i, name) in names.iter().enumerate() {
        let e = entries.iter().find(|e| e.name == *name).expect("listed");
        assert_eq!((e.meta.kind, e.meta.size), (Kind::File, i as u64));
    }
    let link = entries.iter().find(|e| e.name == b"link").unwrap();
    assert_eq!(link.meta.kind, Kind::Symlink);
    assert_eq!(
        entries.iter().find(|e| e.name == b"sub").unwrap().meta.kind,
        Kind::Dir
    );

    // A symlink to a folder lists the folder.
    std::os::unix::fs::symlink(&dir, root.join("via-link")).unwrap();
    assert_eq!(
        scp.list(&s(&root.join("via-link"))).await.unwrap().len(),
        entries.len()
    );

    let d = s(&root.join("made dir"));
    scp.mkdir(&d).await.expect("mkdir");
    assert_eq!(scp.stat(&d).await.unwrap().kind, Kind::Dir);
    assert!(matches!(
        scp.mkdir(&d).await,
        Err(ProviderError::AlreadyExists(_))
    ));
    let f = format!("{d}/it's here");
    std::fs::write(&f, b"1234").unwrap();
    assert!(matches!(
        scp.remove_dir(&d).await,
        Err(ProviderError::DirectoryNotEmpty(_))
    ));
    let g = format!("{d}/renamed");
    scp.rename(&f, &g).await.expect("rename");
    // An existing file or folder as the target is refused, never replaced
    // or entered.
    let other = format!("{d}/other");
    std::fs::write(&other, b"keep").unwrap();
    assert!(matches!(
        scp.rename(&g, &other).await,
        Err(ProviderError::AlreadyExists(_))
    ));
    assert_eq!(std::fs::read(&other).unwrap(), b"keep");
    // The target is quoted in the existence check too: a name that would
    // run a command runs nothing.
    let hostile = format!("{d}/$(touch pwned)");
    std::fs::write(&hostile, b"").unwrap();
    assert!(matches!(
        scp.rename(&g, &hostile).await,
        Err(ProviderError::AlreadyExists(_))
    ));
    assert!(!root.join("pwned").exists() && !std::path::Path::new(&format!("{d}/pwned")).exists());
    std::fs::remove_file(&hostile).unwrap();
    let inner = format!("{d}/inner");
    std::fs::create_dir(&inner).unwrap();
    assert!(matches!(
        scp.rename(&g, &inner).await,
        Err(ProviderError::AlreadyExists(_))
    ));
    assert!(std::path::Path::new(&g).exists());
    std::fs::remove_file(&other).unwrap();
    std::fs::remove_dir(&inner).unwrap();
    assert_eq!(scp.stat(&g).await.unwrap().size, 4);
    scp.chmod(&g, 0o600).await.expect("chmod");
    assert_eq!(scp.stat(&g).await.unwrap().mode, 0o600);
    scp.remove_file(&g).await.expect("rm");
    assert!(matches!(
        scp.stat(&g).await,
        Err(ProviderError::NotFound(_))
    ));
    scp.remove_dir(&d).await.expect("rmdir");
    assert!(matches!(
        scp.list(&d).await,
        Err(ProviderError::NotFound(_))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_goes_up_and_comes_back_byte_exact_with_its_mtime() {
    let root = temp_dir("xfer");
    let scp = ScpSession::open(connect(&root, &system_path()).await, IO)
        .await
        .expect("probe");
    // Not a multiple of any buffer, every byte value present.
    let data: Vec<u8> = (0..300_001u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let path = s(&root.join("up it's.bin"));
    let mut sent = 0;
    scp.upload(
        &path,
        &mut &data[..],
        data.len() as u64,
        0o640,
        Some(1_700_000_000),
        &mut |n| sent = n,
    )
    .await
    .expect("upload");
    assert_eq!(sent, data.len() as u64);
    assert_eq!(std::fs::read(&path).unwrap(), data);
    let meta = scp.stat(&path).await.unwrap();
    assert_eq!((meta.mtime, meta.size), (1_700_000_000, data.len() as u64));

    let mut back = Vec::new();
    let (size, mtime) = scp
        .download(&path, &mut back, &mut |_| {})
        .await
        .expect("download");
    assert_eq!((size, mtime), (data.len() as u64, Some(1_700_000_000)));
    assert_eq!(back, data);

    // A source that ends early is an error, not a short file reported done.
    let short = s(&root.join("short"));
    let err = scp
        .upload(&short, &mut &data[..10], 100, 0o644, None, &mut |_| {})
        .await
        .expect_err("short source");
    assert!(err.to_string().contains("10 of 100"), "{err}");

    let missing = s(&root.join("missing"));
    let err = scp
        .download(&missing, &mut Vec::new(), &mut |_| {})
        .await
        .expect_err("missing");
    assert!(matches!(err, ProviderError::NotFound(_)), "{err:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A server whose `scp` answers with another file's name (CVE-2019-6111)
/// gets its data refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_download_refuses_a_file_the_server_renamed() {
    let root = temp_dir("cve");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("scp");
    std::fs::write(&fake, "#!/bin/sh\nhead -c 1 >/dev/null\nprintf 'C0644 5 .bashrc\\n'\nhead -c 1 >/dev/null\nprintf 'evil\\n\\0'\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", s(&bin), system_path());
    let scp = ScpSession::open(connect(&root, &path).await, IO)
        .await
        .expect("probe");
    let mut sink = Vec::new();
    let err = scp
        .download(&s(&root.join("wanted.txt")), &mut sink, &mut |_| {})
        .await
        .expect_err("renamed file");
    assert!(err.to_string().contains("not the file asked for"), "{err}");
    assert!(sink.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// A server without `scp` is refused at the probe, by name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_without_scp_is_refused_naming_it() {
    let root = temp_dir("noscp");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    for tool in ["ls", "find", "uname", "mkdir", "rm", "rmdir", "mv", "chmod"] {
        let found = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {tool}"))
            .output()
            .unwrap();
        let target = String::from_utf8(found.stdout).unwrap();
        std::os::unix::fs::symlink(target.trim(), bin.join(tool)).unwrap();
    }
    let err = ScpSession::open(connect(&root, &s(&bin)).await, IO)
        .await
        .err()
        .expect("no scp");
    assert!(
        matches!(&err, ProviderError::NotSupported(m) if m.contains("scp") && !m.contains("find")),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
