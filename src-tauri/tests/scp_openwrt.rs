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

    p.rename(&format!("{dir}/a b.txt"), &format!("{dir}/renamed it's.txt"))
        .await
        .expect("rename");
    assert!(p.exists(&format!("{dir}/renamed it's.txt")).await.unwrap());
    assert!(!p.exists(&format!("{dir}/a b.txt")).await.unwrap());
    p.delete(&format!("{dir}/-dash.txt")).await.expect("delete");
    assert!(!p.exists(&format!("{dir}/-dash.txt")).await.unwrap());
    p.rmdir_recursive(&dir).await.expect("rmdir -r");
    assert!(!p.exists(&dir).await.unwrap());
    assert!(!p.exists("pwned").await.unwrap(), "a name ran as a command");
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
    assert!(sftp.server_info().await.unwrap().starts_with("SCP over SSH"));
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
    let key = russh::keys::PrivateKey::random(
        &mut rand_010::rng(),
        russh::keys::Algorithm::Ed25519,
    )
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
