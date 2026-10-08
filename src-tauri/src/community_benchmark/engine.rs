// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The run itself: scratch tree, single-file size sweep, many-small-files
//! workload, cleanup, report.
//!
//! The engine prints nothing. What a surface shows while it runs (the CLI's
//! phase lines and progress bars, AeroAgent's tool progress) comes from a
//! [`BenchmarkObserver`], so the measurement is the same code on every
//! surface and only the narration differs.

use std::time::Instant;

use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

use super::{
    benchmark_mkdir_ladder, benchmark_provider_hint, benchmark_public_ip, benchmark_remote_roots,
    benchmark_remote_roots_from_prefix, benchmark_rounded_hour_utc, benchmark_stats_from,
    benchmark_tod_bucket, note_trash_purge, BenchmarkCliMeta, BenchmarkConfig, BenchmarkConsent,
    BenchmarkEnvironment, BenchmarkErrors, BenchmarkLevel, BenchmarkRawRun, BenchmarkReport,
    BenchmarkResult, BenchmarkStats, BenchmarkSummary, ManyFilesConfig,
    BENCHMARK_IP_CHANGED_WARNING,
};
use crate::providers::{ProviderError, StorageProvider};
use crate::transfer_dag::TransferDirection;
use crate::transfer_dag_single_file::{
    pick_single_file_route, run_single_file_on_route, MultipartFailure, ProgressCallback,
};
use crate::util::size::format_size;

/// What happened during a run, for the surface to narrate.
#[derive(Debug, Clone, Copy)]
pub enum BenchmarkEvent<'a> {
    /// The shared `aeroftp-bench` base could not be created. Not fatal: the
    /// per-run folder is created with its parents right after.
    BaseDirNotCreated { error: &'a str },
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
}

fn is_cancelled(cancel: &Option<CancellationToken>) -> bool {
    cancel.as_ref().is_some_and(CancellationToken::is_cancelled)
}

const CANCELLED_NOTE: &str =
    "benchmark cancelled before the end; the results cover the runs completed until then";

fn single_run_result(
    protocol: &str,
    anonymize_extra: bool,
    report_id: &str,
    operation: &str,
    ms: f64,
) -> BenchmarkResult {
    let (h, hh) = benchmark_provider_hint(protocol, anonymize_extra, report_id);
    BenchmarkResult {
        protocol: protocol.to_string(),
        provider_hint: h,
        provider_hash: hh,
        operation: operation.into(),
        payload_size_bytes: 0,
        runs: 1,
        warmup_runs_discarded: 0,
        file_count: None,
        files_per_second: None,
        throughput_mbps: None,
        latency_ms: BenchmarkStats {
            p50: ms,
            p95: ms,
            stddev: 0.0,
            min: ms,
            max: ms,
        },
        tls_handshake_ms: None,
        errors: BenchmarkErrors {
            transient: 0,
            fatal: 0,
        },
        raw_runs: vec![BenchmarkRawRun {
            duration_ms: ms as u64,
            bytes: 0,
            throughput_mbps: None,
        }],
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
    observer: &dyn BenchmarkObserver,
) {
    if let Ok(Some(waited)) = provider.wait_until_readable(remote_path).await {
        observer.event(BenchmarkEvent::WaitedForReadable { waited });
    }
}

/// One timed transfer, through the router unless the run asked for direct
/// provider calls.
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
    observer: &dyn BenchmarkObserver,
) -> (Box<dyn StorageProvider>, Result<(), ProviderError>) {
    if opts.direct_transfers {
        let mut provider = provider;
        let result = match direction {
            TransferDirection::Upload => provider.upload(local, remote, None).await,
            TransferDirection::Download => provider.download(remote, local, None).await,
        };
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
        opts.cancel.clone(),
        // The payload is a temp file: a failed multipart upload will never be
        // resumed, so its provider session is aborted rather than left open.
        MultipartFailure::Abort,
    )
    .await;
    observer.transfer_finished();
    (provider, result)
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

    // IP fairness check (issue #368 #1): snapshot the public IP at the start
    // so we can warn if it changes mid-run (a VPN switch makes the numbers
    // incomparable).
    let start_public_ip = if opts.check_public_ip {
        benchmark_public_ip().await
    } else {
        None
    };

    let protocol = provider.provider_type().to_string();
    // Access-method label for the "Protocol" column (issue #277): computed from
    // the live provider type so it reflects the transport the factory actually
    // built (e.g. a Koofr-over-WebDAV profile reports `WebDAV`, native `REST API`).
    let access = if crate::crypt_overlay_provider::concrete_provider_mut(&mut *provider)
        .as_any_mut()
        .is::<crate::providers::mega::MegaCmdProvider>()
    {
        "CLI".to_string()
    } else {
        provider.provider_type().access_label().to_string()
    };

    let report_id = uuid::Uuid::new_v4().to_string();
    let (bench_base, test_root) = match opts.test_root_prefix.as_deref() {
        Some(prefix) => benchmark_remote_roots_from_prefix(prefix, &report_id),
        None => benchmark_remote_roots(initial_path, &report_id),
    };
    // Create the scratch directory tree before any upload. `bench_base` may
    // already exist from a prior run (`AlreadyExists` is fine), but `test_root`
    // is unique per run. The mkdir -p below also creates the base with
    // parents; doing it here first lets the surface warn clearly if the
    // profile root is not writable. The final cleanup needs no "did we create
    // it" flag: in the no-prefix case `bench_base` is always our own
    // `aeroftp-bench` folder and is safe to remove when empty (issue #368: the
    // reporter had to delete it by hand on every drive afterwards).
    match provider.mkdir(&bench_base).await {
        Ok(()) | Err(ProviderError::AlreadyExists(_)) => {}
        Err(e) => observer.event(BenchmarkEvent::BaseDirNotCreated {
            error: &e.to_string(),
        }),
    }
    // Create `test_root` with parents (mkdir -p). Some WebDAV servers (pCloud,
    // issue #368) refuse a folder whose parent collection does not yet exist
    // and do not auto-create intermediates, so a single mkdir of the nested
    // scratch path failed with "Parent directory does not exist" even though
    // the base creation above had also been rejected. Creating each component
    // in turn makes the scratch tree robust across all 22 backends.
    if let Err(e) = mkdir_p(&mut provider, &test_root).await {
        if !matches!(e, ProviderError::AlreadyExists(_)) {
            let _ = provider.disconnect().await;
            return Err(format!(
                "benchmark cannot create scratch dir '{}': {}. Provider may not allow folder creation in the configured root (try --test-root-prefix to point at a writable sub-path).",
                test_root, e
            ));
        }
    }

    let total_start = Instant::now();
    let mut results: Vec<BenchmarkResult> = Vec::new();
    let mut total_bytes_transferred: u64 = 0;
    let mut total_runs: u32 = 0;
    let mut errors: Vec<String> = Vec::new();
    let mut early_abort = false;
    let mut cancelled = false;

    'outer: for &size in &cfg.sizes_bytes {
        if is_cancelled(&opts.cancel) {
            cancelled = true;
            early_abort = true;
            break;
        }
        if total_start.elapsed().as_secs() > profile_timeout_secs {
            errors.push(format!(
                "benchmark hit profile-timeout cap ({}s), aborting",
                profile_timeout_secs
            ));
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
                if is_cancelled(&opts.cancel) {
                    cancelled = true;
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
                    let _ = provider.delete(&remote_path).await;
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
                    observer,
                )
                .await;
                provider = returned;
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                match upload_result {
                    Ok(()) => {
                        if !is_warmup {
                            upload_durations_ms.push(elapsed_ms);
                            let mbps =
                                (size as f64 * 8.0) / 1_000_000.0 / (elapsed_ms / 1000.0).max(1e-6);
                            upload_throughput_mbps.push(mbps);
                            upload_raw.push(BenchmarkRawRun {
                                duration_ms: elapsed_ms as u64,
                                bytes: size,
                                throughput_mbps: Some(mbps),
                            });
                            total_bytes_transferred += size;
                            total_runs += 1;
                        }
                    }
                    // A transfer the cancel cut short is not a failure of the
                    // provider: it is not counted, and the runs of this size
                    // measured before it are still reported.
                    Err(_) if is_cancelled(&opts.cancel) => {
                        cancelled = true;
                        early_abort = true;
                        break 'runs;
                    }
                    Err(e) => {
                        upload_fatal += 1;
                        let (h, hh) =
                            benchmark_provider_hint(&protocol, anonymize_extra, &report_id);
                        results.push(BenchmarkResult {
                            protocol: protocol.clone(),
                            provider_hint: h,
                            provider_hash: hh,
                            operation: "upload".into(),
                            payload_size_bytes: size,
                            runs: 0,
                            warmup_runs_discarded: cfg.warmup_runs,
                            file_count: None,
                            files_per_second: None,
                            throughput_mbps: None,
                            latency_ms: BenchmarkStats {
                                p50: 0.0,
                                p95: 0.0,
                                stddev: 0.0,
                                min: 0.0,
                                max: 0.0,
                            },
                            tls_handshake_ms: None,
                            errors: BenchmarkErrors {
                                transient: upload_transient,
                                fatal: upload_fatal,
                            },
                            raw_runs: Vec::new(),
                        });
                        errors.push(format!("upload {} bytes failed: {}", format_size(size), e));
                        early_abort = true;
                        break 'outer;
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
                if is_cancelled(&opts.cancel) {
                    cancelled = true;
                    early_abort = true;
                    break 'runs;
                }
                wait_until_upload_readable(&mut provider, &remote_path, observer).await;
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
                    observer,
                )
                .await;
                provider = returned;
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                match dl_result {
                    Ok(()) => {
                        if !is_warmup {
                            download_durations_ms.push(elapsed_ms);
                            let mbps =
                                (size as f64 * 8.0) / 1_000_000.0 / (elapsed_ms / 1000.0).max(1e-6);
                            download_throughput_mbps.push(mbps);
                            download_raw.push(BenchmarkRawRun {
                                duration_ms: elapsed_ms as u64,
                                bytes: size,
                                throughput_mbps: Some(mbps),
                            });
                            total_bytes_transferred += size;
                            total_runs += 1;
                        }
                    }
                    Err(_) if is_cancelled(&opts.cancel) => {
                        cancelled = true;
                        early_abort = true;
                        break 'runs;
                    }
                    Err(e) => {
                        if !is_warmup {
                            download_fatal += 1;
                        } else {
                            download_transient += 1;
                        }
                        errors.push(format!("download {} bytes failed: {}", size, e));
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
        }

        // A cancel keeps the runs of this size measured above and stops here:
        // the remaining one-shot probes would be measured after the user said
        // stop.
        if cancelled {
            break;
        }

        // list / stat / delete are measured once per size since they do not
        // benefit from multiple runs in the same way as throughput tests.
        if cfg.operations.contains(&"list") {
            let start = Instant::now();
            match provider.list(&test_root).await {
                Ok(_) => {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    results.push(single_run_result(
                        &protocol,
                        anonymize_extra,
                        &report_id,
                        "list",
                        ms,
                    ));
                    total_runs += 1;
                }
                Err(e) => errors.push(format!("list failed: {}", e)),
            }
        }

        if cfg.operations.contains(&"stat") {
            let start = Instant::now();
            match provider.stat(&remote_path).await {
                Ok(_) => {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    results.push(single_run_result(
                        &protocol,
                        anonymize_extra,
                        &report_id,
                        "stat",
                        ms,
                    ));
                    total_runs += 1;
                }
                Err(e) => errors.push(format!("stat failed: {}", e)),
            }
        }

        if cfg.operations.contains(&"delete") {
            let start = Instant::now();
            match provider.delete(&remote_path).await {
                Ok(()) => {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    results.push(single_run_result(
                        &protocol,
                        anonymize_extra,
                        &report_id,
                        "delete",
                        ms,
                    ));
                    total_runs += 1;
                }
                Err(e) => errors.push(format!("delete failed: {}", e)),
            }
        }
    }

    // Many-small-files axis (separate from the size sweep above): only run when
    // requested and when the size sweep did not already abort on a
    // fatal/connection error or a cancel.
    if let Some(mf) = opts.many_files {
        if !early_abort && total_start.elapsed().as_secs() <= profile_timeout_secs {
            let outcome = run_many_files_workload(
                &mut provider,
                mf,
                &test_root,
                &protocol,
                anonymize_extra,
                &report_id,
                profile_timeout_secs,
                total_start,
                &opts.cancel,
                observer,
            )
            .await;
            total_bytes_transferred += outcome.bytes_transferred;
            total_runs += outcome.runs;
            results.extend(outcome.results);
            errors.extend(outcome.errors);
            if outcome.cancelled {
                cancelled = true;
                early_abort = true;
            }
        }
    }
    if cancelled {
        errors.push(CANCELLED_NOTE.to_string());
    }

    // Final cleanup: remove the test root recursively. Best-effort; any
    // residual is logged as an error but does not invalidate the run. It runs
    // after a cancel too: a stopped run leaves nothing behind.
    let rmdir_ok = match provider.rmdir_recursive(&test_root).await {
        Ok(()) => true,
        Err(e) => {
            // Some providers may not implement rmdir_recursive against a path
            // they did not auto-create. We still try to delete the parent.
            errors.push(format!(
                "cleanup of {} returned error (manual review may be needed): {}",
                test_root, e
            ));
            false
        }
    };
    // Trash purge: rmdir_recursive on consumer cloud providers (Google Drive,
    // Dropbox, OneDrive, Box, MEGA, Yandex, FileLu, Internxt, kDrive, Zoho,
    // pCloud, Jottacloud, OpenDrive) is a soft delete and the test root ends
    // up in the recycle bin. Hard-purge it so quotas do not silently fill up
    // across repeated benchmark runs. No-op for FTP/SFTP/S3/plain WebDAV.
    if rmdir_ok {
        let outcome = provider.delete_permanent(&test_root).await;
        if note_trash_purge(&test_root, outcome, &mut errors) {
            observer.event(BenchmarkEvent::TrashPurged { path: &test_root });
        }
    }

    // Remove the shared `aeroftp-bench` base dir too (issue #368: the reporter
    // had to delete it by hand on Google Drive, MEGA and kDrive). ONLY in the
    // no-prefix case: there `bench_base` is always OUR `aeroftp-bench` folder, so
    // it is always safe to remove when empty. With a test-root prefix,
    // `bench_base` IS the user's own chosen directory (see
    // benchmark_remote_roots_from_prefix), which we must never auto-remove.
    // We intentionally do not gate on whether this run created the base: on a
    // second run the base already exists but the folder is still ours to
    // clean, which is exactly why it accumulated empty on Drive/kDrive/MEGA
    // before. Emptiness-guarded so a folder that unexpectedly still holds data
    // (soft-delete lag) is left in place and reported, never force-deleted.
    if opts.test_root_prefix.is_none() {
        let base_empty = match provider.list(&bench_base).await {
            Ok(entries) => entries.is_empty(),
            // If we cannot confirm emptiness, leave the base dir in place
            // rather than risk deleting non-benchmark data.
            Err(_) => false,
        };
        if base_empty {
            if provider.rmdir_recursive(&bench_base).await.is_ok() {
                // Hard-purge the soft-deleted base on consumer clouds, mirroring
                // the test_root trash purge above, refusal included: its result
                // used to be dropped, so a base left in the bin said nothing (#368).
                let outcome = provider.delete_permanent(&bench_base).await;
                if note_trash_purge(&bench_base, outcome, &mut errors) {
                    observer.event(BenchmarkEvent::TrashPurged { path: &bench_base });
                }
            } else {
                errors.push(format!(
                    "note: empty scratch folder '{}' could not be removed automatically; delete it manually",
                    bench_base
                ));
            }
        } else {
            errors.push(format!(
                "note: scratch folder '{}' left in place (not empty after cleanup)",
                bench_base
            ));
        }
    }
    let notes = provider
        .measurement_note()
        .map(|note| vec![note.to_string()])
        .unwrap_or_default();
    let _ = provider.disconnect().await;

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
    // timeout. If any error references a Yandex host, add a one-line hint so the
    // user understands the failure is region/VPN related, not an AeroFTP bug.
    if errors.iter().any(|e| e.to_lowercase().contains("yandex")) {
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
    };

    Ok(BenchmarkOutcome {
        report,
        early_abort,
        cancelled,
    })
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
    cancelled: bool,
}

/// Build a single batch-operation result for the many-small-files axis.
/// `per_file_ms` carries the per-file latency distribution (its p50 is the mean
/// per-file time); `files_per_second` is the headline rate over the whole batch.
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
/// A cancel stops the upload, stat and download loops after the current file;
/// delete-all still runs over every file that landed, so the scratch tree is
/// left empty for the final cleanup.
#[allow(clippy::too_many_arguments)]
async fn run_many_files_workload(
    provider: &mut Box<dyn StorageProvider>,
    mf: ManyFilesConfig,
    test_root: &str,
    protocol: &str,
    anonymize_extra: bool,
    report_id: &str,
    deadline_secs: u64,
    total_start: Instant,
    cancel: &Option<CancellationToken>,
    observer: &dyn BenchmarkObserver,
) -> ManyFilesOutcome {
    let mut out = ManyFilesOutcome {
        results: Vec::new(),
        bytes_transferred: 0,
        runs: 0,
        errors: Vec::new(),
        cancelled: false,
    };

    let many_dir = format!("{}/manyfiles", test_root);
    if let Err(e) = provider.mkdir(&many_dir).await {
        if !matches!(e, ProviderError::AlreadyExists(_)) {
            out.errors.push(format!(
                "many-files: cannot create scratch dir '{}': {}",
                many_dir, e
            ));
            return out;
        }
    }

    let size = mf.file_size_bytes;
    let remote_name = |i: u32| crate::speed_payload::name(&format!("{}/f{:06}", many_dir, i));

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
    let up_start = Instant::now();
    observer.event(BenchmarkEvent::BatchStarted {
        operation: "upload-all",
        total: mf.file_count as u64,
    });
    for i in 0..mf.file_count {
        if is_cancelled(cancel) {
            out.cancelled = true;
            break;
        }
        if total_start.elapsed().as_secs() > deadline_secs {
            out.errors
                .push("many-files: hit profile-timeout during upload-all".into());
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
        match provider.upload(&local_up_path, &remote, None).await {
            Ok(()) => {
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                up_ms.push(ms);
                up_mbps.push((size as f64 * 8.0) / 1_000_000.0 / (ms / 1000.0).max(1e-6));
                uploaded += 1;
                out.bytes_transferred += size;
                out.runs += 1;
                observer.event(BenchmarkEvent::BatchItem {
                    operation: "upload-all",
                });
            }
            Err(e) => {
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
    if uploaded > 0 {
        let secs = up_start.elapsed().as_secs_f64().max(1e-6);
        out.results.push(many_files_result(
            protocol,
            anonymize_extra,
            report_id,
            "upload-all",
            size,
            uploaded,
            uploaded as f64 / secs,
            &up_ms,
            Some(&up_mbps),
            up_fatal,
        ));
    }
    if uploaded == 0 {
        // Nothing landed remotely: the remaining ops have no payload to act on.
        return out;
    }

    // ── list-dir (one call, returns N entries) ──────────────────────
    if !out.cancelled {
        let list_start = Instant::now();
        match provider.list(&many_dir).await {
            Ok(entries) => {
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
            Err(e) => out
                .errors
                .push(format!("many-files: list-dir failed: {}", e)),
        }
    }

    // ── stat-all ────────────────────────────────────────────────────
    let mut stat_ms: Vec<f64> = Vec::new();
    let mut stat_fatal = 0u32;
    let stat_start = Instant::now();
    for i in 0..uploaded {
        if out.cancelled || is_cancelled(cancel) {
            out.cancelled = true;
            break;
        }
        if total_start.elapsed().as_secs() > deadline_secs {
            out.errors
                .push("many-files: hit profile-timeout during stat-all".into());
            break;
        }
        let start = Instant::now();
        match provider.stat(&remote_name(i)).await {
            Ok(_) => stat_ms.push(start.elapsed().as_secs_f64() * 1000.0),
            Err(e) => {
                stat_fatal += 1;
                out.errors
                    .push(format!("many-files: stat of file {} failed: {}", i, e));
            }
        }
    }
    if !stat_ms.is_empty() {
        let secs = stat_start.elapsed().as_secs_f64().max(1e-6);
        out.results.push(many_files_result(
            protocol,
            anonymize_extra,
            report_id,
            "stat-all",
            0,
            stat_ms.len() as u32,
            stat_ms.len() as f64 / secs,
            &stat_ms,
            None,
            stat_fatal,
        ));
        out.runs += stat_ms.len() as u32;
    }

    // ── download-all ────────────────────────────────────────────────
    let local_dn = if out.cancelled {
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
        let dn_start = Instant::now();
        observer.event(BenchmarkEvent::BatchStarted {
            operation: "download-all",
            total: uploaded as u64,
        });
        for i in 0..uploaded {
            if is_cancelled(cancel) {
                out.cancelled = true;
                break;
            }
            if total_start.elapsed().as_secs() > deadline_secs {
                out.errors
                    .push("many-files: hit profile-timeout during download-all".into());
                break;
            }
            wait_until_upload_readable(provider, &remote_name(i), observer).await;
            let start = Instant::now();
            match provider
                .download(&remote_name(i), &local_dn_path, None)
                .await
            {
                Ok(()) => {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    dn_ms.push(ms);
                    dn_mbps.push((size as f64 * 8.0) / 1_000_000.0 / (ms / 1000.0).max(1e-6));
                    out.bytes_transferred += size;
                    out.runs += 1;
                    observer.event(BenchmarkEvent::BatchItem {
                        operation: "download-all",
                    });
                }
                Err(e) => {
                    dn_fatal += 1;
                    out.errors
                        .push(format!("many-files: download of file {} failed: {}", i, e));
                }
            }
        }
        observer.event(BenchmarkEvent::BatchFinished {
            operation: "download-all",
        });
        if !dn_ms.is_empty() {
            let secs = dn_start.elapsed().as_secs_f64().max(1e-6);
            out.results.push(many_files_result(
                protocol,
                anonymize_extra,
                report_id,
                "download-all",
                size,
                dn_ms.len() as u32,
                dn_ms.len() as f64 / secs,
                &dn_ms,
                Some(&dn_mbps),
                dn_fatal,
            ));
        }
    }

    // ── delete-all (also clears the scratch files) ──────────────────
    let mut del_ms: Vec<f64> = Vec::new();
    let mut del_fatal = 0u32;
    let del_start = Instant::now();
    for i in 0..uploaded {
        let start = Instant::now();
        match provider.delete(&remote_name(i)).await {
            Ok(()) => del_ms.push(start.elapsed().as_secs_f64() * 1000.0),
            Err(e) => {
                del_fatal += 1;
                out.errors
                    .push(format!("many-files: delete of file {} failed: {}", i, e));
            }
        }
    }
    if !del_ms.is_empty() {
        let secs = del_start.elapsed().as_secs_f64().max(1e-6);
        out.results.push(many_files_result(
            protocol,
            anonymize_extra,
            report_id,
            "delete-all",
            0,
            del_ms.len() as u32,
            del_ms.len() as f64 / secs,
            &del_ms,
            None,
            del_fatal,
        ));
        out.runs += del_ms.len() as u32;
    }

    out
}
