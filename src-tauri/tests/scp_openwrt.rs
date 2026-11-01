//! A stock OpenWrt router: Dropbear, BusyBox, a blank root password and no
//! sftp-server. The Delta Sync Integration workflow runs the official
//! `openwrt/rootfs` image in Docker and names it in `AEROFTP_OPENWRT_PORT` and
//! `AEROFTP_OPENWRT_CONTAINER`; elsewhere the test is ignored.
//!
//! The router comes as shipped first: an SFTP profile with no password must
//! get in (the `none` method) and go on over SCP, and so must the SCP
//! protocol. Then it is given a root password and an authorized key through
//! `docker exec`, and both sign-ins are tried.
#![cfg(target_os = "linux")]

#[path = "support/scoped_env.rs"]
mod scoped_env;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{
    ProviderConfig, ProviderFactory, ProviderType, StorageProvider,
};
use russh::keys::ssh_key::LineEnding;
use scoped_env::ScopedEnv;

const PASSWORD: &str = "owrt-ci-5b1e-not-in-any-log";
const PASSPHRASE: &str = "owrt-ci-pp-3c7d-not-in-any-log";

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

fn port() -> u16 {
    std::env::var("AEROFTP_OPENWRT_PORT")
        .expect("AEROFTP_OPENWRT_PORT")
        .parse()
        .expect("a port")
}

fn container() -> String {
    std::env::var("AEROFTP_OPENWRT_CONTAINER").expect("AEROFTP_OPENWRT_CONTAINER")
}

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

/// A shell command in the router, as root; panics if it fails.
fn on_router(script: &str, stdin: &[u8]) {
    use std::io::Write;
    let mut child = Command::new("docker")
        .args(["exec", "-i", &container(), "sh", "-c", script])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("docker exec");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    assert!(child.wait().unwrap().success(), "on the router: {script}");
}

/// Puts the router back to its stock login (root with no password, no
/// authorized key) when dropped, failed test included: Dropbear refuses
/// `none` once root has a password, so a rerun on the same container would
/// stop at its first connect.
struct StockLogin;

impl Drop for StockLogin {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args([
                "exec",
                &container(),
                "sh",
                "-c",
                "passwd -d root >/dev/null 2>&1; rm -f /etc/dropbear/authorized_keys",
            ])
            .status();
    }
}

fn sftp_config(password: Option<&str>, key: Option<&Path>) -> SftpConfig {
    SftpConfig {
        host: "127.0.0.1".to_string(),
        port: port(),
        username: "root".to_string(),
        password: password.map(|p| secrecy::SecretString::from(p.to_string())),
        private_key_path: key.map(s),
        key_passphrase: key.map(|_| secrecy::SecretString::from(PASSPHRASE.to_string())),
        initial_path: None,
        timeout_secs: 20,
        trust_unknown_hosts: true,
    }
}

async fn connect(p: &mut dyn StorageProvider) {
    tokio::time::timeout(Duration::from_secs(30), p.connect())
        .await
        .expect("connect hung")
        .expect("connect");
}

/// List, upload, download, rename and delete under hostile names, checked
/// through the provider: the files live in the router.
async fn exercise(p: &mut dyn StorageProvider, local: &Path, tag: &str) {
    let dir = format!("aeroftp ci {tag}");
    p.mkdir(&dir).await.expect("mkdir");
    for (i, name) in HOSTILE.iter().enumerate() {
        let body = format!("{i}:{name}").repeat(200 + i);
        let up = local.join(format!("up-{tag}-{i}"));
        std::fs::write(&up, &body).unwrap();
        let target = format!("{dir}/{name}");
        p.upload(&s(&up), &target, None)
            .await
            .unwrap_or_else(|e| panic!("upload {name}: {e}"));
        let back = p
            .download_to_bytes(&target)
            .await
            .unwrap_or_else(|e| panic!("download {name}: {e}"));
        assert_eq!(back, body.as_bytes(), "{name}");
    }
    let mut names: Vec<String> = p
        .list(&dir)
        .await
        .expect("list")
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort_unstable();
    let mut want: Vec<String> = HOSTILE.iter().map(|n| n.to_string()).collect();
    want.sort_unstable();
    assert_eq!(names, want);

    p.rename(
        &format!("{dir}/a b.txt"),
        &format!("{dir}/renamed it's.txt"),
    )
    .await
    .expect("rename");
    assert!(p.exists(&format!("{dir}/renamed it's.txt")).await.unwrap());
    assert!(!p.exists(&format!("{dir}/a b.txt")).await.unwrap());
    p.delete(&format!("{dir}/-dash.txt")).await.expect("delete");
    assert!(!p.exists(&format!("{dir}/-dash.txt")).await.unwrap());
    operations(p, local, &dir).await;
    p.rmdir_recursive(&dir).await.expect("rmdir -r");
    assert!(!p.exists(&dir).await.unwrap());
    assert!(!p.exists("pwned").await.unwrap(), "a name ran as a command");
}

/// What BusyBox's tools give beyond moving files: a replace, the free
/// space, digests, a search, and transfers that go on from a partial.
async fn operations(p: &mut dyn StorageProvider, local: &Path, dir: &str) {
    use md5::Md5;
    use sha2::{Digest, Sha256};

    let body: Vec<u8> = (0..1024 * 1024).map(|i| (i * 7 + i / 251) as u8).collect();
    let file = local.join(format!("body-{}", dir.len()));
    std::fs::write(&file, &body).unwrap();

    // Replace: the staged file takes the target's place.
    let target = format!("{dir}/target it's.bin");
    let staged = format!("{dir}/staged $(x).tmp");
    p.upload(&s(&file), &staged, None).await.expect("stage");
    std::fs::write(local.join("old"), b"old").unwrap();
    p.upload(&s(&local.join("old")), &target, None)
        .await
        .unwrap();
    assert!(p.supports_atomic_replace().await.unwrap());
    p.replace(&staged, &target).await.expect("replace");
    assert!(!p.exists(&staged).await.unwrap());
    assert_eq!(p.size(&target).await.unwrap(), body.len() as u64);

    let space = p.storage_info().await.expect("df");
    assert!(space.total > 0 && space.free <= space.total);

    let sums = p.checksum(&target).await.unwrap();
    assert_eq!(sums["sha256"], hex::encode(Sha256::digest(&body)));
    assert_eq!(sums["md5"], hex::encode(Md5::digest(&body)));

    let found = p.find(dir, "target*").await.expect("find");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "target it's.bin");

    // An upload that stopped half way goes on by appending.
    let half = local.join("half");
    std::fs::write(&half, &body[..body.len() / 2]).unwrap();
    let appended = format!("{dir}/appended.bin");
    p.upload(&s(&half), &appended, None).await.unwrap();
    p.resume_upload(&s(&file), &appended, (body.len() / 2) as u64, None)
        .await
        .expect("append");
    assert_eq!(p.download_to_bytes(&appended).await.unwrap(), body);

    // A download stopped part way goes on from its partial: paced to 256
    // KiB/s, it is stopped once the partial holds 64 KiB.
    let down = local.join(format!("down-{}", dir.len()));
    p.set_speed_limit(0, 256).await.unwrap();
    {
        let down_path = s(&down);
        let download = p.download(&target, &down_path, None);
        tokio::pin!(download);
        loop {
            tokio::select! {
                done = &mut download => panic!("finished before it was stopped: {done:?}"),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    let temp = down.with_file_name(format!(
                        "{}.aerotmp",
                        down.file_name().unwrap().to_str().unwrap()
                    ));
                    if std::fs::metadata(&temp).map(|m| m.len()).unwrap_or(0) >= 64 * 1024 {
                        break;
                    }
                }
            }
        }
    }
    p.set_speed_limit(0, 0).await.unwrap();
    let part = ftp_client_gui_lib::providers::atomic_write::resumable_part_len(&down).await;
    assert!(part >= 64 * 1024, "part {part}");
    let first = Arc::new(Mutex::new(None));
    let seen = first.clone();
    p.download(
        &target,
        &s(&down),
        Some(Box::new(move |done, _| {
            seen.lock().unwrap().get_or_insert(done);
        })),
    )
    .await
    .expect("go on");
    assert_eq!(std::fs::read(&down).unwrap(), body);
    let first = first.lock().unwrap().expect("progress");
    assert!(first > part, "went on from {first}, the part held {part}");

    parallel(p, local, dir, &target, &body).await;
}

/// Four workers on Dropbear's one connection, then a segmented download.
async fn parallel(p: &mut dyn StorageProvider, local: &Path, dir: &str, target: &str, body: &[u8]) {
    use ftp_client_gui_lib::provider_transfer_executor::{
        provider_segmented_download_eligible, run_provider_segmented_download, SegmentCutoff,
    };

    let mut workers: Vec<_> = (0..4)
        .map(|_| p.clone_for_transfer().expect("worker"))
        .collect();
    let jobs = workers.iter_mut().enumerate().map(|(i, w)| {
        let dest = local.join(format!("parallel-{i}"));
        async move { w.download(target, &s(&dest), None).await.map(|()| dest) }
    });
    for done in futures_util::future::join_all(jobs).await {
        assert_eq!(
            std::fs::read(done.expect("parallel download")).unwrap(),
            body
        );
    }
    assert_eq!(
        workers[0].read_range(target, 4097, 300).await.unwrap(),
        body[4097..4397]
    );
    drop(workers);

    // Windows are at least 1 MiB, so the segmented file is larger.
    let big: Vec<u8> = (0..3 * 1024 * 1024 + 17)
        .map(|i| (i * 13 + i / 509) as u8)
        .collect();
    let source = local.join("big-source");
    std::fs::write(&source, &big).unwrap();
    let remote = format!("{dir}/big file.bin");
    p.upload(&s(&source), &remote, None)
        .await
        .expect("upload big");
    let len = big.len() as u64;
    let segments =
        provider_segmented_download_eligible(&*p, len, 4, 4, SegmentCutoff::Explicit(256 * 1024))
            .expect("eligible for a segmented download");
    let dest = local.join("segmented");
    run_provider_segmented_download(
        p,
        &remote,
        &s(&dest),
        len,
        segments,
        None,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("segmented download");
    assert_eq!(std::fs::read(&dest).unwrap(), big);
    p.delete(&remote).await.expect("delete big");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the OpenWrt container of the Delta Sync Integration workflow"]
async fn stock_openwrt_is_reached_over_scp_with_its_defaults() {
    let root: PathBuf = std::env::temp_dir().join(format!("scp-openwrt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home/.ssh")).unwrap();
    std::fs::create_dir_all(root.join("local")).unwrap();
    // An accepted host key lands here, not in the runner's known_hosts.
    let _home = ScopedEnv::set("HOME", root.join("home"));
    let local = root.join("local");

    // As shipped: blank root password, SFTP profile with defaults.
    let mut sftp = SftpProvider::new(sftp_config(None, None));
    connect(&mut sftp).await;
    assert!(sftp.in_scp_mode(), "stock OpenWrt has no SFTP");
    assert!(sftp
        .server_info()
        .await
        .unwrap()
        .starts_with("SCP over SSH"));
    exercise(&mut sftp, &local, "sftp").await;
    sftp.disconnect().await.unwrap();

    // The SCP protocol, through the factory as the GUI, CLI and MCP build it.
    let profile = ProviderConfig {
        name: "router".to_string(),
        provider_type: ProviderType::Scp,
        host: "127.0.0.1".to_string(),
        port: Some(port()),
        username: Some("root".to_string()),
        password: None,
        initial_path: None,
        extra: [("trust_unknown_hosts".to_string(), "true".to_string())]
            .into_iter()
            .collect(),
    };
    let mut scp = ProviderFactory::create(&profile).expect("scp provider");
    connect(scp.as_mut()).await;
    exercise(scp.as_mut(), &local, "scp").await;
    scp.disconnect().await.unwrap();

    // A root password and an authorized key, as a configured router has.
    let _stock = StockLogin;
    let key =
        russh::keys::PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519)
            .unwrap();
    let key_path = root.join("id_router");
    let encrypted = key.encrypt(&mut rand_010::rng(), PASSPHRASE).unwrap();
    std::fs::write(&key_path, encrypted.to_openssh(LineEnding::LF).unwrap()).unwrap();
    on_router(
        "cat > /etc/dropbear/authorized_keys && chmod 600 /etc/dropbear/authorized_keys",
        key.public_key().to_openssh().unwrap().as_bytes(),
    );
    on_router(
        "passwd root >/dev/null 2>&1",
        format!("{PASSWORD}\n{PASSWORD}\n").as_bytes(),
    );

    let mut with_password = SftpProvider::new(sftp_config(Some(PASSWORD), None));
    connect(&mut with_password).await;
    assert!(with_password.in_scp_mode());
    exercise(&mut with_password, &local, "password").await;
    with_password.disconnect().await.unwrap();

    let mut with_key = SftpProvider::new(sftp_config(None, Some(&key_path)));
    connect(&mut with_key).await;
    assert!(with_key.in_scp_mode());
    exercise(&mut with_key, &local, "key").await;
    with_key.disconnect().await.unwrap();

    let _ = std::fs::remove_dir_all(&root);
}
