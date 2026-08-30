//! Live checks for the listing path taken against a server that speaks MLSD.
//!
//! `#[ignore]` so a default `cargo test` skips them, following the pattern of
//! `integration_ftp_missing_path.rs`.
//!
//! ```bash
//! cd src-tauri/tests/fixtures/ftp-mlsd
//! docker compose up -d --build            # control :2199
//! cd ../../.. && cargo test --test integration_ftp_mlsd -- --ignored --nocapture
//! cd tests/fixtures/ftp-mlsd && docker compose down -v
//! ```
//!
//! Why a second FTP fixture. `list_inner_opts` tries MLSD first and only falls
//! back to LIST, and the anti-hang MLST probe runs only when the server
//! advertises MLST. The vsftpd fixture announces neither, so every FTP test in
//! this repository has been exercising the fallback while the branch most
//! servers take went unexecuted. A defect lived on that branch through two
//! rounds of review for no reason other than that: a lab that cannot reach a
//! path decides what can be found in it.
//!
//! What is proved here cannot be proved by a unit test. The classification
//! itself is covered by a table in `providers::ftp`; what needs a server is
//! that the probe RUNS, that it runs before MLSD, and that its verdict is the
//! one the reply supports.

use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode, ProviderError};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};

fn provider() -> FtpProvider {
    FtpProvider::new(FtpConfig {
        host: "127.0.0.1".to_string(),
        port: 2199,
        username: "testuser".to_string(),
        password: "testpass".to_string().into(),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: None,
    })
}

/// A directory that is not there is reported as NOT FOUND, not as a path that
/// merely did not work.
///
/// The probe answered `InvalidPath` for every 550, including the ones whose
/// text says "No such file or directory", while the comment above it had said
/// "fail fast with a clear NotFound" since the day it was written. The comment
/// stated the intent and nothing checked it, and nothing could: without a
/// server that advertises MLST this branch does not execute.
#[tokio::test]
#[ignore]
async fn a_missing_directory_is_not_found_on_the_mlsd_path() {
    let mut p = provider();
    p.connect().await.expect("fixture not up: see the header");

    let err = p
        .list("/definitely-not-here")
        .await
        .expect_err("a directory that is not there must not list successfully");

    assert!(
        matches!(err, ProviderError::NotFound(_)),
        "the server said \"No such file or directory\" and the verdict was {err:?}"
    );
    // The reply survives into the message, and so does the path.
    let rendered = err.to_string();
    assert!(
        rendered.contains("/definitely-not-here"),
        "the path is missing from {rendered:?}"
    );
}

/// The probe does not turn a directory that IS there into a failure.
///
/// The boundary the fix must not cross: it would be easy to make every 550
/// louder and break ordinary listing, and this row is what would catch that.
#[tokio::test]
#[ignore]
async fn a_directory_that_exists_still_lists_on_the_mlsd_path() {
    let mut p = provider();
    p.connect().await.expect("fixture not up: see the header");

    p.list("/")
        .await
        .expect("the root exists and must list through MLSD");
}
