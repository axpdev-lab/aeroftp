//! Adapter/DAG doors exercised over a local HTTP server, never a live account.
use super::*;
use crate::delta_sync_rsync::{try_delta_transfer, SyncDirection};
use tokio::io::AsyncReadExt;

const SIZE: u64 = 201 * 1024 * 1024;
const GRID: u64 = 8 * 1024 * 1024;

#[derive(Clone, Default)]
struct Fault {
    head_status: Option<u16>,
    copy_status: Option<u16>,
    copy_code: Option<&'static str>,
    complete_code: Option<&'static str>,
    stale: bool,
    archived: bool,
}
#[derive(Default)]
struct Seen {
    wire: AtomicU64,
    creates: AtomicU64,
    completes: AtomicU64,
    aborts: AtomicU64,
    heads: AtomicU64,
    gets: AtomicU64,
    copy_headers: std::sync::Mutex<Vec<String>>,
    mtimes: std::sync::Mutex<Vec<String>>,
    fault: std::sync::Mutex<Fault>,
}
struct Mock {
    provider: S3Provider,
    dir: tempfile::TempDir,
    seen: Arc<Seen>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn xml_error(status: u16, code: &str) -> axum::response::Response {
    axum::response::Response::builder()
        .status(status)
        .header("retry-after", "0")
        .body(axum::body::Body::from(format!(
            "<Error><Code>{code}</Code><Message>mock failure</Message></Error>"
        )))
        .unwrap()
}

async fn mock() -> Mock {
    let dir = tempfile::tempdir().unwrap();
    let remote_dir = dir.path().to_path_buf();
    let seen = Arc::new(Seen::default());
    let state = seen.clone();
    let app = axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
        let root = remote_dir.clone(); let state = state.clone();
        async move {
            let method = req.method().clone();
            let query = req.uri().query().unwrap_or("").to_string();
            let headers = req.headers().clone();
            let fault = state.fault.lock().unwrap().clone();
            let param = |name:&str| query.split('&').find_map(|p|p.strip_prefix(&format!("{name}="))).unwrap_or("").to_owned();
            let uid = param("uploadId");
            let part = param("partNumber");
            let remote = root.join("remote.bin");
            let current_etag = format!("\"v{}\"",state.completes.load(Ordering::SeqCst));
            if method == Method::HEAD {
                state.heads.fetch_add(1,Ordering::SeqCst);
                if let Some(status) = fault.head_status { return xml_error(status,"AccessDenied"); }
                let Ok(meta) = tokio::fs::metadata(&remote).await else { return xml_error(404,"NoSuchKey"); };
                let mut response = axum::response::Response::builder().header("content-length",meta.len()).header("etag",if fault.stale {"\"theirs\""} else {&current_etag});
                if fault.archived { response = response.header("x-amz-storage-class","DEEP_ARCHIVE"); }
                return response.body(axum::body::Body::empty()).unwrap();
            }
            if method == Method::GET { state.gets.fetch_add(1,Ordering::SeqCst); return xml_error(400,"ForbiddenBaselineRead"); }
            if method == Method::POST && query.starts_with("uploads") {
                let id = state.creates.fetch_add(1,Ordering::SeqCst)+1;
                state.mtimes.lock().unwrap().push(headers.get("x-amz-meta-mtime").and_then(|h|h.to_str().ok()).unwrap_or("").to_owned());
                return axum::response::Response::new(axum::body::Body::from(format!("<InitiateMultipartUploadResult><UploadId>u{id}</UploadId></InitiateMultipartUploadResult>")));
            }
            if method == Method::DELETE { state.aborts.fetch_add(1,Ordering::SeqCst); return axum::response::Response::new(axum::body::Body::empty()); }
            let part_path = root.join(format!("{uid}-{part}.part"));
            if method == Method::PUT && headers.contains_key("x-amz-copy-source") {
                let guard = headers.get("x-amz-copy-source-if-match").and_then(|h|h.to_str().ok()).unwrap_or("").to_string();
                state.copy_headers.lock().unwrap().push(guard.clone());
                if let Some(status) = fault.copy_status { return xml_error(status,fault.copy_code.unwrap_or("InternalError")); }
                if guard != current_etag { return xml_error(412,"PreconditionFailed"); }
                let range = headers.get("x-amz-copy-source-range").unwrap().to_str().unwrap().trim_start_matches("bytes=");
                let (start,end) = range.split_once('-').unwrap();
                let start:u64 = start.parse().unwrap(); let end:u64 = end.parse().unwrap();
                let mut source = tokio::fs::File::open(remote).await.unwrap();
                source.seek(std::io::SeekFrom::Start(start)).await.unwrap();
                let mut out = tokio::fs::File::create(part_path).await.unwrap();
                tokio::io::copy(&mut source.take(end-start+1),&mut out).await.unwrap();
                return axum::response::Response::new(axum::body::Body::from("<CopyPartResult><ETag>\"copy\"</ETag></CopyPartResult>"));
            }
            if method == Method::PUT {
                let mut out = tokio::fs::File::create(part_path).await.unwrap();
                let mut stream = req.into_body().into_data_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.unwrap(); out.write_all(&chunk).await.unwrap();
                    state.wire.fetch_add(chunk.len() as u64,Ordering::SeqCst);
                }
                return axum::response::Response::builder().header("etag","\"put\"").body(axum::body::Body::empty()).unwrap();
            }
            if method == Method::POST {
                if let Some(code) = fault.complete_code { return xml_error(200,code); }
                let body = axum::body::to_bytes(req.into_body(),1024*1024).await.unwrap();
                let text = String::from_utf8(body.to_vec()).unwrap();
                let next = root.join("next.bin");
                let mut out = tokio::fs::File::create(&next).await.unwrap();
                for rest in text.split("<PartNumber>").skip(1) {
                    let number = rest.split("</PartNumber>").next().unwrap();
                    let mut source = tokio::fs::File::open(root.join(format!("{uid}-{number}.part"))).await.unwrap();
                    tokio::io::copy(&mut source,&mut out).await.unwrap();
                }
                out.flush().await.unwrap(); drop(out);
                tokio::fs::rename(next,remote).await.unwrap();
                let version = state.completes.fetch_add(1,Ordering::SeqCst)+1;
                return axum::response::Response::new(axum::body::Body::from(format!("<CompleteMultipartUploadResult><ETag>\"v{version}\"</ETag></CompleteMultipartUploadResult>")));
            }
            xml_error(400,"UnexpectedRequest")
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut provider =
        super::tests::make_provider(Some(&format!("http://{}", listener.local_addr().unwrap())));
    provider.connected = true;
    provider.disable_checksum = true;
    provider.baseline_test_path = Some(dir.path().join("baseline.db"));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Mock {
        provider,
        dir,
        seen,
        task,
    }
}

async fn file_digest(path: &std::path::Path) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut file = tokio::fs::File::open(path).await.unwrap();
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    hash.finalize().into()
}

async fn seed(mock: &mut Mock) -> PathBuf {
    let local = mock.dir.path().join("local.bin");
    std::fs::File::create(&local)
        .unwrap()
        .set_len(SIZE)
        .unwrap();
    mock.provider
        .upload(local.to_str().unwrap(), "object", None)
        .await
        .unwrap();
    mock.seen.wire.store(0, Ordering::SeqCst);
    local
}

async fn seed_zero_signature(mock: &Mock) {
    let mut hash = BaselineHasher::new(SIZE).unwrap();
    let buf = vec![0u8; 1024 * 1024];
    for _ in 0..201 {
        hash.update(&buf);
    }
    mock.provider
        .baseline_store()
        .unwrap()
        .record_completed(
            mock.provider.delta_baseline_key("object"),
            hash,
            "\"v1\"".into(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn s3_adapter_middle_append_and_successive_delta_have_independent_wire_ratios() {
    let mut mock = mock().await;
    let local = seed(&mut mock).await;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&local)
        .await
        .unwrap();
    file.seek(std::io::SeekFrom::Start(10 * GRID + 123))
        .await
        .unwrap();
    file.write_all(b"edit").await.unwrap();
    drop(file);
    for (index, expected_wire) in [GRID, 9 * 1024 * 1024, 0].into_iter().enumerate() {
        if index == 1 {
            let mut file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&local)
                .await
                .unwrap();
            for _ in 0..9 {
                file.write_all(&vec![7u8; 1024 * 1024]).await.unwrap();
            }
        }
        mock.seen.wire.store(0, Ordering::SeqCst);
        mock.seen.copy_headers.lock().unwrap().clear();
        let result =
            try_delta_transfer(&mut mock.provider, SyncDirection::Upload, &local, "object")
                .await
                .unwrap();
        assert!(result.used_delta, "{result:?}");
        let size = std::fs::metadata(&local).unwrap().len();
        let received = mock.seen.wire.load(Ordering::SeqCst);
        assert_eq!(received, expected_wire); // independent fixture oracle
        let stats = result.stats.unwrap();
        assert_eq!(stats.bytes_sent, received);
        assert_eq!(stats.bytes_received, 0);
        assert_eq!(stats.total_size, size);
        assert!(stats.speedup.is_finite());
        let copies = mock.seen.copy_headers.lock().unwrap().clone();
        assert!(!copies.is_empty());
        assert_eq!(stats.copy_blocks, copies.len() as u64);
        assert!(copies.iter().all(|h| h == &format!("\"v{}\"", index + 1)));
        assert_eq!(
            file_digest(&local).await,
            file_digest(&mock.dir.path().join("remote.bin")).await
        );
        assert!(mock
            .seen
            .mtimes
            .lock()
            .unwrap()
            .iter()
            .all(|s| !s.is_empty()));
        println!(
            "S3 adapter door {index}: received={received} local={size} wire_ratio={:.8}",
            received as f64 / size as f64
        );
    }
    assert_eq!(mock.seen.gets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn s3_adapter_http_failures_fallback_except_auth_and_never_poison_412_capability() {
    let mut mock = mock().await;
    let local = seed(&mut mock).await;
    let cases = [
        (
            Fault {
                copy_status: Some(412),
                copy_code: Some("PreconditionFailed"),
                ..Fault::default()
            },
            false,
            "etag_mismatch",
        ),
        (
            Fault {
                copy_status: Some(412),
                copy_code: Some("PreconditionFailed"),
                ..Fault::default()
            },
            false,
            "etag_mismatch",
        ),
        (
            Fault {
                copy_status: Some(500),
                copy_code: Some("InternalError"),
                ..Fault::default()
            },
            false,
            "s3_delta_failed",
        ),
        (
            Fault {
                copy_status: Some(403),
                copy_code: Some("AccessDenied"),
                ..Fault::default()
            },
            true,
            "",
        ),
        (
            Fault {
                head_status: Some(403),
                ..Fault::default()
            },
            true,
            "",
        ),
        (
            Fault {
                complete_code: Some("InternalError"),
                ..Fault::default()
            },
            false,
            "s3_delta_failed",
        ),
        (
            Fault {
                complete_code: Some("AccessDenied"),
                ..Fault::default()
            },
            true,
            "",
        ),
        (
            Fault {
                copy_status: Some(400),
                copy_code: Some("XNotImplemented"),
                ..Fault::default()
            },
            false,
            "backend_rejected_range_copy",
        ),
    ];
    for (fault, hard, reason) in cases {
        seed_zero_signature(&mock).await;
        let head_failed = fault.head_status.is_some();
        *mock.seen.fault.lock().unwrap() = fault;
        let before = mock.seen.creates.load(Ordering::SeqCst);
        let aborts = mock.seen.aborts.load(Ordering::SeqCst);
        let result =
            try_delta_transfer(&mut mock.provider, SyncDirection::Upload, &local, "object")
                .await
                .unwrap();
        assert!(!result.used_delta);
        assert_eq!(result.hard_error.is_some(), hard, "{result:?}");
        if !hard {
            assert_eq!(result.fallback_reason.as_deref(), Some(reason));
        }
        assert_eq!(
            mock.seen.creates.load(Ordering::SeqCst),
            before + u64::from(!head_failed)
        );
        assert_eq!(
            mock.seen.aborts.load(Ordering::SeqCst),
            aborts + u64::from(!head_failed)
        );
        assert_eq!(mock.seen.completes.load(Ordering::SeqCst), 1);
    }
    *mock.seen.fault.lock().unwrap() = Fault::default();
    let heads = mock.seen.heads.load(Ordering::SeqCst);
    let creates = mock.seen.creates.load(Ordering::SeqCst);
    let result = try_delta_transfer(
        &mut mock.provider,
        SyncDirection::Upload,
        &local,
        "other-file",
    )
    .await
    .unwrap();
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some("backend_rejected_range_copy")
    );
    assert_eq!(mock.seen.heads.load(Ordering::SeqCst), heads);
    assert_eq!(mock.seen.creates.load(Ordering::SeqCst), creates);
}

#[tokio::test]
async fn s3_adapter_refusal_doors_never_create_multipart() {
    let mut mock = mock().await;
    let local = seed(&mut mock).await;
    for (fault, reason) in [
        (
            Fault {
                stale: true,
                ..Fault::default()
            },
            "etag_mismatch",
        ),
        (Fault::default(), "no_baseline"),
        (
            Fault {
                archived: true,
                ..Fault::default()
            },
            "baseline_archived",
        ),
    ] {
        *mock.seen.fault.lock().unwrap() = fault;
        let result =
            try_delta_transfer(&mut mock.provider, SyncDirection::Upload, &local, "object")
                .await
                .unwrap();
        assert_eq!(result.fallback_reason.as_deref(), Some(reason));
        assert!(!result.used_delta);
        assert!(result.hard_error.is_none());
        assert_eq!(mock.seen.creates.load(Ordering::SeqCst), 1);
    }
    let heads = mock.seen.heads.load(Ordering::SeqCst);
    assert!(try_delta_transfer(
        &mut mock.provider,
        SyncDirection::Download,
        &local,
        "object"
    )
    .await
    .is_none());
    std::fs::File::create(&local).unwrap().set_len(1).unwrap();
    let result = try_delta_transfer(&mut mock.provider, SyncDirection::Upload, &local, "object")
        .await
        .unwrap();
    assert_eq!(result.fallback_reason.as_deref(), Some("file_too_small"));
    assert_eq!(mock.seen.heads.load(Ordering::SeqCst), heads);
}

#[tokio::test]
async fn s3_adapter_batch_dag_seeds_completion_etag_through_clone_pool_hooks() {
    use crate::provider_transfer_executor::{ProviderExecutorSessionModel, ProviderUploadExecutor};
    use crate::transfer_dag::{Capability, TransferCapabilities};
    use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
    let mock = mock().await;
    let local = mock.dir.path().join("dag.bin");
    std::fs::File::create(&local)
        .unwrap()
        .set_len(SIZE)
        .unwrap();
    let shared = Arc::new(tokio::sync::Mutex::new(Some(
        Box::new(mock.provider.clone()) as Box<dyn StorageProvider>,
    )));
    let sink: Arc<dyn crate::transfer_event_sink::TransferEventSink> =
        Arc::new(crate::transfer_event_sink::NoopTransferSink);
    let caps = TransferCapabilities {
        multipart_upload: Capability::Supported,
        preferred_chunk_size: Some(16 * 1024 * 1024),
        multipart_threshold: 200 * 1024 * 1024,
        max_chunk_slots: Some(4),
        ..TransferCapabilities::default()
    };
    let settings = crate::transfer_settings::ResolvedTransferSettings {
        requested_max_concurrent: 1,
        max_concurrent: 1,
        retry_count: 0,
        timeout_seconds: 300,
        download_segments: 1,
        sftp_download_preset: None,
    };
    let executor = Arc::new(ProviderUploadExecutor::new(
        sink.clone(),
        shared,
        settings,
        None,
        tokio_util::sync::CancellationToken::new(),
        ProviderExecutorSessionModel::HttpClonePool {
            provider_type: ProviderType::S3,
            max_leases: 4,
        },
        caps,
    ));
    let batch = crate::transfer_orchestrator::TransferBatch {
        id: "s3-baseline-dag".into(),
        display_name: "S3 baseline DAG door".into(),
        direction: TransferDirection::Upload,
        config: TransferBatchConfig {
            timeout_ms: 300_000,
            ..TransferBatchConfig::default()
        },
        entries: vec![TransferEntry {
            id: "object".into(),
            display_name: "object".into(),
            remote_path: "object".into(),
            local_path: local.to_string_lossy().into_owned(),
            size: SIZE,
            modified: None,
        }],
    };
    let result = crate::transfer_dag_batch::execute_batch_dag(
        sink,
        batch,
        executor,
        Arc::new(AtomicBool::new(false)),
        None,
    )
    .await;
    assert_eq!(result.completed, 1, "{result:?}");
    assert_eq!(result.failed, 0);
    assert_eq!(mock.seen.creates.load(Ordering::SeqCst), 1);
    assert_eq!(mock.seen.completes.load(Ordering::SeqCst), 1);
    assert_eq!(mock.seen.wire.load(Ordering::SeqCst), SIZE);
    let outcome = mock
        .provider
        .baseline_store()
        .unwrap()
        .match_file(
            mock.provider.delta_baseline_key("object"),
            "\"v1\"".into(),
            SIZE,
            &local,
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        super::super::s3_delta_baseline::MatchOutcome::Matches {
            grid: GRID,
            matches: vec![(0, 0, SIZE)]
        }
    );
    assert_eq!(
        file_digest(&local).await,
        file_digest(&mock.dir.path().join("remote.bin")).await
    );
}
