// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Wire-level chunk retries, using the real provider and replayable bodies.
use super::*;
use crate::transfer_multipart::PartBody;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    headers: String,
    body: Vec<u8>,
    at: Instant,
}

struct Server {
    url: String,
    seen: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    // Each PUT consumes one reply; an empty reply drops the connection after
    // receiving the entire body, and the final reply repeats on exhaustion.
    async fn start(replies: Vec<String>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let task = tokio::spawn(async move {
            let mut put_index = 0;
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut wire = Vec::new();
                let mut buf = [0u8; 8192];
                let head_end = loop {
                    let count = socket.read(&mut buf).await.unwrap();
                    if count == 0 {
                        break None;
                    }
                    wire.extend_from_slice(&buf[..count]);
                    if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(end + 4);
                    }
                };
                let Some(head_end) = head_end else { continue };
                let headers = String::from_utf8(wire[..head_end].to_vec()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while wire.len() < head_end + length {
                    let count = socket.read(&mut buf).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    wire.extend_from_slice(&buf[..count]);
                }
                let mut first_line = headers.lines().next().unwrap().split_whitespace();
                let method = first_line.next().unwrap().to_owned();
                let path = first_line.next().unwrap().to_owned();
                recorded.lock().unwrap().push(Request {
                    method: method.clone(),
                    path,
                    headers,
                    body: wire[head_end..].to_vec(),
                    at: Instant::now(),
                });
                let reply = if method == "PUT" {
                    let reply = replies
                        .get(put_index)
                        .or_else(|| replies.last())
                        .unwrap()
                        .clone();
                    put_index += 1;
                    reply
                } else {
                    response(if method == "DELETE" { 204 } else { 201 }, "")
                };
                if !reply.is_empty() {
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
            }
        });
        Self { url, seen, task }
    }

    fn provider(&self) -> WebDavProvider {
        let config = WebDavConfig {
            url: format!("{}/remote.php/dav/files/alice", self.url),
            username: "alice".into(),
            password: secrecy::SecretString::from("fixture-password".to_string()),
            initial_path: None,
            provider_id: Some("nextcloud".into()),
            verify_cert: true,
            anonymous: false,
        };
        let mut provider = WebDavProvider::new(config).unwrap();
        provider.connected = true;
        provider
    }

    fn requests(&self, method: &str) -> Vec<Request> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method)
            .cloned()
            .collect()
    }
}

fn response(status: u16, extra: &str) -> String {
    format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n")
}

fn payload() -> Vec<u8> {
    (0..300_000).map(|i| (i % 251) as u8).collect()
}

fn disk_body(bytes: &[u8]) -> (tempfile::NamedTempFile, PartBody) {
    use std::io::Write;
    let mut source = tempfile::NamedTempFile::new().unwrap();
    source.write_all(b"prefix-13byte").unwrap();
    let offset = source.as_file().metadata().unwrap().len();
    source.write_all(bytes).unwrap();
    source.write_all(b"suffix-not-a-part").unwrap();
    let body = PartBody::disk_slice(source.path().to_path_buf(), offset, bytes.len() as u64);
    (source, body)
}

async fn handle(provider: &mut WebDavProvider, size: u64) -> MultipartHandle {
    provider
        .begin_multipart_upload("/retry-fixture.bin", size, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn transient_chunk_refusals_replay_the_exact_disk_window() {
    for status in [408, 429, 500, 502, 503, 504] {
        let server = Server::start(vec![
            response(status, "Retry-After: 0\r\n"),
            response(204, "ETag: \"accepted\"\r\n"),
        ])
        .await;
        let mut provider = server.provider();
        let bytes = payload();
        let handle = handle(&mut provider, 600_000).await;
        let (_source, body) = disk_body(&bytes);
        let part = provider
            .upload_part_body(&handle, 2, body)
            .await
            .unwrap_or_else(|error| panic!("HTTP {status} must retry its chunk: {error:?}"));
        assert_eq!(part.part_number, 2);
        assert_eq!(part.etag, "accepted");
        let puts = server.requests("PUT");
        assert_eq!(puts.len(), 2, "HTTP {status}");
        assert_eq!(puts[0].path, puts[1].path);
        assert!(puts[0].path.ends_with("/00000000000000000002"));
        for request in puts {
            assert_eq!(request.body, bytes);
            let headers = request.headers.to_ascii_lowercase();
            assert!(headers.contains("content-length: 300000\r\n"));
            assert!(headers.contains("oc-total-length: 600000\r\n"));
        }
        assert_eq!(server.requests("MKCOL").len(), 1);
    }
}

#[tokio::test]
async fn owned_compat_part_replays_without_losing_its_bytes() {
    let server = Server::start(vec![response(503, "Retry-After: 0\r\n"), response(201, "")]).await;
    let mut provider = server.provider();
    let bytes = payload();
    let handle = handle(&mut provider, bytes.len() as u64).await;
    provider
        .upload_part(&handle, 1, bytes.clone())
        .await
        .unwrap();
    let puts = server.requests("PUT");
    assert_eq!(puts.len(), 2);
    assert!(puts.iter().all(|r| r.body == bytes));
}

#[tokio::test]
async fn permanent_chunk_refusals_are_not_replayed() {
    for status in [400, 401, 403, 404, 409, 412, 501, 507] {
        let server = Server::start(vec![response(status, "Retry-After: 0\r\n")]).await;
        let mut provider = server.provider();
        let handle = handle(&mut provider, 3).await;
        assert!(provider
            .upload_part(&handle, 1, b"abc".to_vec())
            .await
            .is_err());
        assert_eq!(server.requests("PUT").len(), 1, "HTTP {status}");
    }
}

#[tokio::test]
async fn acknowledged_parts_and_the_upload_session_are_not_restarted() {
    let server = Server::start(vec![
        response(201, ""),
        response(503, "Retry-After: 0\r\n"),
        response(201, ""),
    ])
    .await;
    let mut provider = server.provider();
    let handle = handle(&mut provider, 6).await;
    let first = provider
        .upload_part(&handle, 1, b"abc".to_vec())
        .await
        .unwrap();
    let second = provider
        .upload_part(&handle, 2, b"def".to_vec())
        .await
        .unwrap();
    provider
        .complete_multipart_upload(handle, vec![first, second])
        .await
        .unwrap();
    let puts = server.requests("PUT");
    assert_eq!(puts.len(), 3);
    assert_ne!(puts[0].path, puts[1].path);
    assert_eq!(puts[1].path, puts[2].path);
    assert_eq!(server.requests("MKCOL").len(), 1);
    assert_eq!(server.requests("MOVE").len(), 1);
    assert!(server.requests("DELETE").is_empty());
}

#[tokio::test]
async fn retry_after_is_respected_and_exhaustion_is_finite() {
    let server = Server::start(vec![
        response(429, "Retry-After: 1\r\n"),
        response(503, "Retry-After: 0\r\n"),
    ])
    .await;
    let mut provider = server.provider();
    let handle = handle(&mut provider, 3).await;
    assert!(provider
        .upload_part(&handle, 1, b"abc".to_vec())
        .await
        .is_err());
    let puts = server.requests("PUT");
    assert_eq!(puts.len(), 4, "one initial attempt and three retries");
    assert!(puts[1].at.duration_since(puts[0].at) >= Duration::from_secs(1));
    assert!(puts
        .iter()
        .all(|r| r.path == puts[0].path && r.body == b"abc"));
}

#[tokio::test]
async fn dropping_a_waiting_part_cancels_without_another_put() {
    let server = Server::start(vec![response(503, "Retry-After: 60\r\n")]).await;
    let mut provider = server.provider();
    let handle = handle(&mut provider, 3).await;
    let outcome = tokio::time::timeout(
        Duration::from_millis(200),
        provider.upload_part(&handle, 1, b"abc".to_vec()),
    )
    .await;
    assert!(
        outcome.is_err(),
        "retry must be waiting when the future is dropped"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server.requests("PUT").len(), 1);
    provider.abort_multipart_upload(handle).await.unwrap();
    assert_eq!(server.requests("DELETE").len(), 1);
    assert!(server.requests("MOVE").is_empty());
}

#[tokio::test]
async fn a_lost_chunk_response_is_safe_to_replay_at_the_same_path() {
    let server = Server::start(vec![String::new(), response(204, "")]).await;
    let mut provider = server.provider();
    let bytes = payload();
    let handle = handle(&mut provider, bytes.len() as u64).await;
    let (_source, body) = disk_body(&bytes);
    provider.upload_part_body(&handle, 1, body).await.unwrap();
    let puts = server.requests("PUT");
    assert_eq!(puts.len(), 2);
    assert_eq!(puts[0].path, puts[1].path);
    assert!(puts.iter().all(|r| r.body == bytes));
}

#[tokio::test]
async fn nonce_rotation_is_allowed_once_across_transient_retries() {
    let rotation = |nonce| {
        response(401, &format!("WWW-Authenticate: Digest realm=\"r\", nonce=\"{nonce}\", qop=\"auth\", stale=true\r\n"))
    };
    let server = Server::start(vec![
        response(503, "Retry-After: 0\r\n"),
        rotation("fresh"),
        response(503, "Retry-After: 0\r\n"),
        rotation("fresh-again"),
        response(201, ""),
    ])
    .await;
    let mut provider = server.provider();
    provider.digest_auth =
        Some(DigestState::parse(r#"Digest realm="r", nonce="old", qop="auth""#).unwrap());
    let handle = handle(&mut provider, 3).await;
    let (_source, body) = disk_body(b"abc");
    assert!(provider.upload_part_body(&handle, 1, body).await.is_err());
    let puts = server.requests("PUT");
    assert_eq!(puts.len(), 4, "a second rotation is terminal");
    assert!(puts[0].headers.contains("nonce=\"old\""));
    assert!(puts[2].headers.contains("nonce=\"fresh\""));
    assert!(puts
        .iter()
        .all(|r| r.path == puts[0].path && r.body == b"abc"));
}

fn small_multipart_caps() -> crate::transfer_dag::TransferCapabilities {
    use crate::transfer_dag::{Capability, TransferCapabilities};
    TransferCapabilities {
        multipart_upload: Capability::Supported,
        preferred_chunk_size: Some(6),
        multipart_threshold: 0,
        max_chunk_slots: Some(1),
        ..TransferCapabilities::default()
    }
}

fn assert_graph_upload(server: &Server, files: usize) {
    let puts = server.requests("PUT");
    assert_eq!(puts.len(), 3 * files + 1, "only the refused chunk repeats");
    assert_eq!(puts[0].body, b"abcdef");
    assert_eq!(puts[1].body, b"ghijkl");
    assert_eq!(puts[2].body, b"ghijkl");
    assert_eq!(puts[1].path, puts[2].path);
    assert_eq!(puts[3].body, b"mnopqr");
    assert_eq!(server.requests("MKCOL").len(), files);
    assert_eq!(server.requests("MOVE").len(), files);
    assert!(server.requests("DELETE").is_empty());
}

#[tokio::test]
async fn single_file_dag_retries_only_the_refused_chunk_and_commits_once() {
    use crate::transfer_dag::{
        NoopDagObserver, TransferCheckpointStore, TransferDagBuilder, TransferDirection,
    };
    use std::sync::atomic::AtomicU64;
    let server = Server::start(vec![
        response(201, ""),
        response(503, "Retry-After: 0\r\n"),
        response(201, ""),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.bin");
    std::fs::write(&source, b"abcdefghijklmnopqr").unwrap();
    let provider: crate::transfer_dag_single_file::SharedProvider = Arc::new(
        tokio::sync::Mutex::new(Some(Box::new(server.provider()) as Box<dyn StorageProvider>)),
    );
    let dag =
        TransferDagBuilder::shaped_file(TransferDirection::Upload, &small_multipart_caps(), 18);
    assert_eq!(dag.profile.upload_parts, 3);
    crate::transfer_dag_single_file::execute_single_file_dag(
        &dag,
        provider,
        "/graph.bin".into(),
        source.to_string_lossy().into_owned(),
        None,
        None,
        Arc::new(NoopDagObserver),
        Arc::new(AtomicU64::new(18)),
        18,
        Some(tokio_util::sync::CancellationToken::new()),
        Some(TransferCheckpointStore::new(directory.path().join("checkpoints")).unwrap()),
    )
    .await
    .unwrap();
    assert_graph_upload(&server, 1);
}

#[tokio::test]
async fn batch_dag_uses_the_same_part_retry_without_restarting_either_file() {
    use crate::provider_transfer_executor::{
        resolve_provider_transfer_runtime, ProviderUploadExecutor,
    };
    use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
    use crate::transfer_event_sink::{NoopTransferSink, TransferEventSink};
    use crate::transfer_orchestrator::TransferBatch;
    use crate::transfer_settings::TransferSettingsInput;
    use std::sync::atomic::AtomicBool;

    let server = Server::start(vec![
        response(201, ""),
        response(429, "Retry-After: 0\r\n"),
        response(201, ""),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.bin");
    std::fs::write(&source, b"abcdefghijklmnopqr").unwrap();
    let provider: crate::transfer_dag_single_file::SharedProvider = Arc::new(
        tokio::sync::Mutex::new(Some(Box::new(server.provider()) as Box<dyn StorageProvider>)),
    );
    let (settings, model, _) = resolve_provider_transfer_runtime(
        &provider,
        TransferSettingsInput {
            max_concurrent: Some(1),
            ..Default::default()
        },
    )
    .await;
    let sink = Arc::new(NoopTransferSink) as Arc<dyn TransferEventSink>;
    let executor = Arc::new(ProviderUploadExecutor::new(
        sink.clone(),
        provider,
        settings,
        None,
        tokio_util::sync::CancellationToken::new(),
        model,
        small_multipart_caps(),
    ));
    let entries = (0..2)
        .map(|index| TransferEntry {
            id: format!("chunk-retry-{index}"),
            display_name: format!("graph-{index}.bin"),
            remote_path: format!("/graph-{index}.bin"),
            local_path: source.to_string_lossy().into_owned(),
            size: 18,
            modified: None,
        })
        .collect();
    let batch = TransferBatch {
        id: "nextcloud-retry-batch".into(),
        display_name: "Fixture".into(),
        direction: TransferDirection::Upload,
        config: TransferBatchConfig {
            max_retries: 4,
            ..Default::default()
        },
        entries,
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        crate::transfer_dag_batch::execute_batch_dag(
            sink,
            batch,
            executor,
            Arc::new(AtomicBool::new(false)),
            None,
        ),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "batch timed out; requests: {:?}",
            server
                .seen
                .lock()
                .unwrap()
                .iter()
                .map(|r| (&r.method, &r.path, r.body.len()))
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(result.failed, 0, "{result:?}");
    assert_eq!(result.completed, 2);
    assert_graph_upload(&server, 2);
}

#[tokio::test]
async fn a_missing_disk_source_is_not_retried_as_a_network_failure() {
    let server = Server::start(vec![response(201, "")]).await;
    let mut provider = server.provider();
    let handle = handle(&mut provider, 3).await;
    let directory = tempfile::tempdir().unwrap();
    let body = PartBody::disk_slice(directory.path().join("absent.bin"), 0, 3);
    let started = Instant::now();
    assert!(provider.upload_part_body(&handle, 1, body).await.is_err());
    let elapsed = started.elapsed();
    // Read as a network failure, the missing file fails every attempt alike,
    // so the send retries the most times it may, each after at least the base
    // delay (3 s in all by default). Bounding by that rather than by one
    // second leaves a loaded runner seconds of room.
    let config = crate::providers::http_retry::HttpRetryConfig::default();
    let retried = Duration::from_millis(config.base_delay_ms) * config.max_retries;
    assert!(
        elapsed < retried,
        "a missing disk source was retried: {elapsed:?}, {} retries take at least {retried:?}",
        config.max_retries
    );
    assert!(server.requests("PUT").len() <= 1);
}

#[tokio::test]
async fn owned_part_survives_nonce_rotation_and_transient_refusal() {
    let rotated = response(
        401,
        "WWW-Authenticate: Digest realm=\"r\", nonce=\"fresh\", qop=\"auth\", stale=true\r\n",
    );
    let server = Server::start(vec![
        rotated,
        response(503, "Retry-After: 0\r\n"),
        response(204, ""),
    ])
    .await;
    let mut provider = server.provider();
    provider.digest_auth =
        Some(DigestState::parse(r#"Digest realm="r", nonce="old", qop="auth""#).unwrap());
    let handle = handle(&mut provider, 3).await;
    provider
        .upload_part(&handle, 1, b"abc".to_vec())
        .await
        .unwrap();
    let puts = server.requests("PUT");
    assert_eq!(puts.len(), 3);
    assert!(puts[1].headers.contains("nonce=\"fresh\""));
    assert!(puts
        .iter()
        .all(|r| r.path == puts[0].path && r.body == b"abc"));
}
