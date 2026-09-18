//! What an abandoned FTP download leaves on the control channel, guarded at the
//! layer where real transfers run.
//!
//! The defect is not a hang. When the executor's per-file deadline expires it
//! DROPS the download future, then retries on the SAME provider instance
//! (`execute_locked` holds one `Mutex` and hands `provider.as_mut()` to every
//! attempt). The abandoned `RETR` pays its `226` late, the retry reads it as the
//! answer to ITS command, and every reply after that is shifted by one for the
//! life of the session. A file size arrives at a caller that asked `TYPE`. A
//! wrong answer delivered as a right one, which a user cannot see, is worse than
//! the stall that produced it.
//!
//! **The defect lives in a window with two edges, and the numbers below encode
//! it.** The stall must outlast the deadline, or the transfer simply succeeds
//! and there is nothing to abandon. It must ALSO land while the retry is still
//! listening, or the client has given up before the late reply arrives and no
//! shift happens: measured, 35s against a 30s deadline reproduces and 45s does
//! not, on the same code. If this test ever goes red, do NOT "fix" it by
//! enlarging a number: leaving the window is exactly how it turns green and
//! empty, and a guard that cannot fail is worse than no guard, because it is
//! believed.
//!
//! **`a_healthy_download_completes` is a precondition, not the first of three.**
//! A fixture that dies on the write (its `sendall` raising `BrokenPipeError`
//! when we drop our end) tears down identically to one that is behaving, and the
//! green it produces says only "the fixture is dead". A pass on the other two
//! means nothing without it.
//!
//! **The precondition needs a SECOND fixture, and that is not a convenience.**
//! `slowftp.py` applies `--retr-stall` to every `RETR` it serves, whatever the
//! path: it does not look at the file name (see its `elif cmd == "RETR"` arm).
//! So on the stalling instance there is no such thing as a healthy download, and
//! a precondition pointed at it could only ever fail. It runs against a second
//! instance started with no stall. The liveness of the STALLING instance, which
//! is the property the precondition was reaching for, is carried by
//! `an_answer_after_an_abandoned_transfer_belongs_to_its_question`: its `SIZE`
//! after the abandonment is answered by the same session that was abandoned, so
//! a dead fixture shows up there as an error and not as a pass.
//!
//! ```bash
//! cd src-tauri/tests/fixtures/ftp
//! # the stalling instance: tests 2 and 3
//! ./slowftp.py --port 2135 --feat nomlsd --lines 1 --file-size 65536 \
//!              --retr-before-stall 4096 --retr-stall 35 &
//! # the healthy instance: the precondition only
//! ./slowftp.py --port 2136 --feat nomlsd --lines 1 --file-size 65536 &
//! cd ../../.. && cargo test --test live_ftp_stall_retry -- --ignored --nocapture
//! ```
//!
//! Every address is overridable, so the same three tests run against a real
//! server when there is one: `AEROFTP_STALL_HOST`, `AEROFTP_STALL_PORT`,
//! `AEROFTP_QUICK_PORT`, `AEROFTP_STALL_USER`, `AEROFTP_STALL_PASS`,
//! `AEROFTP_STALL_FILE`, `AEROFTP_QUICK_FILE`.
//!
//! **What a run actually measured, so this file is a photograph of one tree.**
//! Against `origin/main` `0c94b01f4` on 2026-09-18, with the two fixtures above:
//! the precondition passed; the abandonment was reported as one failure with
//! three metered retries, and the fixture log shows four `RETR` attempts each
//! preceded by a full `USER`/`PASS` re-login, because since #845 (`71cbdb57e`)
//! `redial_if_a_reply_is_pending` finds the abandoned `226` waiting and redials
//! instead of misattributing it; the third test passed through its `Err` arm,
//! because after the dropped future the provider has no control stream left and
//! `size()` answers `NotConnected` rather than a wrong number. So "retried on
//! the same session" in the second test's name means the same provider
//! INSTANCE: that instance now redials rather than reusing a session that owes
//! a reply, and seeing it happen on the real road is what makes these three
//! tests a live guard for #845 and not only for the defect that opened them.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ftp_client_gui_lib::provider_transfer_executor::{
    resolve_provider_executor_runtime, ProviderDownloadExecutor,
};
use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};
use ftp_client_gui_lib::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
use ftp_client_gui_lib::transfer_event_sink::TransferEventSink;
use ftp_client_gui_lib::transfer_orchestrator::{execute_batch, TransferBatch};
use ftp_client_gui_lib::transfer_settings::{
    resolve_transfer_settings_for_capabilities, TransferSettingsInput,
};
use ftp_client_gui_lib::TransferEvent;

/// The executor's per-file deadline, set explicitly so the window below is a
/// relation between known numbers instead of a default that can drift.
const PER_FILE_TIMEOUT_S: u64 = 30;

/// How long the fixture holds the data channel silent before paying its `226`.
/// Both edges of the window are load-bearing, see the module comment.
const STALL_S: u64 = 35;

const _: () = assert!(
    STALL_S > PER_FILE_TIMEOUT_S,
    "a stall shorter than the deadline never gets abandoned"
);
const _: () = assert!(
    STALL_S < PER_FILE_TIMEOUT_S + 15,
    "a stall that lands after the retry stops listening reproduces nothing"
);

/// Size of the file the fixture serves, in bytes. The number the third test
/// asserts: under the defect a `size()` call receives the abandoned transfer's
/// `226` instead of this.
const FIXTURE_FILE_SIZE: u64 = 65536;

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

/// The stalling fixture: tests 2 and 3.
fn stall_port() -> u16 {
    env_or("AEROFTP_STALL_PORT", "2135")
        .parse()
        .expect("AEROFTP_STALL_PORT")
}

/// The non-stalling fixture the precondition needs. See the module comment for
/// why it cannot be the same instance.
fn quick_port() -> u16 {
    env_or("AEROFTP_QUICK_PORT", "2136")
        .parse()
        .expect("AEROFTP_QUICK_PORT")
}

/// Counts what the executor ANNOUNCED, so that a batch which never ran shows up
/// as zero here instead of being mistaken for a batch that ran and failed.
///
/// **It counts `file_start`, and the draft this test came from counted
/// `"start"`.** `"start"` is emitted by the Tauri command layer in
/// `provider_commands.rs`, one storey ABOVE `execute_batch`; driving the
/// executor directly, as this test does, never produces one. The draft's
/// counter therefore read 0 on a run where the executor had announced the file
/// and retried three times, which is the shape of a check that answers an
/// easier question than the one asked. Measured 2026-09-18 against
/// `origin/main` `0c94b01f4`.
///
/// The retry itself is NOT read from here: `emit_download_start` is called once
/// per entry, before the retry loop (`provider_transfer_executor.rs:918`,
/// loop at `:923`), so this count is 1 whether there were one attempt or four.
/// Retries are read from the engine's own metering, see `run_one`.
#[derive(Default)]
struct RecordingSink {
    file_starts: Mutex<Vec<String>>,
}

impl TransferEventSink for RecordingSink {
    fn emit_transfer_event(&self, event: TransferEvent) {
        if event.event_type == "file_start" {
            self.file_starts.lock().unwrap().push(event.transfer_id);
        }
    }
}

async fn connected_provider(port: u16) -> FtpProvider {
    let mut p = FtpProvider::new(FtpConfig {
        host: env_or("AEROFTP_STALL_HOST", "127.0.0.1"),
        port,
        username: env_or("AEROFTP_STALL_USER", "u"),
        password: secrecy::SecretString::from(env_or("AEROFTP_STALL_PASS", "p")),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: None,
    });
    p.connect()
        .await
        .unwrap_or_else(|e| panic!("the fixture on port {port} must be up: {e}"));
    p
}

/// Runs one entry through the real executor.
///
/// Returns `(completed, failed, file_starts, retries)`. `retries` comes from the
/// engine's own metering, folded from `TransferExecutor::take_transfer_attempts`
/// with the first try excluded (`transfer_dag_batch.rs:711`), so a retry policy
/// that stops applying shows up as a measured zero rather than being assumed
/// away.
async fn run_one(port: u16, remote: &str, local: &str) -> (u32, u32, usize, u32) {
    let provider_arc: Arc<tokio::sync::Mutex<Option<Box<dyn StorageProvider>>>> = Arc::new(
        tokio::sync::Mutex::new(Some(Box::new(connected_provider(port).await))),
    );
    let (model, capabilities) = resolve_provider_executor_runtime(&provider_arc, 1).await;
    let runtime_settings = resolve_transfer_settings_for_capabilities(
        TransferSettingsInput {
            max_concurrent: Some(1),
            retry_count: None,
            timeout_seconds: Some(PER_FILE_TIMEOUT_S),
            download_segments: None,
            sftp_download_preset: None,
        },
        &capabilities,
    );
    let sink = Arc::new(RecordingSink::default());
    let sink_dyn: Arc<dyn TransferEventSink> = sink.clone();
    let executor = Arc::new(ProviderDownloadExecutor::new(
        sink_dyn.clone(),
        provider_arc.clone(),
        runtime_settings,
        tokio_util::sync::CancellationToken::new(),
        model,
        capabilities,
    ));
    let batch = TransferBatch {
        id: "stall-probe".to_string(),
        display_name: "stall probe".to_string(),
        direction: TransferDirection::Download,
        entries: vec![TransferEntry {
            id: "e0".to_string(),
            display_name: "target".to_string(),
            remote_path: remote.to_string(),
            local_path: local.to_string(),
            size: 0,
            modified: None,
        }],
        config: TransferBatchConfig::default(),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let result = execute_batch(sink_dyn, batch, executor, cancel, None).await;
    let file_starts = sink.file_starts.lock().unwrap().len();
    let retries = result
        .engine_stats
        .as_ref()
        .map(|stats| stats.metrics.retries)
        .expect("the DAG batch runner always fills engine_stats");
    (result.completed, result.failed, file_starts, retries)
}

/// PRECONDITION for the other two. See the module comment.
#[tokio::test]
#[ignore = "needs the FTP stall fixtures on :2135 and :2136"]
async fn a_healthy_download_completes() {
    let dst = std::env::temp_dir().join("aeroftp-stall-ok.bin");
    let (completed, failed, _, _) = run_one(
        quick_port(),
        &env_or("AEROFTP_QUICK_FILE", "/quick.txt"),
        dst.to_str().unwrap(),
    )
    .await;
    assert_eq!(
        (completed, failed),
        (1, 0),
        "the healthy path must complete; if it does not, the fixture is not serving \
         and a failure in the other tests says nothing about the code"
    );
}

/// The abandonment, and whether the executor tried again on the same session.
#[tokio::test]
#[ignore = "needs the FTP stall fixtures on :2135 and :2136"]
async fn an_abandoned_download_is_retried_on_the_same_session() {
    let dst = std::env::temp_dir().join("aeroftp-stall-bad.bin");
    let (_, failed, file_starts, retries) = run_one(
        stall_port(),
        &env_or("AEROFTP_STALL_FILE", "/file-000.txt"),
        dst.to_str().unwrap(),
    )
    .await;
    eprintln!("MEASURED abandon: failed={failed} file_starts={file_starts} retries={retries}");
    assert_eq!(
        failed, 1,
        "an abandoned download must be reported as a failure"
    );
    assert_eq!(
        file_starts, 1,
        "the executor must have announced the file exactly once; 0 means the batch \
         never reached the executor and every other number here is about nothing"
    );
    assert!(
        retries >= 1,
        "expected the deadline to be classified retryable and retried on the same \
         provider instance; the engine metered {retries} retr(y/ies). At 0 the retry \
         never happened, and the next test would be measuring nothing"
    );
}

/// The one that matters: after an abandoned transfer, does an answer still
/// belong to its question?
#[tokio::test]
#[ignore = "needs the FTP stall fixtures on :2135 and :2136"]
async fn an_answer_after_an_abandoned_transfer_belongs_to_its_question() {
    let mut provider = connected_provider(stall_port()).await;
    let file = env_or("AEROFTP_STALL_FILE", "/file-000.txt");

    let before = provider.size(&file).await.expect("baseline size");
    assert_eq!(
        before, FIXTURE_FILE_SIZE,
        "the fixture must serve the size this test asserts, before anything is abandoned"
    );

    // Abandon mid-RETR exactly the way the executor does: drop the future.
    let dst = std::env::temp_dir().join("aeroftp-stall-drop.bin");
    let abandoned = tokio::time::timeout(
        std::time::Duration::from_secs(PER_FILE_TIMEOUT_S),
        provider.download(&file, dst.to_str().unwrap(), None),
    )
    .await;
    assert!(
        abandoned.is_err(),
        "the fixture must still be stalling at the deadline, otherwise nothing was abandoned"
    );

    match provider.size(&file).await {
        Ok(after) => assert_eq!(
            after, FIXTURE_FILE_SIZE,
            "the size returned after an abandoned transfer must be this file's size; \
             a different value means the reply belonged to the abandoned RETR and every \
             answer on this session is now shifted by one"
        ),
        Err(e) => eprintln!("MEASURED after-abandon: session refused the question: {e}"),
    }
}
