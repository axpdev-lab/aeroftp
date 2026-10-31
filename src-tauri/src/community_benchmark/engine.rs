// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The run itself: scratch tree, single-file size sweep, latency probe,
//! many-small-files workload, cleanup, report.
//!
//! The engine prints nothing. What a surface shows while it runs (the CLI's
//! phase lines and progress bars, AeroAgent's tool progress) comes from a
//! [`BenchmarkObserver`], so the measurement is the same code on every
//! surface and only the narration differs.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

use super::{
    benchmark_effective_prefix, benchmark_endpoint_kind, benchmark_mkdir_ladder,
    benchmark_provider_hint, benchmark_public_ip, benchmark_remote_roots,
    benchmark_remote_roots_from_prefix, benchmark_rounded_hour_utc, benchmark_signal_from,
    benchmark_stats_from, benchmark_tod_bucket, note_trash_purge, trash_kept_note,
    BenchmarkCliMeta, BenchmarkClient, BenchmarkConfig, BenchmarkConsent, BenchmarkEnvironment,
    BenchmarkErrors, BenchmarkLevel, BenchmarkRawRun, BenchmarkReport, BenchmarkResult,
    BenchmarkSummary, ManyFilesConfig, BENCHMARK_IP_CHANGED_WARNING,
};
use crate::providers::{ProviderError, ProviderType, StorageProvider};
use crate::transfer_dag::TransferDirection;
use crate::transfer_dag_single_file::{
    pick_single_file_route, run_single_file_on_route, MultipartFailure, ProgressCallback,
};
use crate::util::size::format_size;

/// Timed round trips of the latency probe, after one untimed request that
/// may open a connection or warm a cache.
pub const BENCHMARK_LATENCY_SAMPLES: u32 = 5;

/// Longest one cleanup step may take. Cleanup runs after the profile timeout
/// too, so it has its own bound: an unreachable server must not hold the run
/// open forever after it has been stopped.
const CLEANUP_STEP_TIMEOUT: Duration = Duration::from_secs(120);

/// What happened during a run, for the surface to narrate.
#[derive(Debug, Clone, Copy)]
pub enum BenchmarkEvent<'a> {
    /// The shared `aeroftp-bench` base could not be created. Not fatal: the
    /// per-run folder is created with its parents right after.
    BaseDirNotCreated { path: &'a str, error: &'a str },
    /// The size sweep moved to a new payload size.
    Payload { size: u64 },
    /// One timed (or warmup) upload or download is about to start.
    Run {
        operation: &'static str,
        size: u64,
        run: u32,
        total: u32,
        warmup: bool,
    },
    /// The latency probe is about to send its round trips.
    LatencyProbe { samples: u32 },
    /// The many-small-files workload is starting.
    ManyFiles { file_count: u32, file_size: u64 },
    /// A many-small-files batch (`upload-all`, `download-all`) is starting.
    BatchStarted { operation: &'static str, total: u64 },
    /// One file of the current batch was transferred.
    BatchItem { operation: &'static str },
    /// The current batch ended.
    BatchFinished { operation: &'static str },
    /// A soft-deleted scratch folder was purged from the provider's trash.
    TrashPurged { path: &'a str },
    /// The run waited for the server to make an upload readable before a
    /// timed download. The wait is not counted as download time.
    WaitedForReadable { waited: std::time::Duration },
}

/// The narration of a run. Every method has a silent default.
pub trait BenchmarkObserver: Send + Sync {
    fn event(&self, _event: BenchmarkEvent<'_>) {}
    /// A byte-progress callback for the timed transfer about to start, if the
    /// observer wants one. Warmup runs and the direct-transfer mode ask too;
    /// the observer decides.
    fn transfer_progress(
        &self,
        _operation: &'static str,
        _size: u64,
        _warmup: bool,
    ) -> Option<ProgressCallback> {
        None
    }
    /// The transfer [`Self::transfer_progress`] was last asked about ended.
    fn transfer_finished(&self) {}
}

/// Everything a run needs besides the connected provider.
pub struct BenchmarkOptions {
    pub level: BenchmarkLevel,
    pub config: BenchmarkConfig,
    pub many_files: Option<ManyFilesConfig>,
    pub consent_publish: bool,
    pub anonymize_extra: bool,
    pub profile_timeout_secs: u64,
    /// A writable sub-path chosen by the user, honored verbatim. The caller
    /// has already refused `..` and null bytes in it.
    pub test_root_prefix: Option<String>,
    /// Delete the previous payload between runs of one size, for providers
    /// that refuse overwrite-on-PUT.
    pub pre_delete: bool,
    /// Call the provider's own `upload` / `download` instead of the routed
    /// transfer (the CLI `--partial` mode).
    pub direct_transfers: bool,
    pub engine_override: crate::transfer_router::Override,
    /// Probe the public IP at the start and the end, and flag a change. Off
    /// when a multi-profile sweep owns one probe across all its profiles.
    pub check_public_ip: bool,
    /// The service identity of the profile, see
    /// [`super::benchmark_service_label`].
    pub service: String,
    /// How long the caller took to open the session (DNS, TCP, TLS and the
    /// login), reported as the `connect` row. `None` when not measured.
    pub connect_ms: Option<f64>,
    /// The profile's host is this machine (see
    /// [`super::benchmark_is_loopback_host`]): its round trip measures a
    /// local bridge, not the service behind it.
    pub local_endpoint: bool,
    /// Raised by the surface to stop the run. The run ends after the current
    /// step, cleans its scratch tree up, and returns what it measured.
    pub cancel: Option<CancellationToken>,
}

/// A finished run.
pub struct BenchmarkOutcome {
    pub report: BenchmarkReport,
    /// The sweep stopped before its end (a fatal transfer, a timeout, a
    /// local temp-file failure, or a cancel).
    pub early_abort: bool,
    /// The run was stopped through [`BenchmarkOptions::cancel`].
    pub cancelled: bool,
    /// The run reached [`BenchmarkOptions::profile_timeout_secs`] and stopped
    /// there, keeping what it had measured.
    pub timed_out: bool,
    /// A measured operation failed: an upload, a timed download, a file of
    /// the many-small-files workload. A failure the report shows as a row
    /// with no runs, not only as a line among the warnings.
    pub failed: bool,
}

const CANCELLED_NOTE: &str =
    "benchmark cancelled before the end; the results cover the runs completed until then";

/// `f`, unless `stop` is raised first (or already is). `None` means stopped:
/// the operation was abandoned where it stood.
async fn until_stopped<F: Future>(stop: &CancellationToken, f: F) -> Option<F::Output> {
    tokio::select! {
        biased;
        _ = stop.cancelled() => None,
        out = f => Some(out),
    }
}

/// `f` with the cleanup bound, see [`CLEANUP_STEP_TIMEOUT`].
async fn bounded<F: Future>(f: F) -> Option<F::Output> {
    tokio::time::timeout(CLEANUP_STEP_TIMEOUT, f).await.ok()
}

/// A result row from metadata round trips (connect, latency, list, stat,
/// delete): the latency distribution of `samples_ms`, no payload.
fn probe_result(
    protocol: &str,
    anonymize_extra: bool,
    report_id: &str,
    operation: &str,
    samples_ms: &[f64],
) -> BenchmarkResult {
    let (h, hh) = benchmark_provider_hint(protocol, anonymize_extra, report_id);
    BenchmarkResult {
        protocol: protocol.to_string(),
        provider_hint: h,
        provider_hash: hh,
        operation: operation.into(),
        payload_size_bytes: 0,
        runs: samples_ms.len() as u32,
        warmup_runs_discarded: 0,
        file_count: None,
        files_per_second: None,
        throughput_mbps: None,
        latency_ms: benchmark_stats_from(samples_ms),
        tls_handshake_ms: None,
        errors: BenchmarkErrors {
            transient: 0,
            fatal: 0,
        },
        raw_runs: samples_ms
            .iter()
            .map(|ms| BenchmarkRawRun {
                duration_ms: *ms as u64,
                bytes: 0,
                throughput_mbps: None,
            })
            .collect(),
    }
}

/// The row of an operation that failed before any run of it succeeded:
/// zero runs and the failures counted, so the report says "failed" rather
/// than dropping the operation or showing it as a 0 ms success.
fn failed_result(
    protocol: &str,
    anonymize_extra: bool,
    report_id: &str,
    operation: &str,
    payload_size_bytes: u64,
    warmup_runs_discarded: u32,
    errors: BenchmarkErrors,
) -> BenchmarkResult {
    let (h, hh) = benchmark_provider_hint(protocol, anonymize_extra, report_id);
    BenchmarkResult {
        protocol: protocol.to_string(),
        provider_hint: h,
        provider_hash: hh,
        operation: operation.into(),
        payload_size_bytes,
        runs: 0,
        warmup_runs_discarded,
        file_count: None,
        files_per_second: None,
        throughput_mbps: None,
        latency_ms: benchmark_stats_from(&[]),
        tls_handshake_ms: None,
        errors,
        raw_runs: Vec::new(),
    }
}

/// Wait, before a timed download, until the file this run just uploaded can
/// be read back. A server that writes uploads back after a delay (Filen
/// Desktop's `rclone serve s3`) would otherwise put that delay into the
/// download time: 10 MB measured at 5 Mbps that was 15 s of waiting and well
/// under a second of transfer (#368). An error is left to the download, which
/// reports it.
async fn wait_until_upload_readable(
    provider: &mut Box<dyn StorageProvider>,
    remote_path: &str,
    stop: &CancellationToken,
    observer: &dyn BenchmarkObserver,
) {
    if let Some(Ok(Some(waited))) =
        until_stopped(stop, provider.wait_until_readable(remote_path)).await
    {
        observer.event(BenchmarkEvent::WaitedForReadable { waited });
    }
}

/// One timed transfer, through the router unless the run asked for direct
/// provider calls. Both paths end at `stop`.
#[allow(clippy::too_many_arguments)]
async fn timed_transfer(
    provider: Box<dyn StorageProvider>,
    direction: TransferDirection,
    operation: &'static str,
    remote: &str,
    local: &str,
    size: u64,
    warmup: bool,
    opts: &BenchmarkOptions,
    stop: &CancellationToken,
    observer: &dyn BenchmarkObserver,
) -> (Box<dyn StorageProvider>, Result<(), ProviderError>) {
    if opts.direct_transfers {
        let mut provider = provider;
        let transfer = async {
            match direction {
                TransferDirection::Upload => provider.upload(local, remote, None).await,
                TransferDirection::Download => provider.download(remote, local, None).await,
            }
        };
        let result = until_stopped(stop, transfer)
            .await
            .unwrap_or_else(|| Err(ProviderError::Other("benchmark stopped".into())));
        return (provider, result);
    }
    let decision = pick_single_file_route(
        provider.as_ref(),
        direction,
        local,
        opts.engine_override,
        None,
    );
    let progress = observer.transfer_progress(operation, size, warmup);
    let (provider, result) = run_single_file_on_route(
        provider,
        &decision,
        direction,
        remote,
        local,
        progress,
        Some(stop.clone()),
        // The payload is a temp file: a failed multipart upload will never be
        // resumed, so its provider session is aborted rather than left open.
        MultipartFailure::Abort,
    )
    .await;
    observer.transfer_finished();
    (provider, result)
}

/// Throughput in megabits per second of `size` bytes moved in `ms`.
fn mbps(size: u64, ms: f64) -> f64 {
    (size as f64 * 8.0) / 1_000_000.0 / (ms / 1000.0).max(1e-6)
}

/// Run the benchmark on `provider`, already connected, whose start path is
/// `initial_path`. The provider is disconnected before this returns.
///
/// `Err` means nothing was measured: the scratch folder could not be created.
/// Everything that goes wrong after that point is a line in the report's
/// `summary.errors`, and the report is returned.
pub async fn run(
    mut provider: Box<dyn StorageProvider>,
    initial_path: &str,
    opts: BenchmarkOptions,
    observer: &dyn BenchmarkObserver,
) -> Result<BenchmarkOutcome, String> {
    let cfg = &opts.config;
    let anonymize_extra = opts.anonymize_extra;
    let profile_timeout_secs = opts.profile_timeout_secs;

    // One stop signal for the whole run: the surface's cancel, or the
    // profile timeout. Every provider call below ends at it, so a server that
    // stops answering cannot hold the run past its timeout (#368: a Yandex
    // request that never returned held a whole comparison for ten minutes).
    let stop = opts
        .cancel
        .as_ref()
        .map(CancellationToken::child_token)
        .unwrap_or_default();
    let timed_out = Arc::new(AtomicBool::new(false));
    // The surface's cancel reaches `stop` at once (a child token); the
    // timeout reaches it from this task.
    let stopper = {
        let stop = stop.clone();
        let timed_out = timed_out.clone();
        let limit = Duration::from_secs(profile_timeout_secs);
        tokio::spawn(async move {
            tokio::select! {
                _ = stop.cancelled() => {}
                _ = tokio::time::sleep(limit) => {
                    timed_out.store(true, Ordering::SeqCst);
                    stop.cancel();
                }
            }
        })
    };

    // IP fairness check (issue #368 #1): snapshot the public IP at the start
    // so we can warn if it changes mid-run (a VPN switch makes the numbers
    // incomparable).
    let start_public_ip = if opts.check_public_ip {
        benchmark_public_ip().await
    } else {
        None
    };

    let protocol = provider.provider_type().to_string();
    // The client the factory actually built, behind any crypt overlay: the
    // provider type alone does not tell MEGAcmd from AeroFTP's own MEGA
    // client, or Proton's official CLI from AeroFTP's own Proton client.
    let client = {
        let concrete =
            crate::crypt_overlay_provider::concrete_provider_mut(&mut *provider).as_any_mut();
        if concrete.is::<crate::providers::mega::MegaCmdProvider>() {
            BenchmarkClient::MegaCmd
        } else if concrete.is::<crate::providers::proton::ProtonCliProvider>() {
            BenchmarkClient::ProtonCli
        } else if proton_native(concrete) {
            BenchmarkClient::ProtonNative
        } else if provider.provider_type() == crate::providers::ProviderType::Mega {
            BenchmarkClient::MegaNative
        } else {
            BenchmarkClient::Other
        }
    };
    // Access-method label for the "Protocol" column (issue #277): computed from
    // the live client so it reflects the transport the factory actually built
    // (a Koofr-over-WebDAV profile reports `WebDAV`, the native one `REST API`).
    let provider_access_label = provider.provider_type().access_label();
    let (access, _, _) = benchmark_endpoint_kind(client, provider_access_label, false, false);

    let report_id = uuid::Uuid::new_v4().to_string();
    // A relative prefix on Proton Drive lands where the session starts (see
    // `benchmark_effective_prefix`); without a prefix the roots are under the
    // start path. The folder the session stands in is asked under the stop
    // signal too: a `pwd` that never answers ends at the cancel or the
    // timeout, and the scratch folder below reports the stop.
    let prefix = match opts.test_root_prefix.as_deref() {
        Some(prefix) => {
            let kind = provider.provider_type();
            let start = if kind == ProviderType::Proton {
                until_stopped(&stop, provider.pwd())
                    .await
                    .and_then(Result::ok)
                    .unwrap_or_else(|| "/".to_string())
            } else {
                String::new()
            };
            Some(benchmark_effective_prefix(prefix, kind, &start))
        }
        None => None,
    };
    let (bench_base, test_root) = match prefix.as_deref() {
        Some(prefix) => benchmark_remote_roots_from_prefix(prefix, &report_id),
        // A relative start path is the folder the session entered: joined to
        // it again, `team` put the scratch folder in `team/team`.
        None => {
            let entered = until_stopped(
                &stop,
                crate::util::start_folder::as_entered(initial_path, &mut *provider),
            )
            .await
            .unwrap_or_else(|| initial_path.to_string());
            benchmark_remote_roots(&entered, &report_id)
        }
    };
    // Create the scratch directory tree before any upload. `bench_base` may
    // already exist from a prior run (`AlreadyExists` is fine), but `test_root`
    // is unique per run. In the no-prefix case `bench_base` is always our own
    // `aeroftp-bench` folder and is safe to remove when empty (issue #368: the
    // reporter had to delete it by hand on every drive afterwards).
    //
    // With `--test-root-prefix` the folders of the prefix are the user's
    // unless this run made them, so each one is checked first: a folder that
    // was absent and that this run created is removed at the end once empty;
    // one that existed is never touched.
    let mut created_by_run: Vec<String> = Vec::new();
    if opts.test_root_prefix.is_some() {
        let mut last_error: Option<ProviderError> = None;
        for dir in benchmark_mkdir_ladder(&bench_base) {
            let existed = until_stopped(&stop, provider.exists(&dir)).await;
            match until_stopped(&stop, provider.mkdir(&dir)).await {
                None => break,
                Some(Ok(())) => {
                    last_error = None;
                    if matches!(existed, Some(Ok(false))) {
                        created_by_run.push(dir);
                    }
                }
                Some(Err(ProviderError::AlreadyExists(_))) => last_error = None,
                Some(Err(e)) => last_error = Some(e),
            }
        }
        if let Some(e) = last_error {
            observer.event(BenchmarkEvent::BaseDirNotCreated {
                path: &bench_base,
                error: &e.to_string(),
            });
        }
    } else {
        match until_stopped(&stop, provider.mkdir(&bench_base)).await {
            None | Some(Ok(())) | Some(Err(ProviderError::AlreadyExists(_))) => {}
            Some(Err(e)) => observer.event(BenchmarkEvent::BaseDirNotCreated {
                path: &bench_base,
                error: &e.to_string(),
            }),
        }
    }
    // Create `test_root` with parents (mkdir -p). Some WebDAV servers (pCloud,
    // issue #368) refuse a folder whose parent collection does not yet exist
    // and do not auto-create intermediates, so a single mkdir of the nested
    // scratch path failed with "Parent directory does not exist" even though
    // the base creation above had also been rejected. Creating each component
    // in turn makes the scratch tree robust across all 22 backends.
    let scratch_created = match until_stopped(&stop, mkdir_p(&mut provider, &test_root)).await {
        Some(Ok(())) | Some(Err(ProviderError::AlreadyExists(_))) => true,
        // Stopped before the run began: nothing to measure, nothing made.
        None if !timed_out.load(Ordering::SeqCst) => false,
        None => {
            stopper.abort();
            let _ = bounded(provider.disconnect()).await;
            return Err(format!(
                "benchmark timed out after {}s creating its scratch folder; the server did not answer",
                profile_timeout_secs
            ));
        }
        Some(Err(e)) => {
            stopper.abort();
            // The prefix folders this run made before the refusal go too,
            // deepest first and only when empty (#368, FileLu left one).
            let (mut left, mut kept) = (Vec::new(), Vec::new());
            for dir in created_by_run.iter().rev() {
                if !remove_if_empty(&mut provider, dir, &mut left, &mut kept, observer).await {
                    break;
                }
            }
            let _ = bounded(provider.disconnect()).await;
            return Err(scratch_dir_error(&test_root, initial_path, &e));
        }
    };

    let total_start = Instant::now();
    let mut results: Vec<BenchmarkResult> = Vec::new();
    let mut total_bytes_transferred: u64 = 0;
    let mut total_runs: u32 = 0;
    let mut errors: Vec<String> = Vec::new();
    // What cleanup leaves by design (a trash this login cannot empty): told
    // as a note, not as an error.
    let mut trash_notes: Vec<String> = Vec::new();
    let mut early_abort = false;
    let mut stopped = stop.is_cancelled();
    let mut fatal_failures: u32 = 0;
    let mut latency_done = false;

    // The time it took to open the session, measured by the caller.
    if let Some(ms) = opts.connect_ms {
        results.push(probe_result(
            &protocol,
            anonymize_extra,
            &report_id,
            "connect",
            &[ms],
        ));
    }

    'outer: for &size in &cfg.sizes_bytes {
        if stop.is_cancelled() {
            stopped = true;
            early_abort = true;
            break;
        }

        let local_payload = match NamedTempFile::new() {
            Ok(f) => f,
            Err(e) => {
                errors.push(format!("cannot create local temp: {}", e));
                early_abort = true;
                break;
            }
        };
        if let Err(e) = crate::speed_payload::write_random(local_payload.path(), size) {
            errors.push(e);
            early_abort = true;
            break;
        }

        let local_download = match NamedTempFile::new() {
            Ok(f) => f,
            Err(e) => {
                errors.push(format!("cannot create download temp: {}", e));
                early_abort = true;
                break;
            }
        };

        let remote_path = crate::speed_payload::name(&format!("{}/payload-{}", test_root, size));

        observer.event(BenchmarkEvent::Payload { size });

        let mut upload_durations_ms: Vec<f64> = Vec::new();
        let mut download_durations_ms: Vec<f64> = Vec::new();
        let mut upload_throughput_mbps: Vec<f64> = Vec::new();
        let mut download_throughput_mbps: Vec<f64> = Vec::new();
        let upload_transient = 0u32;
        let mut upload_fatal = 0u32;
        let mut download_transient = 0u32;
        let mut download_fatal = 0u32;
        let mut upload_raw: Vec<BenchmarkRawRun> = Vec::new();
        let mut download_raw: Vec<BenchmarkRawRun> = Vec::new();
        let mut upload_failed = false;

        let total_iters = cfg.warmup_runs + cfg.runs_per_size;
        let needs_upload = cfg.operations.contains(&"upload");
        let needs_download = cfg.operations.contains(&"download");

        'runs: for iter in 0..total_iters {
            let is_warmup = iter < cfg.warmup_runs;

            if needs_upload {
                // Progress indication (issue #368 #2): show the current phase
                // and run so a long per-profile benchmark is not a blind wait.
                observer.event(BenchmarkEvent::Run {
                    operation: "upload",
                    size,
                    run: iter + 1,
                    total: total_iters,
                    warmup: is_warmup,
                });
                if stop.is_cancelled() {
                    stopped = true;
                    early_abort = true;
                    break 'runs;
                }
                // Strict providers (4shared, several WebDAV servers) reject
                // overwrite-on-PUT: between successive runs of the same size
                // we delete the previous payload best-effort. Errors are
                // ignored on the first iteration (file does not exist yet)
                // and on transient deletes (the upload itself will reveal
                // any real failure).
                if opts.pre_delete && iter > 0 {
                    let _ = until_stopped(&stop, provider.delete(&remote_path)).await;
                }

                let start = Instant::now();
                let local_payload_path = local_payload.path().to_string_lossy().to_string();
                let (returned, upload_result) = timed_transfer(
                    provider,
                    TransferDirection::Upload,
                    "upload",
                    &remote_path,
                    &local_payload_path,
                    size,
                    is_warmup,
                    &opts,
                    &stop,
                    observer,
                )
                .await;
                provider = returned;
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                match upload_result {
                    Ok(()) => {
                        if !is_warmup {
                            upload_durations_ms.push(elapsed_ms);
                            let rate = mbps(size, elapsed_ms);
                            upload_throughput_mbps.push(rate);
                            upload_raw.push(BenchmarkRawRun {
                                duration_ms: elapsed_ms as u64,
                                bytes: size,
                                throughput_mbps: Some(rate),
                            });
                            total_bytes_transferred += size;
                            total_runs += 1;
                        }
                    }
                    // A transfer the stop cut short is not a failure of the
                    // provider: it is not counted, and the runs of this size
                    // measured before it are still reported.
                    Err(_) if stop.is_cancelled() => {
                        stopped = true;
                        early_abort = true;
                        break 'runs;
                    }
                    Err(e) => {
                        upload_fatal += 1;
                        errors.push(format!("upload of {} failed: {}", format_size(size), e));
                        upload_failed = true;
                        early_abort = true;
                        break 'runs;
                    }
                }
            }

            if needs_download {
                // Progress indication (issue #368 #2): mirror the upload phase.
                observer.event(BenchmarkEvent::Run {
                    operation: "download",
                    size,
                    run: iter + 1,
                    total: total_iters,
                    warmup: is_warmup,
                });
                if stop.is_cancelled() {
                    stopped = true;
                    early_abort = true;
                    break 'runs;
                }
                wait_until_upload_readable(&mut provider, &remote_path, &stop, observer).await;
                let start = Instant::now();
                let local_download_path = local_download.path().to_string_lossy().to_string();
                let (returned, dl_result) = timed_transfer(
                    provider,
                    TransferDirection::Download,
                    "download",
                    &remote_path,
                    &local_download_path,
                    size,
                    is_warmup,
                    &opts,
                    &stop,
                    observer,
                )
                .await;
                provider = returned;
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                match dl_result {
                    Ok(()) => {
                        if !is_warmup {
                            download_durations_ms.push(elapsed_ms);
                            let rate = mbps(size, elapsed_ms);
                            download_throughput_mbps.push(rate);
                            download_raw.push(BenchmarkRawRun {
                                duration_ms: elapsed_ms as u64,
                                bytes: size,
                                throughput_mbps: Some(rate),
                            });
                            total_bytes_transferred += size;
                            total_runs += 1;
                        }
                    }
                    Err(_) if stop.is_cancelled() => {
                        stopped = true;
                        early_abort = true;
                        break 'runs;
                    }
                    Err(e) => {
                        if !is_warmup {
                            download_fatal += 1;
                        } else {
                            download_transient += 1;
                        }
                        errors.push(format!("download of {} failed: {}", format_size(size), e));
                    }
                }
            }
        }

        let (provider_hint_str, provider_hash_str) =
            benchmark_provider_hint(&protocol, anonymize_extra, &report_id);

        if needs_upload && !upload_durations_ms.is_empty() {
            results.push(BenchmarkResult {
                protocol: protocol.clone(),
                provider_hint: provider_hint_str.clone(),
                provider_hash: provider_hash_str.clone(),
                operation: "upload".into(),
                payload_size_bytes: size,
                runs: upload_durations_ms.len() as u32,
                warmup_runs_discarded: cfg.warmup_runs,
                file_count: None,
                files_per_second: None,
                throughput_mbps: Some(benchmark_stats_from(&upload_throughput_mbps)),
                latency_ms: benchmark_stats_from(&upload_durations_ms),
                tls_handshake_ms: None,
                errors: BenchmarkErrors {
                    transient: upload_transient,
                    fatal: upload_fatal,
                },
                raw_runs: upload_raw,
            });
        } else if upload_fatal > 0 {
            results.push(failed_result(
                &protocol,
                anonymize_extra,
                &report_id,
                "upload",
                size,
                cfg.warmup_runs,
                BenchmarkErrors {
                    transient: upload_transient,
                    fatal: upload_fatal,
                },
            ));
        }
        if needs_download && !download_durations_ms.is_empty() {
            results.push(BenchmarkResult {
                protocol: protocol.clone(),
                provider_hint: provider_hint_str.clone(),
                provider_hash: provider_hash_str.clone(),
                operation: "download".into(),
                payload_size_bytes: size,
                runs: download_durations_ms.len() as u32,
                warmup_runs_discarded: cfg.warmup_runs,
                file_count: None,
                files_per_second: None,
                throughput_mbps: Some(benchmark_stats_from(&download_throughput_mbps)),
                latency_ms: benchmark_stats_from(&download_durations_ms),
                tls_handshake_ms: None,
                errors: BenchmarkErrors {
                    transient: download_transient,
                    fatal: download_fatal,
                },
                raw_runs: download_raw,
            });
        } else if download_fatal > 0 {
            results.push(failed_result(
                &protocol,
                anonymize_extra,
                &report_id,
                "download",
                size,
                cfg.warmup_runs,
                BenchmarkErrors {
                    transient: download_transient,
                    fatal: download_fatal,
                },
            ));
        }
        fatal_failures += upload_fatal + download_fatal;

        // A stop keeps the runs of this size measured above and ends here:
        // the remaining probes would be measured after the user said stop.
        // A failed upload ends the sweep too: there is no payload to probe.
        if stopped || upload_failed {
            break 'outer;
        }

        // Latency probe, once per run, on the payload this size uploaded: a
        // metadata round trip is what a sync of many small changes waits on,
        // and what the signal bars show.
        if !latency_done && cfg.operations.contains(&"latency") && !upload_durations_ms.is_empty() {
            latency_done = true;
            observer.event(BenchmarkEvent::LatencyProbe {
                samples: BENCHMARK_LATENCY_SAMPLES,
            });
            // One untimed request first: it may open a connection or warm a
            // cache, which a sync pays once and not on every request.
            let _ = until_stopped(&stop, provider.stat(&remote_path)).await;
            let mut round_trips: Vec<f64> = Vec::new();
            for _ in 0..BENCHMARK_LATENCY_SAMPLES {
                let start = Instant::now();
                match until_stopped(&stop, provider.stat(&remote_path)).await {
                    None => {
                        stopped = true;
                        early_abort = true;
                        break;
                    }
                    Some(Ok(_)) => round_trips.push(start.elapsed().as_secs_f64() * 1000.0),
                    Some(Err(e)) => {
                        errors.push(format!("latency probe failed: {}", e));
                        break;
                    }
                }
            }
            if !round_trips.is_empty() {
                total_runs += round_trips.len() as u32;
                results.push(probe_result(
                    &protocol,
                    anonymize_extra,
                    &report_id,
                    "latency",
                    &round_trips,
                ));
            }
            if stopped {
                break 'outer;
            }
        }

        // list / stat / delete are measured once per size since they do not
        // benefit from multiple runs in the same way as throughput tests.
        for operation in ["list", "stat", "delete"] {
            if !cfg.operations.contains(&operation) {
                continue;
            }
            let start = Instant::now();
            let outcome = match operation {
                "list" => until_stopped(&stop, provider.list(&test_root))
                    .await
                    .map(|r| r.map(|_| ())),
                "stat" => until_stopped(&stop, provider.stat(&remote_path))
                    .await
                    .map(|r| r.map(|_| ())),
                _ => until_stopped(&stop, provider.delete(&remote_path)).await,
            };
            match outcome {
                None => {
                    stopped = true;
                    early_abort = true;
                    break 'outer;
                }
                Some(Ok(())) => {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    results.push(probe_result(
                        &protocol,
                        anonymize_extra,
                        &report_id,
                        operation,
                        &[ms],
                    ));
                    total_runs += 1;
                }
                Some(Err(e)) => errors.push(format!("{} failed: {}", operation, e)),
            }
        }
    }

    // Many-small-files axis (separate from the size sweep above): only run when
    // requested and when the size sweep did not already abort on a
    // fatal/connection error or a stop.
    if let Some(mf) = opts.many_files {
        if !early_abort && !stop.is_cancelled() && scratch_created {
            let outcome = run_many_files_workload(
                &mut provider,
                mf,
                &test_root,
                &protocol,
                anonymize_extra,
                &report_id,
                &stop,
                observer,
            )
            .await;
            total_bytes_transferred += outcome.bytes_transferred;
            total_runs += outcome.runs;
            fatal_failures += outcome.fatal;
            results.extend(outcome.results);
            errors.extend(outcome.errors);
            trash_notes.extend(outcome.notes);
            if outcome.stopped {
                stopped = true;
                early_abort = true;
            }
        }
    }

    // The measurement is over: the stop signal no longer applies to cleanup,
    // which has its own bound per step.
    stopper.abort();
    let timed_out = timed_out.load(Ordering::SeqCst);
    let cancelled = stopped && !timed_out;
    if timed_out {
        early_abort = true;
        errors.push(format!(
            "benchmark hit profile-timeout cap ({}s), aborting",
            profile_timeout_secs
        ));
    } else if cancelled {
        errors.push(CANCELLED_NOTE.to_string());
    }

    // Final cleanup: remove the test root recursively. Best-effort; any
    // residual is logged as an error but does not invalidate the run. It runs
    // after a stop too: a stopped run leaves nothing behind.
    let rmdir_ok = if scratch_created {
        match remove_and_purge(
            &mut provider,
            &test_root,
            &mut errors,
            &mut trash_notes,
            observer,
        )
        .await
        {
            Some(Ok(())) => true,
            Some(Err(e)) => {
                errors.push(format!(
                    "cleanup of {} returned error (manual review may be needed): {}",
                    test_root, e
                ));
                false
            }
            None => {
                errors.push(format!(
                    "cleanup of {} did not finish within {} s (manual review may be needed)",
                    test_root,
                    CLEANUP_STEP_TIMEOUT.as_secs()
                ));
                false
            }
        }
    } else {
        false
    };
    // A transfer the stop cut short can still complete on the server after
    // the removal above, and several servers (Koofr) create the folders of an
    // upload's path: the run folder came back, empty, after a Ctrl+C. Look
    // once more after the request has had time to settle.
    if rmdir_ok && stopped {
        tokio::time::sleep(Duration::from_secs(3)).await;
        // Only a confirmed absence closes the cleanup: a check that failed or
        // did not finish proves nothing, and the report says so (L2B-06).
        match bounded(provider.exists(&test_root)).await {
            Some(Ok(false)) => {}
            Some(Ok(true)) => {
                if !matches!(
                    remove_and_purge(
                        &mut provider,
                        &test_root,
                        &mut errors,
                        &mut trash_notes,
                        observer
                    )
                    .await,
                    Some(Ok(()))
                ) {
                    errors.push(format!(
                        "cleanup of {} after the stop: it came back and could not be removed again (manual review may be needed)",
                        test_root
                    ));
                }
            }
            Some(Err(e)) => errors.push(format!(
                "cleanup of {} after the stop could not be verified: looking for it again failed: {} (manual review may be needed)",
                test_root, e
            )),
            None => errors.push(format!(
                "cleanup of {} after the stop could not be verified: looking for it again did not finish within {} s (manual review may be needed)",
                test_root,
                CLEANUP_STEP_TIMEOUT.as_secs()
            )),
        }
    }

    if opts.test_root_prefix.is_none() {
        // Remove the shared `aeroftp-bench` base dir too (issue #368: the
        // reporter had to delete it by hand on Google Drive, MEGA and
        // kDrive). Here `bench_base` is always OUR `aeroftp-bench` folder, so
        // it is safe to remove when empty, whether this run created it or a
        // previous one did: that is exactly why it accumulated empty on
        // Drive/kDrive/MEGA before. Emptiness-guarded so a folder that
        // unexpectedly still holds data (soft-delete lag) is left in place
        // and reported, never force-deleted.
        remove_if_empty(
            &mut provider,
            &bench_base,
            &mut errors,
            &mut trash_notes,
            observer,
        )
        .await;
    } else {
        // The prefix folders this run created, deepest first, each only when
        // empty: a folder the user already had is not in the list, and the
        // first one still holding something stops the walk up.
        for dir in created_by_run.iter().rev() {
            if !remove_if_empty(&mut provider, dir, &mut errors, &mut trash_notes, observer).await {
                break;
            }
        }
    }
    let mut notes: Vec<String> = provider
        .measurement_note()
        .map(|note| vec![note.to_string()])
        .unwrap_or_default();
    // A measurement note marks a local bridge; the trash notes after it do not.
    let bridge_note = !notes.is_empty();
    notes.extend(trash_notes);
    let _ = bounded(provider.disconnect()).await;

    // Close the IP fairness check (issue #368 #1): if the public IP changed
    // since the start, flag the run as not comparable.
    if let Some(start_ip) = &start_public_ip {
        if let Some(end_ip) = benchmark_public_ip().await {
            if &end_ip != start_ip {
                errors.push(BENCHMARK_IP_CHANGED_WARNING.to_string());
            }
        }
    }

    // Yandex region hint (issue #368 #3): Yandex Disk endpoints are unreachable
    // from some countries and VPN exit nodes and the request hangs until the
    // timeout. If any error references a Yandex host, or a Yandex run timed
    // out, add a one-line hint so the user understands the failure is
    // region/VPN related, not an AeroFTP bug.
    if errors.iter().any(|e| e.to_lowercase().contains("yandex"))
        || (timed_out && opts.service.to_lowercase().contains("yandex"))
    {
        errors.push(
            "hint: Yandex Disk is unreachable from some countries and VPN exit nodes; if it hangs or fails to connect, switch your VPN region and re-run".to_string(),
        );
    }

    let total_duration_ms = total_start.elapsed().as_millis() as u64;

    let environment = BenchmarkEnvironment {
        // ASN/country lookup is server-side responsibility (Phase 2): the
        // aggregator fills these from the submitter's network at submission
        // time, so a run never records them, publish consent or not.
        asn_bucket: None,
        country_bucket: None,
        tod_bucket: benchmark_tod_bucket().into(),
        os_family: super::benchmark_os_family(),
        os_arch: std::env::consts::ARCH.into(),
        cpu_class: super::benchmark_cpu_class(),
    };

    // A round trip that ends on this machine measures the bridge: MEGAcmd
    // (its own local server), a Filen Desktop preset (the note says so), or
    // any profile aimed at a loopback host.
    // A round trip that ends on this machine measures the bridge (MEGAcmd,
    // a Filen Desktop preset, a loopback host). AeroFTP's own MEGA and Proton
    // clients keep the account tree in memory and answer `stat` from it, so
    // theirs is a lookup in this process (measured: 0.002 to 0.04 ms).
    let (_, local_bridge, metadata_cached) = benchmark_endpoint_kind(
        client,
        provider_access_label,
        opts.local_endpoint,
        bridge_note,
    );
    let signal = benchmark_signal_from(&results, local_bridge, metadata_cached);

    let mut private_paths: Vec<(String, &'static str)> = vec![
        (test_root.clone(), "<run-folder>"),
        (bench_base.clone(), "<bench-folder>"),
        (initial_path.to_string(), "<start-path>"),
    ];
    if let Some(prefix) = opts.test_root_prefix.as_deref() {
        private_paths.push((prefix.to_string(), "<bench-folder>"));
    }
    private_paths.extend(created_by_run.iter().map(|d| (d.clone(), "<bench-folder>")));

    let report = BenchmarkReport {
        schema_version: 1,
        report_id,
        generated_at: benchmark_rounded_hour_utc(),
        cli: BenchmarkCliMeta {
            version: env!("CARGO_PKG_VERSION").to_string(),
            build_target: std::env::var("TARGET")
                .unwrap_or_else(|_| std::env::consts::ARCH.to_string()),
            rustc: option_env!("RUSTC_VERSION")
                .unwrap_or("unknown")
                .to_string(),
        },
        level: opts.level,
        access,
        service: opts.service.clone(),
        // Set by the compare sweep for a fan-out run; a standalone run has no mode.
        mode: None,
        environment,
        consent: BenchmarkConsent {
            publish: opts.consent_publish,
            anonymize_extra,
        },
        results,
        summary: BenchmarkSummary {
            total_runs,
            total_bytes_transferred,
            total_duration_ms,
            errors,
        },
        notes,
        signal,
        private_paths,
    };

    Ok(BenchmarkOutcome {
        report,
        early_abort,
        cancelled,
        timed_out,
        failed: fatal_failures > 0,
    })
}

/// Whether the concrete client is AeroFTP's own Proton Drive client.
#[cfg(feature = "proton-native")]
fn proton_native(concrete: &dyn std::any::Any) -> bool {
    concrete.is::<crate::providers::proton_native::provider::ProtonNativeProvider>()
}

#[cfg(not(feature = "proton-native"))]
fn proton_native(_concrete: &dyn std::any::Any) -> bool {
    false
}

/// Why the run could not start: the scratch folder, where it was created
/// (a relative one lands in the profile's start folder) and the provider's
/// answer, so a report names the folder that refused (#368, Ehud's OneDrive).
pub(super) fn scratch_dir_error(
    test_root: &str,
    start_folder: &str,
    error: &ProviderError,
) -> String {
    let start = if start_folder.trim().is_empty() {
        "/"
    } else {
        start_folder.trim()
    };
    let place = if test_root.starts_with('/') {
        String::new()
    } else {
        format!(" in the profile's start folder '{start}'")
    };
    format!(
        "benchmark cannot create its scratch folder '{test_root}'{place}: {error}. The provider refused to create a folder there; check that the start folder exists and is writable, or point the run at a writable folder with --test-root-prefix."
    )
}

/// Purge `path` from the provider's trash after a soft delete, and say so.
async fn purge_from_trash(
    provider: &mut Box<dyn StorageProvider>,
    path: &str,
    errors: &mut Vec<String>,
    notes: &mut Vec<String>,
    observer: &dyn BenchmarkObserver,
) {
    // A provider that keeps a trash it cannot purge (pCloud through OAuth,
    // Koofr): the folder stays there by design, which is a note, not a
    // failed purge (#368, Ehud).
    if let Some(reason) = provider.trash_left_unpurged() {
        notes.push(trash_kept_note(path, reason));
        return;
    }
    match bounded(provider.delete_permanent(path)).await {
        Some(outcome) => {
            if note_trash_purge(path, outcome, errors, notes) {
                observer.event(BenchmarkEvent::TrashPurged { path });
            }
        }
        None => errors.push(format!(
            "trash purge of {} did not finish within {} s, check the provider's trash",
            path,
            CLEANUP_STEP_TIMEOUT.as_secs()
        )),
    }
}

/// Remove the folder `path` with everything in it, and purge it from the
/// trash where the client can. A recursive delete on consumer clouds (Google
/// Drive, Dropbox, OneDrive, Box, MEGA, Yandex, FileLu, Internxt, kDrive,
/// Zoho, pCloud, Jottacloud, OpenDrive) is a soft delete, so the folder is
/// purged after it, or quotas fill up across repeated runs. A client whose
/// purge works on a live path (Proton's CLI) is asked to remove and purge in
/// one step instead: after a plain delete its purge finds nothing, and the
/// folder stayed in the trash without a word. `None` when the removal did not
/// finish within the cleanup bound; purge outcomes go to `errors`.
async fn remove_and_purge(
    provider: &mut Box<dyn StorageProvider>,
    path: &str,
    errors: &mut Vec<String>,
    notes: &mut Vec<String>,
    observer: &dyn BenchmarkObserver,
) -> Option<Result<(), ProviderError>> {
    let live = provider.purges_live_path();
    let mut live_purge_failed = false;
    if live {
        match bounded(provider.delete_permanent(path)).await {
            Some(Ok(true)) => {
                observer.event(BenchmarkEvent::TrashPurged { path });
                return Some(Ok(()));
            }
            // Nothing at that path any more: nothing to remove.
            Some(Ok(false)) => return Some(Ok(())),
            // The purge failed: remove it plainly, so the folder at least
            // leaves the drive, and say where it went.
            Some(Err(e)) => {
                live_purge_failed = true;
                errors.push(format!(
                    "trash purge of {} failed, check the provider's trash: {}",
                    path, e
                ));
            }
            None => return None,
        }
    }
    let removed = bounded(provider.rmdir_recursive(path)).await;
    // A live purge can fail after it moved the folder to the trash: then the
    // path is gone, which the purge error above already reports.
    if live_purge_failed && matches!(removed, Some(Err(ProviderError::NotFound(_)))) {
        return Some(Ok(()));
    }
    if !live && matches!(removed, Some(Ok(()))) {
        purge_from_trash(provider, path, errors, notes, observer).await;
    }
    removed
}

/// Remove the scratch folder `dir` and purge it from the trash, only when it
/// is empty. `true` when it is gone. A folder that still holds something, or
/// whose emptiness cannot be confirmed, is left in place.
async fn remove_if_empty(
    provider: &mut Box<dyn StorageProvider>,
    dir: &str,
    errors: &mut Vec<String>,
    notes: &mut Vec<String>,
    observer: &dyn BenchmarkObserver,
) -> bool {
    let empty = match bounded(provider.list(dir)).await {
        Some(Ok(entries)) => entries.is_empty(),
        // If we cannot confirm emptiness, leave the folder in place rather
        // than risk deleting non-benchmark data.
        _ => false,
    };
    if !empty {
        errors.push(format!(
            "note: scratch folder '{}' left in place (not empty after cleanup)",
            dir
        ));
        return false;
    }
    // Removed and hard-purged from the trash on consumer clouds, mirroring
    // the test_root, refusal included: its result used to be dropped, so a
    // folder left in the bin said nothing (#368).
    if matches!(
        remove_and_purge(provider, dir, errors, notes, observer).await,
        Some(Ok(()))
    ) {
        true
    } else {
        errors.push(format!(
            "note: empty scratch folder '{}' could not be removed automatically; delete it manually",
            dir
        ));
        false
    }
}

/// Create a remote directory and every missing parent (mkdir -p semantics).
///
/// Several backends do not auto-create intermediate path components and reject
/// a folder creation whose parent collection does not exist. pCloud WebDAV
/// (issue #368) returned `Parent directory does not exist` for the scratch
/// `aeroftp-bench/<uuid>` tree because the `aeroftp-bench` base had not been
/// created first. The benchmark scratch tree is at least two levels deep under
/// the profile root, so we create each component in turn, treating
/// `AlreadyExists` as success. Returns the error of the deepest component that
/// still failed (so the caller can decide whether to hard-fail), or `Ok` when
/// the full path now exists.
async fn mkdir_p(provider: &mut Box<dyn StorageProvider>, path: &str) -> Result<(), ProviderError> {
    let mut last_err: Option<ProviderError> = None;
    for acc in benchmark_mkdir_ladder(path) {
        match provider.mkdir(&acc).await {
            Ok(()) | Err(ProviderError::AlreadyExists(_)) => last_err = None,
            Err(e) => last_err = Some(e),
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Accumulated output of the many-small-files workload, merged back into the
/// main benchmark report by the caller.
struct ManyFilesOutcome {
    results: Vec<BenchmarkResult>,
    bytes_transferred: u64,
    runs: u32,
    errors: Vec<String>,
    /// What the run left by design, as the trash a client cannot empty.
    notes: Vec<String>,
    /// Files of a batch that failed.
    fatal: u32,
    stopped: bool,
}

/// Files per second of a batch, from the time spent in the provider calls
/// alone: preparing each file's content and waiting for a written-back upload
/// to become readable are not part of what the batch measures.
fn files_per_second(per_file_ms: &[f64]) -> f64 {
    let secs = per_file_ms.iter().sum::<f64>() / 1000.0;
    per_file_ms.len() as f64 / secs.max(1e-6)
}

/// Build a single batch-operation result for the many-small-files axis.
/// `per_file_ms` carries the per-file latency distribution (its p50 is the
/// median per-file time); `files_per_second` is the headline rate over the
/// whole batch.
#[allow(clippy::too_many_arguments)]
pub(super) fn many_files_result(
    protocol: &str,
    anonymize_extra: bool,
    report_id: &str,
    operation: &str,
    payload_size_bytes: u64,
    files: u32,
    files_per_second: f64,
    per_file_ms: &[f64],
    throughput_mbps: Option<&[f64]>,
    fatal: u32,
) -> BenchmarkResult {
    let (hint, hash) = benchmark_provider_hint(protocol, anonymize_extra, report_id);
    let raw_runs = per_file_ms
        .iter()
        .map(|ms| BenchmarkRawRun {
            duration_ms: *ms as u64,
            bytes: payload_size_bytes,
            throughput_mbps: None,
        })
        .collect();
    BenchmarkResult {
        protocol: protocol.to_string(),
        provider_hint: hint,
        provider_hash: hash,
        operation: operation.to_string(),
        payload_size_bytes,
        // `runs` is the number of timed measurements (one list-dir call vs. one
        // per file for the batch ops); `file_count` is how many files the op
        // touched (the N entries listed, the N files uploaded, etc.).
        runs: per_file_ms.len() as u32,
        warmup_runs_discarded: 0,
        file_count: Some(files),
        files_per_second: Some(files_per_second),
        throughput_mbps: throughput_mbps.map(benchmark_stats_from),
        latency_ms: benchmark_stats_from(per_file_ms),
        tls_handshake_ms: None,
        errors: BenchmarkErrors {
            transient: 0,
            fatal,
        },
        raw_runs,
    }
}

/// Run the many-small-files axis: create N files of a fixed small size and
/// measure upload-all / list-dir / stat-all / download-all / delete-all,
/// reporting files/sec and per-file latency. This is the workload where
/// per-file overhead (handshake, signing, metadata round-trips) dominates and
/// S3 typically beats WebDAV/FTP.
///
/// Unlike the single-file size sweep (which goes through the transfer DAG to
/// mirror a real transfer), these ops call the provider directly so the numbers
/// isolate the protocol's raw per-file cost, the dimension being compared.
///
/// A stop ends the upload, stat and download loops after the current file;
/// delete-all still runs over every file that landed, each call bounded, so
/// the scratch tree is left empty for the final cleanup.
#[allow(clippy::too_many_arguments)]
async fn run_many_files_workload(
    provider: &mut Box<dyn StorageProvider>,
    mf: ManyFilesConfig,
    test_root: &str,
    protocol: &str,
    anonymize_extra: bool,
    report_id: &str,
    stop: &CancellationToken,
    observer: &dyn BenchmarkObserver,
) -> ManyFilesOutcome {
    let mut out = ManyFilesOutcome {
        results: Vec::new(),
        bytes_transferred: 0,
        runs: 0,
        errors: Vec::new(),
        notes: Vec::new(),
        fatal: 0,
        stopped: false,
    };

    let many_dir = format!("{}/manyfiles", test_root);
    match until_stopped(stop, provider.mkdir(&many_dir)).await {
        None => {
            out.stopped = true;
            return out;
        }
        Some(Ok(())) | Some(Err(ProviderError::AlreadyExists(_))) => {}
        Some(Err(e)) => {
            out.errors.push(format!(
                "many-files: cannot create scratch dir '{}': {}",
                many_dir, e
            ));
            return out;
        }
    }

    let size = mf.file_size_bytes;
    let run_id = report_id.get(..8).unwrap_or(report_id);
    let remote_name =
        |i: u32| crate::speed_payload::name(&format!("{}/f{:06}-{}", many_dir, i, run_id));

    observer.event(BenchmarkEvent::ManyFiles {
        file_count: mf.file_count,
        file_size: size,
    });

    // ── upload-all ──────────────────────────────────────────────────
    let local_up = match NamedTempFile::new() {
        Ok(f) => f,
        Err(e) => {
            out.errors
                .push(format!("many-files: cannot create local temp: {}", e));
            return out;
        }
    };
    let local_up_path = local_up.path().to_string_lossy().to_string();
    let mut up_ms: Vec<f64> = Vec::new();
    let mut up_mbps: Vec<f64> = Vec::new();
    let mut up_fatal = 0u32;
    let mut uploaded = 0u32;
    observer.event(BenchmarkEvent::BatchStarted {
        operation: "upload-all",
        total: mf.file_count as u64,
    });
    for i in 0..mf.file_count {
        if stop.is_cancelled() {
            out.stopped = true;
            break;
        }
        // Fresh random content per file (untimed) so content-dedup backends
        // cannot short-circuit the upload and inflate files/sec.
        if let Err(e) = crate::speed_payload::write_random(local_up.path(), size) {
            out.errors.push(format!("many-files: {}", e));
            break;
        }
        let remote = remote_name(i);
        let start = Instant::now();
        match until_stopped(stop, provider.upload(&local_up_path, &remote, None)).await {
            None => {
                out.stopped = true;
                break;
            }
            Some(Ok(())) => {
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                up_ms.push(ms);
                up_mbps.push(mbps(size, ms));
                uploaded += 1;
                out.bytes_transferred += size;
                out.runs += 1;
                observer.event(BenchmarkEvent::BatchItem {
                    operation: "upload-all",
                });
            }
            Some(Err(e)) => {
                up_fatal += 1;
                out.errors
                    .push(format!("many-files: upload of file {} failed: {}", i, e));
                break;
            }
        }
    }
    observer.event(BenchmarkEvent::BatchFinished {
        operation: "upload-all",
    });
    out.fatal += up_fatal;
    if uploaded > 0 || up_fatal > 0 {
        out.results.push(many_files_result(
            protocol,
            anonymize_extra,
            report_id,
            "upload-all",
            size,
            uploaded,
            if uploaded > 0 {
                files_per_second(&up_ms)
            } else {
                0.0
            },
            &up_ms,
            (uploaded > 0).then_some(up_mbps.as_slice()),
            up_fatal,
        ));
    }
    if uploaded == 0 {
        // Nothing landed remotely: the remaining ops have no payload to act on.
        return out;
    }

    // ── list-dir (one call, returns N entries) ──────────────────────
    if !out.stopped {
        let list_start = Instant::now();
        match until_stopped(stop, provider.list(&many_dir)).await {
            None => out.stopped = true,
            Some(Ok(entries)) => {
                let ms = list_start.elapsed().as_secs_f64() * 1000.0;
                let listed = entries.iter().filter(|e| !e.is_dir).count() as u32;
                let fps = listed as f64 / (ms / 1000.0).max(1e-6);
                out.results.push(many_files_result(
                    protocol,
                    anonymize_extra,
                    report_id,
                    "list-dir",
                    0,
                    listed,
                    fps,
                    &[ms],
                    None,
                    0,
                ));
                out.runs += 1;
            }
            Some(Err(e)) => out
                .errors
                .push(format!("many-files: list-dir failed: {}", e)),
        }
    }

    // ── stat-all ────────────────────────────────────────────────────
    let mut stat_ms: Vec<f64> = Vec::new();
    let mut stat_fatal = 0u32;
    for i in 0..uploaded {
        if out.stopped || stop.is_cancelled() {
            out.stopped = true;
            break;
        }
        let start = Instant::now();
        match until_stopped(stop, provider.stat(&remote_name(i))).await {
            None => {
                out.stopped = true;
                break;
            }
            Some(Ok(_)) => stat_ms.push(start.elapsed().as_secs_f64() * 1000.0),
            Some(Err(e)) => {
                stat_fatal += 1;
                out.errors
                    .push(format!("many-files: stat of file {} failed: {}", i, e));
            }
        }
    }
    out.fatal += stat_fatal;
    if !stat_ms.is_empty() {
        out.results.push(many_files_result(
            protocol,
            anonymize_extra,
            report_id,
            "stat-all",
            0,
            stat_ms.len() as u32,
            files_per_second(&stat_ms),
            &stat_ms,
            None,
            stat_fatal,
        ));
        out.runs += stat_ms.len() as u32;
    }

    // ── download-all ────────────────────────────────────────────────
    let local_dn = if out.stopped {
        None
    } else {
        match NamedTempFile::new() {
            Ok(f) => Some(f),
            Err(e) => {
                out.errors
                    .push(format!("many-files: cannot create download temp: {}", e));
                None
            }
        }
    };
    if let Some(local_dn) = local_dn {
        let local_dn_path = local_dn.path().to_string_lossy().to_string();
        let mut dn_ms: Vec<f64> = Vec::new();
        let mut dn_mbps: Vec<f64> = Vec::new();
        let mut dn_fatal = 0u32;
        observer.event(BenchmarkEvent::BatchStarted {
            operation: "download-all",
            total: uploaded as u64,
        });
        for i in 0..uploaded {
            if stop.is_cancelled() {
                out.stopped = true;
                break;
            }
            wait_until_upload_readable(provider, &remote_name(i), stop, observer).await;
            let start = Instant::now();
            match until_stopped(
                stop,
                provider.download(&remote_name(i), &local_dn_path, None),
            )
            .await
            {
                None => {
                    out.stopped = true;
                    break;
                }
                Some(Ok(())) => {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    dn_ms.push(ms);
                    dn_mbps.push(mbps(size, ms));
                    out.bytes_transferred += size;
                    out.runs += 1;
                    observer.event(BenchmarkEvent::BatchItem {
                        operation: "download-all",
                    });
                }
                Some(Err(e)) => {
                    dn_fatal += 1;
                    out.errors
                        .push(format!("many-files: download of file {} failed: {}", i, e));
                }
            }
        }
        observer.event(BenchmarkEvent::BatchFinished {
            operation: "download-all",
        });
        out.fatal += dn_fatal;
        if !dn_ms.is_empty() {
            out.results.push(many_files_result(
                protocol,
                anonymize_extra,
                report_id,
                "download-all",
                size,
                dn_ms.len() as u32,
                files_per_second(&dn_ms),
                &dn_ms,
                Some(&dn_mbps),
                dn_fatal,
            ));
        }
    }

    // Capture stable identities in one provider batch, outside every measured
    // phase. `Ok(None)` is a provider with no per-file identity and still
    // purges by path. An `Err` is a failed capture: it must not become that
    // path-only purge, which on Nextcloud can delete another generation.
    let live_paths: Vec<String> = (0..uploaded).map(&remote_name).collect();
    let mut identities = match bounded(provider.capture_trash_identities(&live_paths)).await {
        Some(captured) if captured.len() == uploaded as usize => captured,
        Some(captured) => {
            out.errors.push(format!(
                "many-files: identity capture returned {} results for {} files",
                captured.len(),
                uploaded
            ));
            Vec::new()
        }
        None => {
            out.errors
                .push("many-files: identity capture exceeded its cleanup bound".into());
            Vec::new()
        }
    };
    identities.resize_with(uploaded as usize, || {
        Err(ProviderError::Other("identity was not captured".into()))
    });
    let mut identities: Vec<Result<Option<String>, String>> = identities
        .into_iter()
        .enumerate()
        .map(|(i, captured)| match captured {
            Ok(identity) => Ok(identity),
            Err(error) => {
                let message = error.to_string();
                out.errors.push(format!(
                    "many-files: identity capture of file {i} failed: {message}"
                ));
                Err(message)
            }
        })
        .collect();

    // ── delete-all (also clears the scratch files) ──────────────────
    // Runs after a stop too, each call bounded: it is the cleanup of what
    // landed, and the final recursive removal is the fallback.
    let mut del_ms: Vec<f64> = Vec::new();
    let mut del_fatal = 0u32;
    let mut deleted: Vec<u32> = Vec::new();
    for i in 0..uploaded {
        let start = Instant::now();
        match bounded(provider.delete(&remote_name(i))).await {
            Some(Ok(())) => {
                del_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                deleted.push(i);
            }
            Some(Err(e)) => {
                del_fatal += 1;
                out.errors
                    .push(format!("many-files: delete of file {} failed: {}", i, e));
            }
            None => {
                out.errors.push(format!(
                    "many-files: delete of file {} did not finish within {} s",
                    i,
                    CLEANUP_STEP_TIMEOUT.as_secs()
                ));
                break;
            }
        }
    }
    if !del_ms.is_empty() {
        out.results.push(many_files_result(
            protocol,
            anonymize_extra,
            report_id,
            "delete-all",
            0,
            del_ms.len() as u32,
            files_per_second(&del_ms),
            &del_ms,
            None,
            del_fatal,
        ));
        out.runs += del_ms.len() as u32;
    }

    // The purge costs about what a delete does: as long as delete-all took,
    // plus one step bound for a slow start.
    let budget =
        CLEANUP_STEP_TIMEOUT + Duration::from_secs_f64(del_ms.iter().sum::<f64>() / 1000.0);
    purge_deleted_files_with_identities(
        provider,
        &deleted,
        &remote_name,
        &mut identities,
        stop,
        budget,
        &mut out.errors,
        &mut out.notes,
    )
    .await;

    out
}

/// After the measured delete-all pass, purge from the trash each file it
/// deleted: on a soft-delete provider every one is a separate trash item, up
/// to 5 GiB per run, and purging the run folder later does not take them.
/// The purge stays outside the timing, so delete-all keeps measuring plain
/// deletes. `Ok(Some)` purges that identity. `Ok(None)` keeps the path-only
/// purge. `Err` skips the purge for that file and counts it as not purged:
/// a failed capture must not fall through to a path match. The folder
/// cleanup remains the fallback. Anything not verifiably purged is counted
/// in the report.
///
/// The whole pass ends at `budget` and at `stop`, a cancel or the profile
/// timeout: a server that stops answering must not hold the report one
/// cleanup bound per file. What was not purged is counted in the report.
#[allow(clippy::too_many_arguments)]
pub(super) async fn purge_deleted_files_with_identities(
    provider: &mut Box<dyn StorageProvider>,
    deleted: &[u32],
    remote_name: &(dyn Fn(u32) -> String + Sync),
    identities: &mut [Result<Option<String>, String>],
    stop: &CancellationToken,
    budget: Duration,
    errors: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    if deleted.is_empty() {
        return;
    }
    if let Some(reason) = provider.trash_left_unpurged() {
        notes.push(format!(
            "the {} files the many-files workload deleted went to the provider's trash and stay there: {}",
            deleted.len(),
            reason
        ));
        return;
    }
    let deadline = tokio::time::Instant::now() + budget;
    let mut failed = 0usize;
    let mut first_error: Option<String> = None;
    let mut not_purged: Option<(usize, String)> = None;
    for (n, &i) in deleted.iter().enumerate() {
        let left = deleted.len() - n;
        if stop.is_cancelled() {
            not_purged = Some((left, "the run was stopped".into()));
            break;
        }
        let path = remote_name(i);
        let identity = match identities.get(i as usize) {
            Some(Ok(identity)) => identity.as_deref(),
            Some(Err(reason)) => {
                failed += 1;
                first_error.get_or_insert_with(|| reason.clone());
                continue;
            }
            None => {
                failed += 1;
                first_error.get_or_insert_with(|| "identity was not captured".into());
                continue;
            }
        };
        let purge = until_stopped(
            stop,
            provider.delete_permanent_with_identity(&path, identity),
        );
        match tokio::time::timeout_at(deadline, purge).await {
            Ok(Some(Ok(true))) => {}
            Ok(Some(Ok(false))) if provider.purges_live_path() => {
                failed += 1;
                first_error.get_or_insert_with(|| {
                    "the live-path client found no captured identity to purge".into()
                });
            }
            Ok(Some(Ok(false))) => {}
            // The client cannot purge here by design: every file would
            // answer the same, and that is a note, not a failure.
            Ok(Some(Err(ProviderError::NotSupported(why)))) if failed == 0 => {
                notes.push(format!(
                    "the {} files the many-files workload deleted went to the provider's trash and stay there: {}",
                    deleted.len(),
                    why
                ));
                return;
            }
            Ok(Some(Err(e))) => {
                failed += 1;
                first_error.get_or_insert_with(|| e.to_string());
            }
            Ok(None) => {
                not_purged = Some((left, "the run was stopped".into()));
                break;
            }
            Err(_) => {
                not_purged = Some((
                    left,
                    format!(
                        "the purge took longer than its {} s limit",
                        budget.as_secs()
                    ),
                ));
                break;
            }
        }
    }
    if let Some(first) = first_error {
        errors.push(format!(
            "trash purge of {} of the {} files the many-files workload deleted failed, check the provider's trash: {}",
            failed,
            deleted.len(),
            first
        ));
    }
    if let Some((left, why)) = not_purged {
        errors.push(format!(
            "{} of the {} files the many-files workload deleted were not purged and stay in the provider's trash: {}",
            left,
            deleted.len(),
            why
        ));
    }
}
