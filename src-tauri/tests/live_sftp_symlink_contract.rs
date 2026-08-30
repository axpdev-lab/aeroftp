//! Live characterisation of what `list()` produces for a symlink to a directory.
//!
//! This is NOT the premise of a fix: the `find` walk reads `read_dir`
//! attributes and never sees `is_dir == true` for a link. `list()` is the one
//! path that resolves the target and sets `is_symlink` separately, and it is
//! therefore the only place where `is_walkable_dir()` differs from `is_dir` on
//! any provider in this tree. That is the fact worth pinning.
//!
//! `#[ignore]` by default: it needs the sftp-rsync Docker fixture up, so it is
//! NOT part of the gate. It is run by hand and its output reported, because what
//! it measures is not our behaviour but what the SFTP provider produces against
//! a real server, and that is a question to ask when writing the code rather
//! than on every push.
//!
//!   cd src-tauri/tests/fixtures/sftp-rsync && ./setup.sh
//!   DOCKER_BUILDKIT=0 docker build -t aeroftp-sftp-probe .
//!   docker run -d --name aeroftp-sftp-probe -p 127.0.0.1:2225:22 \
//!       -v "$PWD/ssh_key.pub:/mnt/authorized_keys:ro" aeroftp-sftp-probe
//!   docker exec aeroftp-sftp-probe sh -c 'mkdir -p /workdir/probe/realdir && \
//!       ln -sfn realdir /workdir/probe/linkdir && chown -R testuser /workdir/probe'
//!   cargo test --test live_sftp_symlink_contract -- --ignored --nocapture
use ftp_client_gui_lib::providers::{ProviderConfig, ProviderFactory, ProviderType};

#[tokio::test]
#[ignore = "live: needs the sftp-rsync fixture on :2225 with /workdir/probe/linkdir -> realdir"]
async fn a_symlink_to_a_directory_reports_both_is_dir_and_is_symlink() {
    let key = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/sftp-rsync/ssh_key"
    );
    let mut extra = std::collections::HashMap::new();
    extra.insert("private_key_path".to_string(), key.to_string());
    extra.insert("trust_host_key".to_string(), "true".to_string());
    let cfg = ProviderConfig {
        name: "prd-symlink-probe".to_string(),
        provider_type: ProviderType::Sftp,
        host: "127.0.0.1".to_string(),
        port: Some(2225),
        username: Some("testuser".to_string()),
        password: None,
        initial_path: Some("/".to_string()),
        extra,
    };

    let mut p = ProviderFactory::create(&cfg).expect("provider");
    p.connect().await.expect("connect to the fixture");
    let entries = p.list("/workdir/probe").await.expect("list");

    let link = entries
        .iter()
        .find(|e| e.name == "linkdir")
        .expect("linkdir present");
    println!(
        "MEASURED linkdir: is_dir={} is_symlink={} link_target={:?} perms={:?}",
        link.is_dir, link.is_symlink, link.link_target, link.permissions
    );

    // The defect: the walk sees a directory.
    assert!(
        link.is_dir,
        "SFTP resolves the target, so a link to a dir reports is_dir"
    );
    // The fix: the contract has something to refuse it on.
    assert!(
        link.is_symlink,
        "and it must also report is_symlink, or is_walkable_dir cannot help"
    );
    assert!(
        !link.is_walkable_dir(),
        "so the contract refuses the descent"
    );
}
