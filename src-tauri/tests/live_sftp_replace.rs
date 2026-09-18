//! What an SFTP server does when a rename lands on a name that is already taken,
//! and the two different answers AeroFTP owes for it.
//!
//! `SSH_FXP_RENAME` in SFTP protocol 3 is specified to FAIL when the
//! destination exists, and servers answer it with a bare `SSH_FX_FAILURE`.
//! That single fact has two consequences pulling in opposite directions, and
//! this file pins both so a later change cannot quietly trade one for the
//! other:
//!
//! - **`rename` must keep failing.** It is the verb behind the user's "rename",
//!   and no caller in this tree checks whether the destination is occupied
//!   first. If it gained overwrite semantics, `mv a.txt b.txt` would delete an
//!   existing `b.txt` without asking. The server's refusal is the whole guard.
//! - **`replace` must succeed.** It is the verb behind "publish a staged
//!   temporary over the live file", where the destination is meant to go.
//!   Before G119 it did not exist and every such caller used `rename`, so CLI
//!   and MCP `edit`, the AeroCrypt marker publish and the crypt configuration
//!   writes all failed on every SFTP server.
//!
//! The control that settles the diagnosis is in the third assertion: the same
//! overwrite, on the same server in the same run, is refused one way and
//! accepted the other. A server without `posix-rename@openssh.com` refuses
//! both, which is a fact about that server and is asserted as such.
//!
//! ```bash
//! AEROFTP_LIVE_SFTP_HOST=example.test \
//! AEROFTP_LIVE_SFTP_USER=someone \
//! AEROFTP_LIVE_SFTP_KEY=$HOME/.ssh/id_ed25519 \
//! AEROFTP_LIVE_SFTP_DIR=/home/someone \
//!   cargo test --test live_sftp_replace -- --ignored --nocapture
//! ```
//!
//! `AEROFTP_LIVE_SFTP_PORT` defaults to 22 and `AEROFTP_LIVE_SFTP_PASS` is an
//! alternative to the key. The scratch directory is created under
//! `AEROFTP_LIVE_SFTP_DIR` and removed at the end.

use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{SftpProvider, StorageProvider};

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

async fn connected() -> SftpProvider {
    let key = std::env::var("AEROFTP_LIVE_SFTP_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let password = std::env::var("AEROFTP_LIVE_SFTP_PASS")
        .ok()
        .filter(|p| !p.is_empty())
        .map(secrecy::SecretString::from);
    assert!(
        key.is_some() || password.is_some(),
        "set AEROFTP_LIVE_SFTP_KEY or AEROFTP_LIVE_SFTP_PASS"
    );
    let mut provider = SftpProvider::new(SftpConfig {
        host: env_or("AEROFTP_LIVE_SFTP_HOST", "127.0.0.1"),
        port: env_or("AEROFTP_LIVE_SFTP_PORT", "22")
            .parse()
            .expect("AEROFTP_LIVE_SFTP_PORT"),
        username: env_or("AEROFTP_LIVE_SFTP_USER", "root"),
        password,
        private_key_path: key,
        key_passphrase: None,
        initial_path: None,
        timeout_secs: 30,
        trust_unknown_hosts: true,
    });
    provider
        .connect()
        .await
        .expect("the live SFTP server must be reachable");
    provider
}

async fn put(provider: &mut SftpProvider, remote: &str, bytes: &[u8]) {
    let local = std::env::temp_dir().join(format!("aeroftp-live-replace-{}", uuid::Uuid::new_v4()));
    std::fs::write(&local, bytes).expect("stage locally");
    provider
        .upload(&local.to_string_lossy(), remote, None)
        .await
        .unwrap_or_else(|e| panic!("upload {remote}: {e}"));
    let _ = std::fs::remove_file(&local);
}

#[tokio::test]
#[ignore = "needs a real SFTP server; see the module comment"]
async fn rename_refuses_an_occupied_destination_and_replace_does_not() {
    let mut provider = connected().await;

    let base = env_or("AEROFTP_LIVE_SFTP_DIR", "/tmp");
    let dir = format!("{base}/aeroftp-live-replace-{}", uuid::Uuid::new_v4());
    provider.mkdir(&dir).await.expect("scratch dir");

    let a = format!("{dir}/a.txt");
    let b = format!("{dir}/b.txt");
    let c = format!("{dir}/c.txt");
    put(&mut provider, &a, b"alpha\n").await;
    put(&mut provider, &b, b"beta\n").await;

    // 1. A free destination: plain rename is the whole answer and nothing
    //    below is about to prove anything unless this works.
    provider
        .rename(&a, &c)
        .await
        .expect("renaming onto a free name must work");

    // 2. An occupied destination: rename must refuse. This is the guard that
    //    keeps `mv` from destroying a file, and it is asserted rather than
    //    assumed because the fix for `replace` runs through the same provider.
    let refused = provider
        .rename(&c, &b)
        .await
        .expect_err("renaming onto an occupied name must be refused");
    eprintln!("MEASURED rename onto an occupied name: {refused}");

    // 3. The same overwrite through the verb that is allowed to do it.
    let atomic = provider
        .supports_atomic_replace()
        .await
        .expect("asking about atomic replace must not fail");
    eprintln!("MEASURED posix-rename advertised: {atomic}");

    if !atomic {
        let err = provider
            .replace(&c, &b)
            .await
            .expect_err("without the extension `replace` must refuse, not improvise");
        let text = err.to_string();
        assert!(
            text.contains("posix-rename@openssh.com"),
            "the refusal must name what the server is missing, got: {text}"
        );
        assert!(
            text.contains("unchanged"),
            "the refusal must say the destination was not touched, got: {text}"
        );
        eprintln!("MEASURED server without the extension; refusal: {text}");
    } else {
        provider
            .replace(&c, &b)
            .await
            .expect("with the extension `replace` must put one file over the other");
        let after = provider
            .download_to_bytes(&b)
            .await
            .expect("read the replaced file");
        assert_eq!(
            after, b"alpha\n",
            "the destination must hold the source's bytes after a replace"
        );
        assert!(
            !provider.exists(&c).await.expect("exists after replace"),
            "the source must be gone after a replace, the way a rename leaves nothing behind"
        );
    }

    let _ = provider.rmdir_recursive(&dir).await;
    let _ = provider.disconnect().await;
}
