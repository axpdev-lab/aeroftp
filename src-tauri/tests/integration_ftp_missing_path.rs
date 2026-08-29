//! Live checks for the three FTP behaviours that cannot be proved without a
//! server, against the repo's vsftpd Docker fixture.
//!
//! `#[ignore]` so a default `cargo test` skips them, following the pattern of
//! `integration_ftp_pool.rs`.
//!
//! ```bash
//! cd src-tauri/tests/fixtures/ftp
//! docker compose up -d --build            # control :2123
//! cd ../../.. && cargo test --test integration_ftp_missing_path -- --ignored --nocapture
//! cd tests/fixtures/ftp && docker compose down -v
//! ```
//!
//! If every test here fails with "Connection refused", the fixture container
//! has exited, not the code under test: it has been seen to die on its own
//! mid-run. `docker ps -a` shows it as status 139. Restart it and re-run. The
//! compose file asks for `restart: on-failure` so this heals itself, but a
//! container started by hand without that flag will not.
//!
//! Why these three need a server. FTP answers a LIST against a directory that
//! does not exist with a successful, empty listing, so "missing" and "empty"
//! are the same bytes on the wire: no parser test can tell them apart, because
//! the difference is not in the text. The same goes for the working directory
//! left behind by a listing, and for a size that only the server can supply.
//! Each was previously compensated for in the CLI, so each was correct on one
//! surface out of three, and none had a test that would fail if the
//! compensation were removed without the fix.

use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode, ProviderError};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};

const PORT: u16 = 2123;

fn fixture_config() -> FtpConfig {
    FtpConfig {
        host: "127.0.0.1".to_string(),
        port: PORT,
        username: "testuser".to_string(),
        password: secrecy::SecretString::from("testpass".to_string()),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: Some("/".to_string()),
    }
}

async fn connected() -> FtpProvider {
    let mut p = FtpProvider::new(fixture_config());
    p.connect().await.expect("FTP connect to the fixture");
    p
}

/// The defect this PR exists for: a directory that is not there must not look
/// like one that is empty.
///
/// It is not a cosmetic difference. A sync with `--delete` reads an empty
/// source listing as "everything was removed" and plans to mirror that onto
/// the destination, so a mistyped path could authorise deleting a local tree.
#[tokio::test]
#[ignore = "live: requires the ftp Docker fixture up on :2123"]
async fn a_missing_directory_is_not_reported_as_an_empty_one() {
    let mut p = connected().await;
    let missing = "/no-such-directory-6f1a9c";

    match p.list(missing).await {
        Err(ProviderError::NotFound(_)) | Err(ProviderError::InvalidPath(_)) => {}
        Ok(entries) => panic!(
            "a missing directory answered with a successful listing of {} entries; \
             this is the shape that lets a delete pass treat it as emptied",
            entries.len()
        ),
        Err(other) => panic!("expected a not-found answer, got {other:?}"),
    }

    // The control case, and it matters as much: a directory that really is
    // empty must still list as empty. A fix that answered NotFound for both
    // would trade one wrong answer for another.
    let empty = "/aeroftp-test-empty-6f1a9c";
    let _ = p.rmdir(empty).await;
    p.mkdir(empty).await.expect("create the empty directory");
    let listed = p
        .list(empty)
        .await
        .expect("an existing empty directory lists");
    assert!(listed.is_empty(), "the directory was created empty");
    p.rmdir(empty).await.expect("clean up");

    let _ = p.disconnect().await;
}

/// A listing must leave the connection where it found it, and must say so if
/// it cannot.
///
/// The hidden-file path CWDs into the target to issue `LIST -a`. The restore
/// afterwards used to be `let _ = ...`, so a failed restore left the session
/// in another directory silently: every later relative operation addressed the
/// wrong place, and nothing connected the damage back to the listing.
#[tokio::test]
#[ignore = "live: requires the ftp Docker fixture up on :2123"]
async fn a_listing_leaves_the_working_directory_where_it_found_it() {
    let mut p = connected().await;
    let dir = "/aeroftp-test-cwd-6f1a9c";
    let _ = p.rmdir_recursive(dir).await;
    p.mkdir(dir).await.expect("create the directory");

    let before = p.pwd().await.expect("pwd before");
    // `rmdir_recursive` enumerates with hidden files included, which is the
    // path that CWDs into the target: the only one that can leave the session
    // somewhere else.
    p.rmdir_recursive(dir)
        .await
        .expect("recursive delete of an empty directory");
    let after = p.pwd().await.expect("pwd after");

    assert_eq!(
        before, after,
        "the listing moved the session and left it there"
    );
    let _ = p.disconnect().await;
}

/// `stat` must report the real size, not the zero a listing row may carry.
///
/// The CLI has been asking the server with SIZE since this was first hit, in a
/// helper applied after its own stat calls, so `stat` was right on one surface
/// and wrong on the other two. The hydration now lives in the provider; this
/// checks it from the provider, which is where all three surfaces enter.
#[tokio::test]
#[ignore = "live: requires the ftp Docker fixture up on :2123"]
async fn stat_reports_the_real_size_and_not_a_listing_zero() {
    let mut p = connected().await;
    let remote = "/aeroftp-test-size-6f1a9c.bin";
    let payload = vec![7u8; 4096];

    let _ = p.delete(remote).await;
    let local = std::env::temp_dir().join("aeroftp-test-size-6f1a9c.bin");
    std::fs::write(&local, &payload).expect("write the local file");
    p.upload(local.to_str().unwrap(), remote, None)
        .await
        .expect("upload");

    let entry = p.stat(remote).await.expect("stat the uploaded file");
    assert_eq!(
        entry.size,
        payload.len() as u64,
        "stat reported {} for a {}-byte file: a zero here is indistinguishable \
         from an empty file to every caller",
        entry.size,
        payload.len()
    );

    p.delete(remote).await.expect("clean up");
    let _ = std::fs::remove_file(&local);
    let _ = p.disconnect().await;
}

/// Creating a directory that is already there is not a failure.
///
/// Every MKD failure used to become one generic error, so the idempotent
/// branch that `mkdir -p` already had could never be reached on FTP: the
/// variant it matches on was never produced. rclone answers this case with a
/// silent success, so the target behaviour is not something we are inventing.
#[tokio::test]
#[ignore = "live: requires the ftp Docker fixture up on :2123"]
async fn creating_an_existing_directory_says_it_already_exists() {
    let mut p = connected().await;
    let dir = "/aeroftp-test-mkdir-6f1a9c";
    // A previous run that failed before its cleanup would otherwise make the
    // FIRST create the duplicate one, and the test would fail for a reason
    // that has nothing to do with what it checks.
    let _ = p.rmdir(dir).await;

    p.mkdir(dir).await.expect("first create succeeds");
    match p.mkdir(dir).await {
        Err(ProviderError::AlreadyExists(_)) => {}
        Ok(()) => { /* a server that answers 2xx twice is also fine */ }
        Err(other) => panic!(
            "a second mkdir must be recognisable as already-existing, got {other:?}; \
             a generic error here is what forced callers to confirm with a stat"
        ),
    }

    p.rmdir(dir).await.expect("clean up");
    let _ = p.disconnect().await;
}

/// A name that is taken by a FILE is not a directory that already exists.
///
/// The probe behind the previous test asks whether the path is there, and it
/// must insist that it is there as a DIRECTORY. Someone with a file called
/// `backup` who asks for a folder called `backup` gets a real failure, not a
/// silent success followed by an unrelated error on the first upload into a
/// directory that was never created. The patch this replaces made the same
/// distinction, and a fix that dropped it would be worse than what it removed.
#[tokio::test]
#[ignore = "live: requires the ftp Docker fixture up on :2123"]
async fn a_file_with_the_same_name_is_not_an_existing_directory() {
    let mut p = connected().await;
    let taken = "/aeroftp-test-taken-6f1a9c";
    let _ = p.delete(taken).await;
    let _ = p.rmdir(taken).await;

    let local = std::env::temp_dir().join("aeroftp-test-taken-6f1a9c");
    std::fs::write(&local, b"occupied").expect("write the local file");
    p.upload(local.to_str().unwrap(), taken, None)
        .await
        .expect("upload a file under the name the directory would take");

    match p.mkdir(taken).await {
        Err(ProviderError::AlreadyExists(_)) => panic!(
            "a file is occupying that name; reporting it as an existing directory \
             would let an idempotent caller carry on and fail later, somewhere else"
        ),
        Err(_) => {}
        Ok(()) => panic!("the server should not have created a directory over a file"),
    }

    p.delete(taken).await.expect("clean up");
    let _ = std::fs::remove_file(&local);
    let _ = p.disconnect().await;
}
