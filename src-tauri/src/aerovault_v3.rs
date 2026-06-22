//! AeroVault v3 Tauri command layer.
//!
//! The AEROVAULT3 container format, crypto pipeline (content-defined chunks,
//! keyed BLAKE3 chunk ids, zstd-per-chunk, AES-256-GCM-SIV) and rev. 4 Reed-
//! Solomon Error Correction now live in the `aerovault` crate (`aerovault::v3`).
//! T7 app convergence: this module is the thin embedder layer over that crate.
//! It keeps only the `vault_v3_*` Tauri commands (preserving their public
//! signatures and the `VaultV3Info` JSON the frontend consumes), the
//! `ReportSink` that forwards the crate's telemetry into the app `VaultReport`,
//! and the app-specific glue the crate does not own: the per-vault write lock,
//! the OOM pre-flight guard, and path/profile helpers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::error_correction::ERROR_CORRECTION_DEFAULT_PCT;
use crate::vault_telemetry::VaultReport;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tauri::Emitter;

#[derive(Debug, Serialize)]
pub struct VaultV3Info {
    pub version: u8,
    pub file_count: usize,
    pub chunk_count: usize,
    pub dedup_chunks: usize,
    pub compression_level: i32,
    pub files: Vec<VaultV3FileInfo>,
    /// Behind-the-scenes technical receipt for the operation that produced
    /// this info (additive: `None` for plain open/listing). Serde-skipped
    /// when absent so the frontend TS interface only gains an optional field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<VaultReport>,
}

#[derive(Debug, Serialize)]
pub struct VaultV3FileInfo {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: String,
    pub chunk_count: usize,
}
fn validate_vault_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\0')
        || path.contains('\\')
        || path.split('/').any(|part| part == "..")
        || path.as_bytes().get(1) == Some(&b':')
    {
        return Err(format!("Invalid AeroVault path: {path}"));
    }
    Ok(())
}

fn safe_entry_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("Invalid file name: {}", path.display()))?
        .to_string_lossy()
        .to_string();
    validate_vault_path(&name)?;
    Ok(name)
}

fn normalize_vault_relative_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Err("Invalid AeroVault path: empty".to_string());
    }
    validate_vault_path(trimmed)?;
    if trimmed
        .split('/')
        .any(|part| part.is_empty() || part == ".")
    {
        return Err(format!("Invalid AeroVault path: {trimmed}"));
    }
    Ok(trimmed.to_string())
}

fn normalize_leaf_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.contains('\0')
    {
        return Err("Invalid AeroVault name".to_string());
    }
    Ok(trimmed.to_string())
}
fn join_vault_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn path_parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}
struct VaultWriteLock {
    path: PathBuf,
    _file: File,
}

impl Drop for VaultWriteLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn lock_path_for(vault_path: &Path) -> Result<PathBuf, String> {
    let parent = vault_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = vault_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid vault path: {}", vault_path.display()))?;
    Ok(parent.join(format!(".{name}.lock")))
}

/// True if a process with `pid` is currently running. Conservative: when the
/// answer is uncertain (e.g. the OS refuses to report) it returns `true`, so a
/// live writer's lock is never mistaken for stale and reclaimed (audit M9).
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // kill(pid, 0): 0 -> alive; EPERM -> exists but not ours (alive); ESRCH -> dead.
    let r = unsafe { libc::kill(pid as i32, 0) };
    if r == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    // Minimal kernel32 FFI (no extra `windows` crate features): open the process
    // with the least privilege and read its exit code. STILL_ACTIVE (259) means it
    // is still running. A failed open with ERROR_ACCESS_DENIED means the process
    // exists but is protected -> treat as alive (never reclaim).
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;
    type Handle = isize;
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn GetExitCodeProcess(handle: Handle, code: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetLastError() -> u32;
    }
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE
    }
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    true // conservative: unknown platform never reclaims a lock
}

/// Best-effort read of the pid recorded in `pid=<n> created_at=<t>`.
fn lock_recorded_pid(lock_path: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(lock_path).ok()?;
    content
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("pid="))
        .and_then(|v| v.parse::<u32>().ok())
}

/// A lock is stale only when its recorded owner process is provably gone (crash
/// before `Drop` ran). When the pid is unreadable (e.g. a crash truncated the
/// write) fall back to a generous age bound that no real seal approaches, so a
/// legitimate in-progress seal is never reclaimed (audit M9).
fn lock_is_stale(lock_path: &Path) -> bool {
    match lock_recorded_pid(lock_path) {
        Some(pid) => pid != std::process::id() && !process_is_alive(pid),
        None => std::fs::metadata(lock_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age > Duration::from_secs(600))
            .unwrap_or(false),
    }
}

fn acquire_vault_write_lock(vault_path: &Path) -> Result<VaultWriteLock, String> {
    let lock_path = lock_path_for(vault_path)?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Create lock dir: {e}"))?;
    }

    let started = Instant::now();
    let mut reclaimed = false;
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let _ = writeln!(
                    file,
                    "pid={} created_at={}",
                    std::process::id(),
                    chrono::Utc::now().to_rfc3339()
                );
                let _ = file.sync_all();
                return Ok(VaultWriteLock {
                    path: lock_path,
                    _file: file,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Reclaim a lock orphaned by a crashed writer (its RAII Drop never ran)
                // so a dead seal does not block the next writer ~30s then hard-error
                // until manual cleanup (audit M9). Reclaim ATOMICALLY by renaming the
                // stale lock aside rather than unconditionally removing it: an unlinked
                // path could already be a fresh lock a live writer just recreated, so a
                // bare remove can clobber a live lock (controaudit TOCTOU). rename is
                // atomic, so only one racing writer moves a given source; we then confirm
                // the moved file really was the dead-owner lock (and drop it) or, if a
                // live writer had recreated it in between, restore it and fall through to
                // the wait. create_new (O_EXCL) below is the single-writer gate and
                // assert_vault_generation_current is the final data-integrity backstop
                // for any residual race. Reclaim at most once per acquire so live
                // contention still falls through to the bounded busy-wait below.
                if !reclaimed && lock_is_stale(&lock_path) {
                    let aside = lock_path.with_file_name(format!(
                        "{}.reclaim.{}",
                        lock_path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        std::process::id()
                    ));
                    if std::fs::rename(&lock_path, &aside).is_ok() {
                        if lock_is_stale(&aside) {
                            let _ = std::fs::remove_file(&aside);
                            reclaimed = true;
                        } else {
                            // A live writer recreated the lock between our check and the
                            // rename: put it back, do not steal it. If the put-back fails
                            // (the path was recreated again in the meantime), drop our
                            // aside copy so no `.reclaim.<pid>` orphan is left behind.
                            if std::fs::rename(&aside, &lock_path).is_err() {
                                let _ = std::fs::remove_file(&aside);
                            }
                        }
                    }
                    continue;
                }
                if started.elapsed() > Duration::from_secs(30) {
                    return Err(format!(
                        "AeroVault v3 write lock is busy: {}",
                        lock_path.display()
                    ));
                }
                sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Create vault write lock: {e}")),
        }
    }
}
/// Build the handler-facing `VaultV3Info` from the crate's vault summary,
/// preserving the JSON shape the frontend consumes. T7 convergence: the crate
/// now owns the manifest, so the info is derived from `VaultV3::summary`.
fn info_from_summary(summary: &aerovault::v3::VaultSummaryV3) -> VaultV3Info {
    VaultV3Info {
        version: summary.version,
        file_count: summary.file_count,
        chunk_count: summary.chunk_count,
        dedup_chunks: summary.dedup_chunks,
        compression_level: summary.compression_level,
        files: summary
            .entries
            .iter()
            .map(|entry| VaultV3FileInfo {
                name: entry.path.clone(),
                size: entry.size,
                is_dir: entry.is_dir,
                modified: entry.modified.clone(),
                chunk_count: entry.chunk_count,
            })
            .collect(),
        report: None,
    }
}

/// zstd compression level -> the profile label shown on the technical receipt.
/// Archive maps to zstd -15 (aerovault #5 item 10: the benchmark showed -19 buys
/// only ~5-8% extra ratio for 2.5-14x the time and is pathological on
/// incompressible media); -19 is still recognised as Archive so vaults created
/// before the change keep their label.
fn level_to_profile(level: i32) -> &'static str {
    match level {
        3 => "fast",
        15 | 19 => "archive",
        _ => "balanced",
    }
}

/// Apply a `fast`/`balanced`/`archive` compression profile to crate create
/// options. `balanced`/empty/none keep the crate's default zstd level.
fn apply_profile_level(
    opts: aerovault::v3::CreateOptionsV3,
    profile: Option<&str>,
) -> Result<aerovault::v3::CreateOptionsV3, String> {
    Ok(match profile {
        Some("fast") => opts.with_zstd_level(3),
        Some("archive") => opts.with_zstd_level(15),
        Some("balanced") | None | Some("") => opts,
        Some(other) => return Err(format!("Unknown AeroVault v3 compression profile: {other}")),
    })
}

const AEROVZ_PRODUCT_EXTENSION: &str = "aerozip";

fn is_aerovz_product_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(AEROVZ_PRODUCT_EXTENSION))
        .unwrap_or(false)
}

fn ensure_aerovz_product_path(path: &str) -> Result<(), String> {
    if is_aerovz_product_path(path) {
        Ok(())
    } else {
        Err(format!("Expected a .{AEROVZ_PRODUCT_EXTENSION} archive path"))
    }
}

fn aerovz_plaintext_flag(path: &Path) -> Result<Option<bool>, String> {
    let mut file = File::open(path).map_err(|e| format!("Open archive: {e}"))?;
    let mut header_prefix = [0u8; 12];
    file.read_exact(&mut header_prefix)
        .map_err(|e| format!("Read archive header: {e}"))?;
    if &header_prefix[..10] != aerovault::v3::constants::MAGIC
        || header_prefix[10] != aerovault::v3::constants::VERSION
    {
        return Ok(None);
    }
    Ok(Some(
        header_prefix[11] & aerovault::v3::constants::FLAG_PLAINTEXT_CONTENT != 0,
    ))
}

fn open_aerovz_archive(path: &Path) -> Result<aerovault::v3::OpenVaultV3, String> {
    if matches!(aerovz_plaintext_flag(path)?, Some(false)) {
        return Err("Not a plaintext .aerozip archive (it is encrypted)".to_string());
    }
    aerovault::v3::VaultV3::open_plaintext(path)
}

/// Forwards the crate's content-pipeline telemetry into a shared [`VaultReport`].
/// Attached to an opened crate vault via `set_telemetry_sink`; the handler keeps
/// the `Arc` to read the populated report back after the blocking op. Poison is
/// recovered (a sink panic must not poison subsequent receipts).
struct ReportSink {
    report: Arc<Mutex<VaultReport>>,
    progress: Option<VaultProgressEmitter>,
}

/// Progress callback for a long vault op: `(percent 0..=100, bytes done, bytes
/// total)`. The GUI passes a closure that emits a `vault_progress` Tauri event;
/// the CLI passes one that drives an `indicatif` bar (Ehud #5: progress in both
/// GUI and CLI). Boxed `FnMut + Send` so it can cross into `spawn_blocking`.
pub type VaultProgressFn = Box<dyn FnMut(i64, u64, u64) + Send>;

/// Live progress for a long vault op: accumulates the plaintext bytes seen by
/// `on_chunk` (compression + encryption run on the same chunk stream) and
/// invokes the callback at most once per integer percent.
struct VaultProgressEmitter {
    cb: VaultProgressFn,
    total: u64,
    acc: u64,
    last_pct: i64,
}

impl ReportSink {
    fn with<R>(&self, f: impl FnOnce(&mut VaultReport) -> R) -> R {
        let mut guard = self.report.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

impl aerovault::v3::VaultTelemetrySink for ReportSink {
    fn on_chunk(&mut self, is_new: bool, plaintext: u64, compressed: u64, encrypted: u64) {
        self.with(|r| r.on_chunk(is_new, plaintext, compressed, encrypted));
        if let Some(p) = self.progress.as_mut() {
            p.acc = p.acc.saturating_add(plaintext);
            let done = p.acc.min(p.total);
            let pct = done.saturating_mul(100).checked_div(p.total).unwrap_or(0) as i64;
            if pct != p.last_pct {
                p.last_pct = pct;
                (p.cb)(pct, done, p.total);
            }
        }
    }
    fn on_file(&mut self, packed: bool) {
        self.with(|r| r.on_file(packed));
    }
    fn on_pack(&mut self) {
        self.with(|r| r.on_pack());
    }
    fn set_cdc(&mut self, min: usize, avg: usize, max: usize) {
        self.with(|r| r.set_cdc(min, avg, max));
    }
    fn set_error_correction(&mut self, shards: u64, bytes_protected: u64, overhead_pct: f64) {
        self.with(|r| r.set_error_correction_protection(shards, bytes_protected, overhead_pct));
    }
    fn step(&mut self, message: &str) {
        self.with(|r| r.step(message));
    }
}

#[tauri::command]
pub async fn vault_v3_create(
    vault_path: String,
    password: String,
    compression_profile: Option<String>,
) -> Result<String, String> {
    let opts = apply_profile_level(
        aerovault::v3::CreateOptionsV3::new(&vault_path, &password),
        compression_profile.as_deref(),
    )?;
    let vp = vault_path.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vp))?;
        aerovault::v3::VaultV3::create(&opts)
    })
    .await
    .map_err(|e| format!("vault create task failed: {e}"))??;
    log::info!("vault v3 create: '{}'", vault_basename(&vault_path));
    Ok(vault_path)
}

/// Create a new AeroVault v3 container **with Reed-Solomon Error Correction**.
///
/// `placement` selects where the parity lives: `embedded` (default; non-critical
/// in-container extension, recomputed on every seal), `detached` (a sibling
/// `.aerocorrect` sidecar, container stays byte-identical to a plain vault), or
/// `both`. The embedded extension is non-critical so existing v3 readers can still
/// open the vault and extract data (per AEROVAULT-V3-SPEC.md + discussion #276).
#[tauri::command]
pub async fn vault_v3_create_with_error_correction(
    vault_path: String,
    password: String,
    profile: Option<String>,
    placement: Option<String>,
    error_correction_pct: Option<u32>,
) -> Result<String, String> {
    let opts = apply_profile_level(
        aerovault::v3::CreateOptionsV3::new(&vault_path, &password),
        profile.as_deref(),
    )?;
    let placement = match placement.as_deref() {
        None | Some("") => aerovault::v3::RecoveryPlacement::Embedded,
        Some(p) => aerovault::v3::RecoveryPlacement::parse(p)?,
    };
    // QR-style overhead level (#276): default 20% reproduces the original K=10/P=2 grid.
    let pct = error_correction_pct.unwrap_or(ERROR_CORRECTION_DEFAULT_PCT);
    let vp = vault_path.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vp))?;
        aerovault::v3::VaultV3::create_with_error_correction(&opts, placement, pct)
    })
    .await
    .map_err(|e| format!("vault create task failed: {e}"))??;
    Ok(vault_path)
}

#[tauri::command]
pub async fn aerovz_is_archive(path: String) -> Result<bool, String> {
    Ok(matches!(
        aerovz_plaintext_flag(Path::new(&path)),
        Ok(Some(true))
    ))
}

/// Header-only container sniff for the GUI's "open by content, not extension"
/// double-click path. Reads just the magic/flag bytes (cheap regardless of file
/// size) and classifies the file as the plaintext Zip lane (`"zip"`), an
/// encrypted AeroVault container (`"vault"`, v1/v2/v3), or not a container
/// (`None`). The frontend uses this so a renamed/extensionless AeroVault file
/// still opens in the modal, which then runs its finer-grained detection.
#[tauri::command]
pub async fn detect_aero_container(path: String) -> Result<Option<String>, String> {
    let p = Path::new(&path);
    // AEROVAULT3 family: the plaintext flag distinguishes the Zip lane from an
    // encrypted v3 vault. A read error here is non-fatal (treat as "unknown").
    match aerovz_plaintext_flag(p) {
        Ok(Some(true)) => return Ok(Some("zip".to_string())),
        Ok(Some(false)) => return Ok(Some("vault".to_string())),
        Ok(None) => {}
        Err(_) => return Ok(None),
    }
    // Encrypted AeroVault v3 (defensive; the flag read above already covers it),
    // then v2/v1 via the crate's header check.
    if aerovault::v3::VaultV3::is_vault_v3(&path) || aerovault::Vault::is_vault(&path) {
        return Ok(Some("vault".to_string()));
    }
    Ok(None)
}

#[tauri::command]
pub async fn aerovz_create_archive(
    vault_path: String,
    compression_profile: Option<String>,
    error_correction_pct: Option<u32>,
) -> Result<String, String> {
    ensure_aerovz_product_path(&vault_path)?;
    let opts = apply_profile_level(
        aerovault::v3::CreateOptionsV3::new_plaintext(PathBuf::from(&vault_path)),
        compression_profile.as_deref(),
    )?;
    let pct = error_correction_pct.unwrap_or(ERROR_CORRECTION_DEFAULT_PCT);
    let vp = vault_path.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vp))?;
        aerovault::v3::VaultV3::create_with_error_correction(
            &opts,
            aerovault::v3::RecoveryPlacement::Embedded,
            pct,
        )
    })
    .await
    .map_err(|e| format!("archive create task failed: {e}"))??;
    Ok(vault_path)
}

#[tauri::command]
pub async fn aerovz_open_archive(vault_path: String) -> Result<VaultV3Info, String> {
    let summary =
        tokio::task::spawn_blocking(move || -> Result<aerovault::v3::VaultSummaryV3, String> {
            let vault = open_aerovz_archive(Path::new(&vault_path))?;
            Ok(aerovault::v3::VaultV3::summary(&vault))
        })
        .await
        .map_err(|e| format!("archive open task failed: {e}"))??;
    Ok(info_from_summary(&summary))
}

#[tauri::command]
pub async fn aerovz_recovery_status(path: String) -> Result<serde_json::Value, String> {
    let status = aerovault::v3::VaultV3::recovery_status(Path::new(&path))?;
    Ok(serde_json::json!({
        "embedded": status.embedded,
        "detached": status.detached,
        "header_parity": status.header_parity,
        "manifest_parity": status.manifest_parity,
    }))
}

#[tauri::command]
pub async fn aerovz_add_files(
    vault_path: String,
    file_paths: Vec<String>,
    app: tauri::AppHandle,
) -> Result<VaultV3Info, String> {
    let cb: VaultProgressFn = Box::new(move |pct, done, total| {
        let _ = app.emit(
            "vault_progress",
            serde_json::json!({ "percentage": pct, "transferred": done, "total": total }),
        );
    });
    aerovz_add_files_with_progress(vault_path, file_paths, Some(cb)).await
}

async fn aerovz_add_files_with_progress(
    vault_path: String,
    file_paths: Vec<String>,
    progress: Option<VaultProgressFn>,
) -> Result<VaultV3Info, String> {
    let started = Instant::now();
    let mut sources: Vec<(PathBuf, String)> = Vec::with_capacity(file_paths.len());
    for file_path in &file_paths {
        let path = PathBuf::from(file_path);
        if !path.is_file() {
            return Err(format!("Not a regular file: {file_path}"));
        }
        let name = safe_entry_name(&path)?;
        sources.push((path, name));
    }
    let total_bytes: u64 = sources
        .iter()
        .map(|(p, _)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();
    preflight_vault_memory_guard(Path::new(&vault_path), &sources)?;

    let report = Arc::new(Mutex::new(VaultReport::new("archive_add_files", 3)));
    let report_for_task = report.clone();
    let summary =
        tokio::task::spawn_blocking(move || -> Result<aerovault::v3::VaultSummaryV3, String> {
            let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
            let mut vault = open_aerovz_archive(Path::new(&vault_path))?;
            let pre = aerovault::v3::VaultV3::summary(&vault);
            {
                let mut r = report_for_task.lock().unwrap_or_else(|e| e.into_inner());
                r.set_profile(level_to_profile(pre.compression_level));
                r.set_algorithms(pre.algorithms.clone());
            }
            vault.set_telemetry_sink(Box::new(ReportSink {
                report: report_for_task.clone(),
                progress: progress.map(|cb| VaultProgressEmitter {
                    cb,
                    total: total_bytes,
                    acc: 0,
                    last_pct: -1,
                }),
            }));
            aerovault::v3::VaultV3::add_files(&mut vault, &sources)?;
            Ok(aerovault::v3::VaultV3::summary(&vault))
        })
        .await
        .map_err(|e| format!("archive add task failed: {e}"))??;

    let mut info = info_from_summary(&summary);
    {
        let mut r = report.lock().unwrap_or_else(|e| e.into_inner());
        r.step("seal: rebuild plaintext manifest + atomic write");
        r.finish(started.elapsed().as_millis() as u64);
        let (np, dh, ratio) = (r.new_physical_chunks, r.dedup_hits, r.compression_ratio_pct);
        r.step(format!(
            "done: {np} new physical chunk(s), {dh} dedup hit(s), {ratio:.1}% compressed"
        ));
        info.report = Some(r.clone());
    }
    Ok(info)
}

#[tauri::command]
pub async fn aerovz_add_files_to_dir(
    vault_path: String,
    file_paths: Vec<String>,
    target_dir: String,
) -> Result<serde_json::Value, String> {
    let paths: Vec<PathBuf> = file_paths.iter().map(PathBuf::from).collect();
    let guard_sources: Vec<(PathBuf, String)> =
        paths.iter().map(|p| (p.clone(), String::new())).collect();
    preflight_vault_memory_guard(Path::new(&vault_path), &guard_sources)?;
    let added = paths.len();
    let total = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = open_aerovz_archive(Path::new(&vault_path))?;
        aerovault::v3::VaultV3::add_files_to_dir(&mut vault, &paths, &target_dir)?;
        Ok(aerovault::v3::VaultV3::summary(&vault).entries.len())
    })
    .await
    .map_err(|e| format!("archive add task failed: {e}"))??;
    Ok(serde_json::json!({ "added": added, "total": total }))
}

#[tauri::command]
pub async fn aerovz_add_directory(
    app: tauri::AppHandle,
    vault_path: String,
    source_dir: String,
    target_prefix: Option<String>,
) -> Result<serde_json::Value, String> {
    let (added_files, added_dirs) =
        tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
            let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
            let mut vault = open_aerovz_archive(Path::new(&vault_path))?;
            aerovault::v3::VaultV3::add_directory(
                &mut vault,
                Path::new(&source_dir),
                target_prefix.as_deref(),
            )
        })
        .await
        .map_err(|e| format!("archive add directory task failed: {e}"))??;

    let _ = app.emit(
        "vault-add-progress",
        serde_json::json!({
            "current": added_files,
            "total": added_files,
            "current_file": ""
        }),
    );
    Ok(serde_json::json!({
        "added_files": added_files,
        "added_dirs": added_dirs,
        "total_entries": added_files + added_dirs
    }))
}

#[tauri::command]
pub async fn aerovz_create_directory(
    vault_path: String,
    dir_name: String,
) -> Result<serde_json::Value, String> {
    let dir = normalize_vault_relative_path(&dir_name)?;
    let created = tokio::task::spawn_blocking(move || -> Result<bool, String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = open_aerovz_archive(Path::new(&vault_path))?;
        aerovault::v3::VaultV3::create_directory(&mut vault, &dir_name)
    })
    .await
    .map_err(|e| format!("archive mkdir task failed: {e}"))??;
    Ok(serde_json::json!({ "created": created, "dir": dir }))
}

#[tauri::command]
pub async fn aerovz_delete_entry(
    vault_path: String,
    entry_name: String,
) -> Result<serde_json::Value, String> {
    let deleted = normalize_vault_relative_path(&entry_name)?;
    let remaining = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = open_aerovz_archive(Path::new(&vault_path))?;
        aerovault::v3::VaultV3::delete_entry(&mut vault, &entry_name)?;
        Ok(aerovault::v3::VaultV3::summary(&vault).entries.len())
    })
    .await
    .map_err(|e| format!("archive delete task failed: {e}"))??;
    Ok(serde_json::json!({ "deleted": deleted, "remaining": remaining }))
}

#[tauri::command]
pub async fn aerovz_delete_entries(
    vault_path: String,
    entry_names: Vec<String>,
    recursive: bool,
) -> Result<serde_json::Value, String> {
    let (removed, remaining) =
        tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
            let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
            let mut vault = open_aerovz_archive(Path::new(&vault_path))?;
            let removed =
                aerovault::v3::VaultV3::delete_entries(&mut vault, &entry_names, recursive)?;
            Ok((
                removed,
                aerovault::v3::VaultV3::summary(&vault).entries.len(),
            ))
        })
        .await
        .map_err(|e| format!("archive delete task failed: {e}"))??;
    Ok(serde_json::json!({ "removed": removed, "remaining": remaining }))
}

#[tauri::command]
pub async fn aerovz_extract_entry(
    vault_path: String,
    entry_name: String,
    dest_path: String,
) -> Result<String, String> {
    let extracted = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        let vault = open_aerovz_archive(Path::new(&vault_path))?;
        aerovault::v3::VaultV3::extract_entry(&vault, &entry_name, Path::new(&dest_path))
    })
    .await
    .map_err(|e| format!("archive extract task failed: {e}"))??;
    Ok(extracted.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn aerovz_extract_all(vault_path: String, dest_path: String) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || -> Result<u64, String> {
        let vault = open_aerovz_archive(Path::new(&vault_path))?;
        aerovault::v3::VaultV3::extract_all(&vault, Path::new(&dest_path))
    })
    .await
    .map_err(|e| format!("archive extract task failed: {e}"))?
}

#[tauri::command]
pub async fn aerovz_scrub(vault_path: String) -> Result<serde_json::Value, String> {
    let (checked, list, parity_source) = tokio::task::spawn_blocking(
        move || -> Result<(usize, Vec<serde_json::Value>, &'static str), String> {
            let vault = open_aerovz_archive(Path::new(&vault_path))?;
            let checked = aerovault::v3::VaultV3::summary(&vault).chunk_count;
            let damaged = aerovault::v3::VaultV3::scrub(&vault);
            let parity_source =
                match aerovault::v3::VaultV3::resolve_parity_source(&vault, None) {
                    Ok(s) => s,
                    Err(_) => aerovault::v3::ParitySource::None,
                };
            let list: Vec<_> = damaged
                .into_iter()
                .map(|d| {
                    serde_json::json!({
                        "id": d.record.id,
                        "on_disk_start": d.on_disk_start,
                        "on_disk_len": d.on_disk_len,
                        "cipher_hash": d.record.cipher_hash,
                    })
                })
                .collect();
            Ok((checked, list, parity_source.as_str()))
        },
    )
    .await
    .map_err(|e| format!("archive scrub task failed: {e}"))??;
    let count = list.len();
    Ok(serde_json::json!({
        "damaged": list,
        "count": count,
        "checked": checked,
        "parity_source": parity_source,
    }))
}

#[tauri::command]
pub async fn aerovz_repair(vault_path: String, dry_run: bool) -> Result<serde_json::Value, String> {
    let (repaired, damaged, source) =
        tokio::task::spawn_blocking(move || -> Result<(usize, usize, &'static str), String> {
            let _lock = if dry_run {
                None
            } else {
                Some(acquire_vault_write_lock(Path::new(&vault_path))?)
            };
            let mut vault = open_aerovz_archive(Path::new(&vault_path))?;
            let damaged = aerovault::v3::VaultV3::scrub(&vault).len();
            let (repaired, source) = aerovault::v3::VaultV3::repair(&mut vault, dry_run, None)?;
            Ok((repaired, damaged, source.as_str()))
        })
        .await
        .map_err(|e| format!("archive repair task failed: {e}"))??;
    Ok(serde_json::json!({
        "repaired": repaired,
        "damaged": damaged,
        "dry_run": dry_run,
        "parity_source": source,
    }))
}

/// The target "mode" for a vault repack (Ehud #2: a "Change Mode" action next to
/// Change Password on the open vault). It carries the security level (a v2 <-> v3
/// format move), the compression profile and the Error-Correction choice, so the
/// repack engine reads a struct rather than a fixed argument list.
struct RepackTarget {
    /// Target security level for the full v2 <-> v3 Change Mode, mapping to a
    /// container format: "standard"/"advanced" -> v2 (non-cascade), "paranoid" ->
    /// v2 cascade, "experimental"/"archive" -> v3. `None` keeps the source format
    /// (the v3 -> v3 step-1 behaviour: only the profile / EC change).
    security_level: Option<String>,
    compression_profile: Option<String>,
    error_correction: bool,
    error_correction_pct: u32,
}

/// Resolve a target security level to `(is_v3, cascade)`. The v2 format carries a
/// cascade axis (AES + ChaCha20 double encryption = "paranoid"); v3 has none. When
/// `level` is `None` (or empty) the source format is kept, preserving a v2 source's
/// cascade flag, so a profile/EC-only change never moves the container format.
fn resolve_target_format(
    level: Option<&str>,
    source_is_v3: bool,
    source_cascade: bool,
) -> Result<(bool, bool), String> {
    Ok(match level {
        None | Some("") => (source_is_v3, source_cascade),
        Some("standard") | Some("advanced") => (false, false),
        Some("paranoid") => (false, true),
        Some("experimental") | Some("archive") => (true, false),
        Some(other) => return Err(format!("Unknown vault security level: {other}")),
    })
}

/// Extract every entry of the v2 vault at `vault_path` into `dest`, rebuilding the
/// directory tree. Thin path-based wrapper over the shared
/// [`crate::aerovault_v2::extract_all_v2_tree`] so the repack and the GUI's
/// `vault_v2_extract_all` use one tree-preserving extractor (the crate's own v2
/// extract flattens nested files into the destination root).
fn extract_all_v2_preserving_tree(
    vault_path: &Path,
    password: &str,
    dest: &Path,
) -> Result<(), String> {
    let vault = aerovault::Vault::open(vault_path, password).map_err(|e| e.to_string())?;
    crate::aerovault_v2::extract_all_v2_tree(&vault, dest)?;
    Ok(())
}

/// Re-add an extracted plaintext tree at `root` into the freshly created v2 vault at
/// `vault_path`, preserving the directory structure. The crate's v2 `Vault` has no
/// recursive `add_directory` (unlike `VaultV3`), so this mirrors
/// `vault_v2_add_directory`'s two-pass shape: create every directory parent-first
/// (by depth), then add files grouped by parent directory. Each v2 op reads and
/// rewrites the whole container, so the vault is re-opened per phase/batch (matching
/// the GUI command) rather than reusing one stale handle. Returns
/// `(added_files, added_dirs)`.
fn add_directory_v2(
    vault_path: &Path,
    password: &str,
    root: &Path,
) -> Result<(usize, usize), String> {
    struct Item {
        rel: String,
        abs: PathBuf,
        is_dir: bool,
        depth: usize,
    }
    let mut items: Vec<Item> = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path() == root {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| "compute relative path".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        items.push(Item {
            rel,
            abs: entry.path().to_path_buf(),
            is_dir: entry.file_type().is_dir(),
            depth: entry.depth(),
        });
    }

    // 1) Create every directory, parents before children.
    let mut dirs: Vec<&Item> = items.iter().filter(|i| i.is_dir).collect();
    dirs.sort_by_key(|d| d.depth);
    let mut added_dirs = 0usize;
    if !dirs.is_empty() {
        let vault = aerovault::Vault::open(vault_path, password).map_err(|e| e.to_string())?;
        for d in &dirs {
            match vault.create_directory(&d.rel) {
                Ok(_) => added_dirs += 1,
                Err(e) => {
                    let s = e.to_string();
                    if !s.contains("already exists") {
                        return Err(format!("Create directory '{}': {s}", d.rel));
                    }
                    added_dirs += 1;
                }
            }
        }
    }

    // 2) Add files grouped by their parent directory (which now exists).
    let mut by_dir: std::collections::BTreeMap<String, Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    for f in items.iter().filter(|i| !i.is_dir) {
        let parent = Path::new(&f.rel)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        by_dir.entry(parent).or_default().push(f.abs.clone());
    }
    let mut added_files = 0usize;
    for (dir_key, paths) in &by_dir {
        let vault = aerovault::Vault::open(vault_path, password).map_err(|e| e.to_string())?;
        let n = if dir_key.is_empty() {
            vault.add_files(paths).map_err(|e| e.to_string())?
        } else {
            vault
                .add_files_to_dir(paths, dir_key)
                .map_err(|e| e.to_string())?
        };
        added_files += n as usize;
    }
    Ok((added_files, added_dirs))
}

/// Re-pack `orig` under a new `target` mode, keeping the same `password`. The crate
/// has no in-place reseal, so changing the mode is a repack: open (decrypt) the
/// vault, extract every entry into a scratch dir on the same filesystem, create a
/// fresh container with the target options, re-add the content preserving the tree,
/// then swap it over the original (original moved aside first, restored on any
/// failure). The plaintext scratch dir auto-scrubs on every path (TempDir drop),
/// success or error, mirroring the audit M1/M2 temp-hygiene rule.
fn repack_vault_blocking(
    orig: &Path,
    password: &str,
    target: &RepackTarget,
) -> Result<serde_json::Value, String> {
    let _lock = acquire_vault_write_lock(orig)?;
    let parent = orig
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = orig
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vault".to_string());

    // 0) Detect the source format and resolve the target format. The repack is the
    //    only re-encrypt path that can move a vault between v2 and v3, so both ends
    //    are format-aware: a v2 source's cascade flag is preserved unless a target
    //    security level overrides it. Error Correction lives only in v3 (Reed-Solomon
    //    parity in-container), so it is dropped when the target is v2.
    let source_is_v3 = aerovault::v3::VaultV3::is_vault_v3(orig);
    let source_cascade = if source_is_v3 {
        false
    } else {
        aerovault::Vault::peek(orig)
            .map(|p| p.mode == aerovault::EncryptionMode::Cascade)
            .unwrap_or(false)
    };
    let (target_v3, target_cascade) = resolve_target_format(
        target.security_level.as_deref(),
        source_is_v3,
        source_cascade,
    )?;
    let effective_ec = target.error_correction && target_v3;

    // 1) Extract every entry to a scratch dir on the SAME filesystem (so the later
    //    swap is a same-device rename). TempDir scrubs the plaintext on drop. The
    //    extract is format-agnostic: open the source in its own format.
    let scratch = tempfile::Builder::new()
        .prefix(".aerovault-repack-")
        .tempdir_in(parent)
        .map_err(|e| format!("Create repack scratch dir: {e}"))?;
    if source_is_v3 {
        let vault = aerovault::v3::VaultV3::open(orig, password)?;
        aerovault::v3::VaultV3::extract_all(&vault, scratch.path())?;
    } else {
        extract_all_v2_preserving_tree(orig, password, scratch.path())?;
    } // the source vault handle is dropped here, before we rewrite the path

    // 2) Create a fresh container at a temp path in the same dir, under the target
    //    format, and 3) re-add the extracted content preserving the tree.
    let tmp_container = parent.join(format!(".{name}.repack-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp_container); // clear any stale leftover
    let readd: Result<(usize, usize), String> = if target_v3 {
        let opts = apply_profile_level(
            aerovault::v3::CreateOptionsV3::new(&tmp_container, password),
            target.compression_profile.as_deref(),
        )?;
        let create_res = if effective_ec {
            aerovault::v3::VaultV3::create_with_error_correction(
                &opts,
                aerovault::v3::RecoveryPlacement::Embedded,
                target.error_correction_pct,
            )
        } else {
            aerovault::v3::VaultV3::create(&opts)
        };
        if let Err(e) = create_res {
            let _ = std::fs::remove_file(&tmp_container);
            return Err(format!("Create repacked vault: {e}"));
        }
        (|| -> Result<(usize, usize), String> {
            let mut new_vault = aerovault::v3::VaultV3::open(&tmp_container, password)?;
            aerovault::v3::VaultV3::add_directory(&mut new_vault, scratch.path(), None)
        })()
    } else {
        let mode = if target_cascade {
            aerovault::EncryptionMode::Cascade
        } else {
            aerovault::EncryptionMode::Standard
        };
        let opts = aerovault::CreateOptions::new(&tmp_container, password).with_mode(mode);
        match aerovault::Vault::create(opts) {
            Ok(_) => add_directory_v2(&tmp_container, password, scratch.path()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_container);
                return Err(format!("Create repacked vault: {e}"));
            }
        }
    };
    let (added_files, added_dirs) = match readd {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_container);
            return Err(format!("Re-add into repacked vault: {e}"));
        }
    };

    // 4) Swap the new container over the original: move the original aside, move the
    //    new one into place, restore the original on failure. Cross-platform (a bare
    //    rename onto an existing path fails on Windows).
    let bak = parent.join(format!(".{name}.repack-bak-{}", std::process::id()));
    let _ = std::fs::remove_file(&bak);
    if let Err(e) = std::fs::rename(orig, &bak) {
        let _ = std::fs::remove_file(&tmp_container);
        return Err(format!("Stage original vault aside: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp_container, orig) {
        let _ = std::fs::rename(&bak, orig); // restore the original
        return Err(format!("Swap repacked vault into place: {e}"));
    }
    let _ = std::fs::remove_file(&bak);

    // A detached `.aerocorrect` sidecar from before the repack can no longer validate
    // the rewritten container, so a later verify would mis-report. Drop the stale
    // sibling (best effort); embedded parity, when requested, lives in-container.
    let sidecar = parent.join(format!("{name}.{}", aerovault::AEROCORRECT_EXTENSION));
    let _ = std::fs::remove_file(&sidecar);

    Ok(serde_json::json!({
        "repacked": true,
        "files": added_files,
        "dirs": added_dirs,
        "format": if target_v3 { 3 } else { 2 },
        "security_level": target.security_level,
        "cascade": target_cascade,
        "compression_profile": if target_v3 { target.compression_profile.clone() } else { None },
        "error_correction": effective_ec,
    }))
}

/// Basename of a vault path, for activity logging. We never log the full path so the
/// user's home directory is not written into the on-disk log file.
pub(crate) fn vault_basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Change the "mode" of an existing vault (Ehud #2): re-pack it under a new security
/// level (v2 <-> v3 / cascade), compression profile and/or Error-Correction setting,
/// keeping the same password. `target_security_level` (`None` keeps the current
/// format) drives the container move; the repack engine ([`repack_vault_blocking`])
/// extracts the source in its own format and recreates it in the target format.
#[tauri::command]
pub async fn vault_v3_change_mode(
    vault_path: String,
    password: String,
    target_security_level: Option<String>,
    compression_profile: Option<String>,
    error_correction: bool,
    error_correction_pct: Option<u32>,
) -> Result<serde_json::Value, String> {
    let pct = error_correction_pct.unwrap_or(ERROR_CORRECTION_DEFAULT_PCT);
    // Activity logging (surfaces in the Debug panel "Logs" tab via tauri-plugin-log).
    // Log the basename only, never the full path, to avoid home-dir leakage on disk.
    let vault_label = vault_basename(&vault_path);
    let level_label = target_security_level
        .clone()
        .unwrap_or_else(|| "(unchanged)".to_string());
    log::info!("vault change-mode: '{vault_label}' -> level={level_label} ec={error_correction}");
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let target = RepackTarget {
            security_level: target_security_level,
            compression_profile,
            error_correction,
            error_correction_pct: pct,
        };
        repack_vault_blocking(Path::new(&vault_path), &password, &target)
    })
    .await
    .map_err(|e| format!("vault change-mode task failed: {e}"))?;
    match &result {
        Ok(_) => log::info!("vault change-mode: '{vault_label}' done"),
        Err(e) => log::error!("vault change-mode: '{vault_label}' failed: {e}"),
    }
    result
}

/// Export a detached `.aerocorrect` recovery file for an existing vault. This is
/// the "add Error Correction later" path: the encrypted container is read but never
/// rewritten. Pass `out_path` to override the default `<vault>.aerocorrect`.
#[tauri::command]
pub async fn vault_v3_export_parity(
    vault_path: String,
    password: String,
    out_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let res = tokio::task::spawn_blocking(move || {
        let out = out_path.as_deref().map(Path::new);
        aerovault::v3::VaultV3::export_parity(Path::new(&vault_path), &password, out)
    })
    .await
    .map_err(|e| format!("export parity task failed: {e}"))??;
    Ok(serde_json::json!({
        "path": res.path.to_string_lossy(),
        "shards": res.shards,
        "bytes_protected": res.bytes_protected,
        "overhead_pct": res.overhead_pct,
        "payload_len": res.payload_len,
        "file_len": res.file_len,
        "header_parity_len": res.header_parity_len,
        "manifest_parity_len": res.manifest_parity_len,
    }))
}

/// Drop the embedded Error Correction extension from a vault on the next seal.
/// Refuses unless a detached sidecar exists or `force` is set, so a vault is never
/// silently left with zero recovery.
#[tauri::command]
pub async fn vault_v3_strip_parity(
    vault_path: String,
    password: String,
    force: bool,
) -> Result<serde_json::Value, String> {
    let res = tokio::task::spawn_blocking(move || {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        aerovault::v3::VaultV3::strip_parity(Path::new(&vault_path), &password, force)
    })
    .await
    .map_err(|e| format!("strip parity task failed: {e}"))??;
    Ok(serde_json::json!({
        "stripped": true,
        "sidecar_present": res.sidecar_present,
        "sidecar_path": res.sidecar_path.to_string_lossy(),
    }))
}

/// Report the Error Correction recovery surfaces available for a vault without the
/// password: `embedded` (in-container extension present) and `detached` (a sibling
/// `.aerocorrect` sidecar exists). Used by the GUI to show the badge and enable
/// scrub/repair when either source is present.
#[tauri::command]
pub async fn vault_v3_recovery_status(path: String) -> Result<serde_json::Value, String> {
    // Header-only read: embedded extension presence + detached sidecar surfaces
    // (the sidecar additionally reports the GAP-4 metadata regions it protects:
    // locator + 1024-byte header), so the GUI can show detached recovery covers
    // the header, not just the data blocks.
    let status = aerovault::v3::VaultV3::recovery_status(Path::new(&path))?;
    Ok(serde_json::json!({
        "embedded": status.embedded,
        "detached": status.detached,
        "any": status.any,
        "manifest_parity": status.manifest_parity,
        "header_parity": status.header_parity,
    }))
}

#[tauri::command]
pub async fn vault_v3_open(vault_path: String, password: String) -> Result<VaultV3Info, String> {
    let summary =
        tokio::task::spawn_blocking(move || -> Result<aerovault::v3::VaultSummaryV3, String> {
            let vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
            Ok(aerovault::v3::VaultV3::summary(&vault))
        })
        .await
        .map_err(|e| format!("vault open task failed: {e}"))??;
    Ok(info_from_summary(&summary))
}

#[tauri::command]
pub async fn is_vault_v3(path: String) -> Result<bool, String> {
    Ok(aerovault::v3::VaultV3::is_vault_v3(&path))
}

/// Lightweight check for the presence of the Error Correction (error-correction) extension.
/// Does **not** require the vault password: it only reads the header and the
/// plaintext extension directory. This is safe for `vault info` / pre-flight
/// use cases and matches the "has_error_correction_extension" need from the plan (P1-05).
///
/// Returns true if a non-critical (or any) "error-correction.reed-solomon" entry is present
/// in the extension directory.
#[tauri::command]
pub async fn vault_v3_has_error_correction(path: String) -> Result<bool, String> {
    aerovault::v3::VaultV3::has_error_correction(Path::new(&path))
}

#[tauri::command]
pub async fn vault_v3_scrub(
    vault_path: String,
    password: String,
    parity_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let (checked, list, parity_source) = tokio::task::spawn_blocking(
        move || -> Result<(usize, Vec<serde_json::Value>, &'static str), String> {
            let vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
            let checked = aerovault::v3::VaultV3::summary(&vault).chunk_count;
            let damaged = aerovault::v3::VaultV3::scrub(&vault);
            // Pre-flight: report which parity source a repair would draw from. An
            // explicitly named source that fails is a hard error; an absent default
            // source reports "none".
            let explicit = parity_path.as_deref().map(Path::new);
            let parity_source =
                match aerovault::v3::VaultV3::resolve_parity_source(&vault, explicit) {
                    Ok(s) => s,
                    Err(e) => {
                        if explicit.is_some() {
                            return Err(e);
                        }
                        aerovault::v3::ParitySource::None
                    }
                };
            let list: Vec<_> = damaged
                .into_iter()
                .map(|d| {
                    serde_json::json!({
                        "id": d.record.id,
                        "on_disk_start": d.on_disk_start,
                        "on_disk_len": d.on_disk_len,
                        "cipher_hash": d.record.cipher_hash,
                    })
                })
                .collect();
            Ok((checked, list, parity_source.as_str()))
        },
    )
    .await
    .map_err(|e| format!("vault scrub task failed: {e}"))??;
    let count = list.len();
    Ok(serde_json::json!({
        "damaged": list,
        "count": count,
        "checked": checked,
        "parity_source": parity_source,
    }))
}

#[tauri::command]
pub async fn vault_v3_repair(
    vault_path: String,
    password: String,
    dry_run: bool,
    parity_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let (repaired, damaged, source) =
        tokio::task::spawn_blocking(move || -> Result<(usize, usize, &'static str), String> {
            // A real repair mutates and atomically re-seals the vault, so take the
            // same write lock the other mutating ops use to keep a concurrent
            // add/delete from racing the rewrite. Dry-run is read-only, no lock.
            let _lock = if dry_run {
                None
            } else {
                Some(acquire_vault_write_lock(Path::new(&vault_path))?)
            };
            let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
            let damaged = aerovault::v3::VaultV3::scrub(&vault).len();
            let explicit = parity_path.as_deref().map(Path::new);
            let (repaired, source) = aerovault::v3::VaultV3::repair(&mut vault, dry_run, explicit)?;
            Ok((repaired, damaged, source.as_str()))
        })
        .await
        .map_err(|e| format!("vault repair task failed: {e}"))??;
    Ok(serde_json::json!({
        "repaired": repaired,
        "damaged": damaged,
        "dry_run": dry_run,
        "parity_source": source,
    }))
}

/// Peak in-memory multiplier over the projected container size. While sealing,
/// AeroVault v3 holds the data plus working copies (plaintext, compressed,
/// encrypted), so the real peak is a few times the stored bytes.
const VAULT_MEMORY_PEAK_MULTIPLIER: u64 = 3;
/// Memory budget used when available RAM cannot be read (a conservative floor).
const VAULT_MEMORY_FIXED_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// Human-readable byte size for user-facing guard messages.
fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Best-effort available physical memory in bytes. Linux reads
/// `/proc/meminfo MemAvailable`; other platforms return `None` and the caller
/// falls back to a fixed budget.
fn available_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb.saturating_mul(1024));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Ehud #2: AeroVault v3 buffers the whole container in memory during create/add,
/// so a multi-GB input OOMs (real streaming I/O is the proper fix, tracked as
/// debt). Reject up front, with a clear message instead of a crash, when the
/// projected peak in-memory size would blow the budget. `existing` is taken from
/// the vault file on disk; `sources` are the files about to be added.
fn preflight_vault_memory_guard(
    vault_path: &Path,
    sources: &[(PathBuf, String)],
) -> Result<(), String> {
    let existing = std::fs::metadata(vault_path).map(|m| m.len()).unwrap_or(0);
    let mut added: u64 = 0;
    for (path, _) in sources {
        if let Ok(meta) = std::fs::metadata(path) {
            added = added.saturating_add(meta.len());
        }
    }
    let projected = existing.saturating_add(added);
    let peak = projected.saturating_mul(VAULT_MEMORY_PEAK_MULTIPLIER);
    // 60% of available memory, or the fixed floor when RAM cannot be read.
    let budget = available_memory_bytes()
        .map(|avail| avail / 5 * 3)
        .unwrap_or(VAULT_MEMORY_FIXED_BUDGET);
    if peak > budget {
        return Err(format!(
            "Adding these files needs about {} of memory (vault {} + {} new, peaking near {}x while sealing), above the safe limit of {}. AeroVault v3 buffers the whole container in memory, so add fewer or smaller files, or split them across vaults. Streaming large vaults is planned.",
            human_size(peak),
            human_size(existing),
            human_size(added),
            VAULT_MEMORY_PEAK_MULTIPLIER,
            human_size(budget),
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn vault_v3_add_files(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    app: tauri::AppHandle,
) -> Result<VaultV3Info, String> {
    vault_v3_add_files_inner(vault_path, password, file_paths, Some(app)).await
}

/// Shared body for the public command (which forwards the injected `AppHandle`
/// so the GUI gets live progress) and internal callers like the AeroCrypt
/// overlay path (which pass `None`, no progress channel). The `AppHandle` is
/// adapted into the generic progress callback consumed by
/// [`vault_v3_add_files_with_progress`].
pub async fn vault_v3_add_files_inner(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    app: Option<tauri::AppHandle>,
) -> Result<VaultV3Info, String> {
    let progress: Option<VaultProgressFn> = app.map(|app| {
        let cb: VaultProgressFn = Box::new(move |pct, done, total| {
            let _ = app.emit(
                "vault_progress",
                serde_json::json!({ "percentage": pct, "transferred": done, "total": total }),
            );
        });
        cb
    });
    vault_v3_add_files_with_progress(vault_path, password, file_paths, progress).await
}

/// Add files to a v3 vault with an optional progress callback. The CLI passes an
/// `indicatif`-driven closure here directly (Ehud #5: CLI progress for the long
/// compress+encrypt pass); the GUI reaches it through
/// [`vault_v3_add_files_inner`] with an event-emitting closure.
pub async fn vault_v3_add_files_with_progress(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    progress: Option<VaultProgressFn>,
) -> Result<VaultV3Info, String> {
    let started = Instant::now();
    let mut sources: Vec<(PathBuf, String)> = Vec::with_capacity(file_paths.len());
    for file_path in &file_paths {
        let path = PathBuf::from(file_path);
        if !path.is_file() {
            return Err(format!("Not a regular file: {file_path}"));
        }
        let name = safe_entry_name(&path)?;
        sources.push((path, name));
    }
    let total_bytes: u64 = sources
        .iter()
        .map(|(p, _)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();
    preflight_vault_memory_guard(Path::new(&vault_path), &sources)?;
    let report = Arc::new(Mutex::new(VaultReport::new("add_files", 3)));
    let report_for_task = report.clone();
    let summary =
        tokio::task::spawn_blocking(move || -> Result<aerovault::v3::VaultSummaryV3, String> {
            let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
            let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
            // Receipt header: profile + algorithm chain of the vault being added to.
            let pre = aerovault::v3::VaultV3::summary(&vault);
            {
                let mut r = report_for_task.lock().unwrap_or_else(|e| e.into_inner());
                r.set_profile(level_to_profile(pre.compression_level));
                r.set_algorithms(pre.algorithms.clone());
            }
            // The crate emits scan/pack/chunk/file/cdc + Error-Correction events.
            vault.set_telemetry_sink(Box::new(ReportSink {
                report: report_for_task.clone(),
                progress: progress.map(|cb| VaultProgressEmitter {
                    cb,
                    total: total_bytes,
                    acc: 0,
                    last_pct: -1,
                }),
            }));
            aerovault::v3::VaultV3::add_files(&mut vault, &sources)?;
            Ok(aerovault::v3::VaultV3::summary(&vault))
        })
        .await
        .map_err(|e| format!("vault add task failed: {e}"))??;

    let mut info = info_from_summary(&summary);
    // Handler-level receipt events: seal step, total timing, done summary.
    {
        let mut r = report.lock().unwrap_or_else(|e| e.into_inner());
        r.step("seal: rebuild manifest + atomic write");
        r.finish(started.elapsed().as_millis() as u64);
        let (np, dh, ratio) = (r.new_physical_chunks, r.dedup_hits, r.compression_ratio_pct);
        r.step(format!(
            "done: {np} new physical chunk(s), {dh} dedup hit(s), {ratio:.1}% compressed"
        ));
        info.report = Some(r.clone());
    }
    Ok(info)
}

#[tauri::command]
pub async fn vault_v3_add_files_to_dir(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    target_dir: String,
) -> Result<serde_json::Value, String> {
    let paths: Vec<PathBuf> = file_paths.iter().map(PathBuf::from).collect();
    // The guard only reads each source's on-disk size; the entry name is unused.
    let guard_sources: Vec<(PathBuf, String)> =
        paths.iter().map(|p| (p.clone(), String::new())).collect();
    preflight_vault_memory_guard(Path::new(&vault_path), &guard_sources)?;
    let added = paths.len();
    let total = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::add_files_to_dir(&mut vault, &paths, &target_dir)?;
        Ok(aerovault::v3::VaultV3::summary(&vault).entries.len())
    })
    .await
    .map_err(|e| format!("vault add task failed: {e}"))??;
    Ok(serde_json::json!({
        "added": added,
        "total": total
    }))
}

#[tauri::command]
pub async fn vault_v3_create_directory(
    vault_path: String,
    password: String,
    dir_name: String,
) -> Result<serde_json::Value, String> {
    let dir = normalize_vault_relative_path(&dir_name)?;
    let created = tokio::task::spawn_blocking(move || -> Result<bool, String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::create_directory(&mut vault, &dir_name)
    })
    .await
    .map_err(|e| format!("vault mkdir task failed: {e}"))??;
    Ok(serde_json::json!({
        "created": created,
        "dir": dir
    }))
}

#[tauri::command]
pub async fn vault_v3_delete_entry(
    vault_path: String,
    password: String,
    entry_name: String,
) -> Result<serde_json::Value, String> {
    let deleted = normalize_vault_relative_path(&entry_name)?;
    let remaining = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::delete_entry(&mut vault, &entry_name)?;
        Ok(aerovault::v3::VaultV3::summary(&vault).entries.len())
    })
    .await
    .map_err(|e| format!("vault delete task failed: {e}"))??;
    Ok(serde_json::json!({
        "deleted": deleted,
        "remaining": remaining
    }))
}

#[tauri::command]
pub async fn vault_v3_delete_entries(
    vault_path: String,
    password: String,
    entry_names: Vec<String>,
    recursive: bool,
) -> Result<serde_json::Value, String> {
    let (removed, remaining) =
        tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
            let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
            let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
            let removed =
                aerovault::v3::VaultV3::delete_entries(&mut vault, &entry_names, recursive)?;
            Ok((
                removed,
                aerovault::v3::VaultV3::summary(&vault).entries.len(),
            ))
        })
        .await
        .map_err(|e| format!("vault delete task failed: {e}"))??;
    Ok(serde_json::json!({
        "removed": removed,
        "remaining": remaining
    }))
}

#[tauri::command]
pub async fn vault_v3_move_entry(
    vault_path: String,
    password: String,
    from: String,
    to: String,
) -> Result<serde_json::Value, String> {
    let from_n = normalize_vault_relative_path(&from)?;
    let to_n = normalize_vault_relative_path(&to)?;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::move_entry(&mut vault, &from, &to)
    })
    .await
    .map_err(|e| format!("vault move task failed: {e}"))??;
    Ok(serde_json::json!({
        "moved": true,
        "from": from_n,
        "to": to_n
    }))
}

#[tauri::command]
pub async fn vault_v3_rename_entry(
    vault_path: String,
    password: String,
    current_name: String,
    new_name: String,
) -> Result<serde_json::Value, String> {
    // Resolve the echoed from/to here; the crate's rename_entry re-derives them
    // identically (normalize current + leaf, rejoin under the same parent).
    let from = normalize_vault_relative_path(&current_name)?;
    let leaf = normalize_leaf_name(&new_name)?;
    let destination = match path_parent(&from) {
        Some(parent) => join_vault_path(parent, &leaf),
        None => leaf.clone(),
    };
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::rename_entry(&mut vault, &current_name, &new_name)
    })
    .await
    .map_err(|e| format!("vault rename task failed: {e}"))??;
    Ok(serde_json::json!({
        "renamed": true,
        "from": from,
        "to": destination
    }))
}

#[tauri::command]
pub async fn vault_v3_copy_entry(
    vault_path: String,
    password: String,
    from: String,
    to: String,
) -> Result<serde_json::Value, String> {
    let from_n = normalize_vault_relative_path(&from)?;
    let to_n = normalize_vault_relative_path(&to)?;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::copy_entry(&mut vault, &from, &to)
    })
    .await
    .map_err(|e| format!("vault copy task failed: {e}"))??;
    Ok(serde_json::json!({
        "copied": true,
        "from": from_n,
        "to": to_n
    }))
}

#[tauri::command]
pub async fn vault_v3_change_password(
    vault_path: String,
    old_password: String,
    new_password: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &old_password)?;
        aerovault::v3::VaultV3::change_password(&mut vault, &new_password)
    })
    .await
    .map_err(|e| format!("vault change-password task failed: {e}"))??;
    Ok("Password changed successfully".to_string())
}

/// AppHandle-free core of [`vault_v3_add_directory`]: recursively add `source_dir`
/// into the v3 vault (optionally under `target_prefix`), returning
/// `(added_files, added_dirs)`. Shared by the GUI command and the CLI `vault add-dir`
/// verb so both walk the tree the same way.
pub async fn vault_v3_add_directory_core(
    vault_path: String,
    password: String,
    source_dir: String,
    target_prefix: Option<String>,
) -> Result<(usize, usize), String> {
    tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
        let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
        let mut vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::add_directory(
            &mut vault,
            Path::new(&source_dir),
            target_prefix.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("vault add directory task failed: {e}"))?
}

#[tauri::command]
pub async fn vault_v3_add_directory(
    app: tauri::AppHandle,
    vault_path: String,
    password: String,
    source_dir: String,
    target_prefix: Option<String>,
) -> Result<serde_json::Value, String> {
    use tauri::Emitter;

    let (added_files, added_dirs) =
        vault_v3_add_directory_core(vault_path, password, source_dir, target_prefix).await?;

    // Single completion tick: the crate walk runs in the blocking worker with no
    // per-file progress, so emit the final count once it returns.
    let _ = app.emit(
        "vault-add-progress",
        serde_json::json!({
            "current": added_files,
            "total": added_files,
            "current_file": ""
        }),
    );

    Ok(serde_json::json!({
        "added_files": added_files,
        "added_dirs": added_dirs,
        "total_entries": added_files + added_dirs
    }))
}

#[tauri::command]
pub async fn vault_v3_extract_entry(
    vault_path: String,
    password: String,
    entry_name: String,
    dest_path: String,
) -> Result<String, String> {
    let extracted = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        let vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::extract_entry(&vault, &entry_name, Path::new(&dest_path))
    })
    .await
    .map_err(|e| format!("vault extract task failed: {e}"))??;
    Ok(extracted.to_string_lossy().to_string())
}

/// Extract every entry in the vault into `dest_path`, preserving the tree
/// (Ehud #2). Returns the number of files written.
#[tauri::command]
pub async fn vault_v3_extract_all(
    vault_path: String,
    password: String,
    dest_path: String,
) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || -> Result<u64, String> {
        let vault = aerovault::v3::VaultV3::open(&vault_path, &password)?;
        aerovault::v3::VaultV3::extract_all(&vault, Path::new(&dest_path))
    })
    .await
    .map_err(|e| format!("vault extract task failed: {e}"))?
}

#[tauri::command]
pub async fn vault_v3_security_info(path: Option<String>) -> serde_json::Value {
    let mut info = serde_json::json!({
        "version": "3.0-draft",
        "pipeline": [
            "small-file-batching",
            "gear-cdc",
            "blake3-keyed-128 chunk ids",
            "zstd per chunk",
            "AES-256-GCM-SIV",
            "BLAKE3-256 cipher block hashes",
            "extension directory for Error Correction (reed-solomon)"
        ],
        "compression_profiles": {
            "fast": 3,
            "balanced": 9,
            "archive": 19
        },
        "compatibility": "v4 is expected to read v3 directly; v3 skips unknown non-critical extensions",
        "error_correction_support": "live: detached Reed-Solomon (.aerocorrect) parity with create/scrub/repair/export-parity, detached-sidecar refresh, and embedded/detached/both placements; reconstruction is re-verified against authenticated material (all-or-nothing). See AEROVAULT-V3-SPEC and #272."
    });

    if let Some(p) = path {
        if let Ok(has_error_correction) = vault_v3_has_error_correction(p).await {
            if let Some(obj) = info.as_object_mut() {
                obj.insert(
                    "error_correction".to_string(),
                    serde_json::json!({
                        "enabled": has_error_correction,
                        "algorithm": "reed-solomon",
                        "version": 1,
                        "critical": false
                    }),
                );
            }
        }
    }

    info
}

#[cfg(test)]
mod tests {
    use super::*;

    // T7 convergence: the AEROVAULT3 container format, crypto pipeline and
    // Error-Correction logic now live in the `aerovault` crate and are covered by
    // its own test suite (incl. the T5 cross-impl byte-compat golden). What stays
    // app-side and is exercised here is only the handler-layer glue: path
    // normalization, the compression-profile mapping, the OOM pre-flight helper
    // and the per-vault write lock.

    #[test]
    fn level_to_profile_maps_known_levels() {
        assert_eq!(level_to_profile(3), "fast");
        // Archive is now zstd -15; -19 stays recognised for pre-change vaults.
        assert_eq!(level_to_profile(15), "archive");
        assert_eq!(level_to_profile(19), "archive");
        assert_eq!(level_to_profile(9), "balanced");
        assert_eq!(level_to_profile(1), "balanced");
    }

    #[test]
    fn apply_profile_level_sets_or_keeps_default() {
        let mk = || aerovault::v3::CreateOptionsV3::new("/tmp/x.aerovault", "pw-12345678");
        assert_eq!(
            apply_profile_level(mk(), Some("fast")).unwrap().zstd_level,
            3
        );
        assert_eq!(
            apply_profile_level(mk(), Some("archive"))
                .unwrap()
                .zstd_level,
            15
        );
        // balanced / empty / none keep the crate default level (untouched).
        let default_level = mk().zstd_level;
        assert_eq!(
            apply_profile_level(mk(), Some("balanced"))
                .unwrap()
                .zstd_level,
            default_level
        );
        assert_eq!(
            apply_profile_level(mk(), None).unwrap().zstd_level,
            default_level
        );
        assert_eq!(
            apply_profile_level(mk(), Some("")).unwrap().zstd_level,
            default_level
        );
        assert!(apply_profile_level(mk(), Some("bogus")).is_err());
    }

    #[test]
    fn validate_vault_path_rejects_traversal_and_absolute() {
        assert!(validate_vault_path("docs/readme.txt").is_ok());
        assert!(validate_vault_path("/etc/passwd").is_err());
        assert!(validate_vault_path("..\\evil").is_err());
        assert!(validate_vault_path("a/../b").is_err());
        assert!(validate_vault_path("C:\\win").is_err());
        assert!(validate_vault_path("has\0null").is_err());
    }

    #[test]
    fn normalize_paths_and_leaf_names() {
        assert_eq!(normalize_vault_relative_path("  /a/b/  ").unwrap(), "a/b");
        assert!(normalize_vault_relative_path("   ").is_err());
        assert!(normalize_vault_relative_path("a//b").is_err());
        assert!(normalize_vault_relative_path("a/./b").is_err());
        assert_eq!(normalize_leaf_name("  file.txt ").unwrap(), "file.txt");
        assert!(normalize_leaf_name("with/slash").is_err());
        assert!(normalize_leaf_name("..").is_err());
    }

    #[test]
    fn join_and_parent_round_trip() {
        assert_eq!(join_vault_path("", "a"), "a");
        assert_eq!(join_vault_path("a/b", "c"), "a/b/c");
        assert_eq!(path_parent("a/b/c"), Some("a/b"));
        assert_eq!(path_parent("root"), None);
    }

    #[test]
    fn safe_entry_name_extracts_basename() {
        let name = safe_entry_name(Path::new("/some/dir/report.pdf")).unwrap();
        assert_eq!(name, "report.pdf");
    }

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn write_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        std::fs::write(&vault, b"placeholder").unwrap();
        {
            let _lock = acquire_vault_write_lock(&vault).unwrap();
            // The lock file exists while held.
            assert!(lock_path_for(&vault).unwrap().exists());
        }
        // ...and is removed on drop.
        assert!(!lock_path_for(&vault).unwrap().exists());
    }

    /// Change Mode (Ehud #2) step 1: repacking a v3 vault under a new compression
    /// profile + Error Correction must preserve every byte and keep the same
    /// password, and the swap must leave a single valid container in place.
    #[test]
    fn change_mode_repack_roundtrip_preserves_content() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        let body_a = b"hello world ".repeat(1000);
        let body_b = vec![7u8; 50_000];
        std::fs::write(src.join("a.txt"), &body_a).unwrap();
        std::fs::write(src.join("sub").join("b.bin"), &body_b).unwrap();

        // Start as a balanced v3 vault with the content added.
        let opts = apply_profile_level(
            aerovault::v3::CreateOptionsV3::new(&vault, "pw-12345678"),
            Some("balanced"),
        )
        .unwrap();
        aerovault::v3::VaultV3::create(&opts).unwrap();
        {
            let mut v = aerovault::v3::VaultV3::open(&vault, "pw-12345678").unwrap();
            aerovault::v3::VaultV3::add_directory(&mut v, &src, None).unwrap();
        }

        // Change mode -> archive profile + Error Correction, same password.
        let target = RepackTarget {
            security_level: None,
            compression_profile: Some("archive".to_string()),
            error_correction: true,
            error_correction_pct: ERROR_CORRECTION_DEFAULT_PCT,
        };
        let res = repack_vault_blocking(&vault, "pw-12345678", &target).unwrap();
        assert_eq!(res["repacked"], serde_json::json!(true));

        // No temp/backup leftovers beside the vault, and the original path is the
        // single surviving container.
        assert!(vault.exists());
        for e in std::fs::read_dir(dir.path()).unwrap() {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !n.contains("repack-") && !n.contains("repack-bak"),
                "stray repack artifact left behind: {n}"
            );
        }

        // The repacked vault opens with the same password and extracts identical bytes.
        let out = dir.path().join("out");
        {
            let v = aerovault::v3::VaultV3::open(&vault, "pw-12345678").unwrap();
            aerovault::v3::VaultV3::extract_all(&v, &out).unwrap();
        }
        assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), body_a);
        assert_eq!(
            std::fs::read(out.join("sub").join("b.bin")).unwrap(),
            body_b
        );
    }

    #[test]
    fn resolve_target_format_maps_levels_and_keeps_source() {
        // Explicit levels resolve to (is_v3, cascade) regardless of source.
        assert_eq!(
            resolve_target_format(Some("standard"), true, false).unwrap(),
            (false, false)
        );
        assert_eq!(
            resolve_target_format(Some("advanced"), true, false).unwrap(),
            (false, false)
        );
        assert_eq!(
            resolve_target_format(Some("paranoid"), true, false).unwrap(),
            (false, true)
        );
        assert_eq!(
            resolve_target_format(Some("experimental"), false, true).unwrap(),
            (true, false)
        );
        assert_eq!(
            resolve_target_format(Some("archive"), false, false).unwrap(),
            (true, false)
        );
        // None / "" keep the source format and (for v2) its cascade flag.
        assert_eq!(
            resolve_target_format(None, true, false).unwrap(),
            (true, false)
        );
        assert_eq!(
            resolve_target_format(Some(""), false, true).unwrap(),
            (false, true)
        );
        assert!(resolve_target_format(Some("bogus"), true, false).is_err());
    }

    /// Helper for the cross-format Change Mode tests: a small two-level tree whose
    /// bytes survive every v2<->v3 repack path unchanged.
    fn write_sample_tree(root: &Path) -> (Vec<u8>, Vec<u8>) {
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let body_a = b"cross-format payload ".repeat(800);
        let body_b = vec![42u8; 40_000];
        std::fs::write(root.join("a.txt"), &body_a).unwrap();
        std::fs::write(root.join("sub").join("b.bin"), &body_b).unwrap();
        (body_a, body_b)
    }

    fn assert_tree_matches(out: &Path, body_a: &[u8], body_b: &[u8]) {
        assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), body_a);
        assert_eq!(
            std::fs::read(out.join("sub").join("b.bin")).unwrap(),
            body_b
        );
    }

    fn assert_no_repack_artifacts(dir: &Path) {
        for e in std::fs::read_dir(dir).unwrap() {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !n.contains("repack-") && !n.contains("repack-bak"),
                "stray repack artifact left behind: {n}"
            );
        }
    }

    /// Full Change Mode (Ehud #2) v3 -> v2: an Archive vault repacked down to the
    /// Standard/Advanced level must become a real v2 container, preserve every byte
    /// under the same password, and drop the v3-only Error Correction.
    #[test]
    fn change_mode_v3_to_v2_standard_preserves_content() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        let src = dir.path().join("src");
        let (body_a, body_b) = write_sample_tree(&src);

        // Start as a v3 vault WITH Error Correction.
        let opts = apply_profile_level(
            aerovault::v3::CreateOptionsV3::new(&vault, "pw-12345678"),
            Some("balanced"),
        )
        .unwrap();
        aerovault::v3::VaultV3::create_with_error_correction(
            &opts,
            aerovault::v3::RecoveryPlacement::Embedded,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        {
            let mut v = aerovault::v3::VaultV3::open(&vault, "pw-12345678").unwrap();
            aerovault::v3::VaultV3::add_directory(&mut v, &src, None).unwrap();
        }

        // Change mode -> Standard (v2), with EC still requested: it must be dropped.
        let target = RepackTarget {
            security_level: Some("standard".to_string()),
            compression_profile: Some("balanced".to_string()),
            error_correction: true,
            error_correction_pct: ERROR_CORRECTION_DEFAULT_PCT,
        };
        let res = repack_vault_blocking(&vault, "pw-12345678", &target).unwrap();
        assert_eq!(res["format"], serde_json::json!(2));
        assert_eq!(res["error_correction"], serde_json::json!(false));
        assert_eq!(res["cascade"], serde_json::json!(false));
        assert_no_repack_artifacts(dir.path());

        // The container is now v2 (not v3), opens with the same password, same bytes.
        assert!(!aerovault::v3::VaultV3::is_vault_v3(&vault));
        let out = dir.path().join("out");
        extract_all_v2_preserving_tree(&vault, "pw-12345678", &out).unwrap();
        assert_tree_matches(&out, &body_a, &body_b);
    }

    /// Full Change Mode v3 -> v2 Paranoid: the target must be a cascade (AES+ChaCha20)
    /// v2 container, byte-identical under the same password.
    #[test]
    fn change_mode_v3_to_v2_paranoid_sets_cascade() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        let src = dir.path().join("src");
        let (body_a, body_b) = write_sample_tree(&src);

        let opts = apply_profile_level(
            aerovault::v3::CreateOptionsV3::new(&vault, "pw-12345678"),
            Some("balanced"),
        )
        .unwrap();
        aerovault::v3::VaultV3::create(&opts).unwrap();
        {
            let mut v = aerovault::v3::VaultV3::open(&vault, "pw-12345678").unwrap();
            aerovault::v3::VaultV3::add_directory(&mut v, &src, None).unwrap();
        }

        let target = RepackTarget {
            security_level: Some("paranoid".to_string()),
            compression_profile: None,
            error_correction: false,
            error_correction_pct: ERROR_CORRECTION_DEFAULT_PCT,
        };
        let res = repack_vault_blocking(&vault, "pw-12345678", &target).unwrap();
        assert_eq!(res["format"], serde_json::json!(2));
        assert_eq!(res["cascade"], serde_json::json!(true));
        assert_no_repack_artifacts(dir.path());

        assert!(!aerovault::v3::VaultV3::is_vault_v3(&vault));
        assert_eq!(
            aerovault::Vault::peek(&vault).unwrap().mode,
            aerovault::EncryptionMode::Cascade
        );
        let out = dir.path().join("out");
        extract_all_v2_preserving_tree(&vault, "pw-12345678", &out).unwrap();
        assert_tree_matches(&out, &body_a, &body_b);
    }

    /// Full Change Mode v2 -> v3 (+ Error Correction): a Standard v2 vault repacked up
    /// to Archive must become a v3 container with EC, byte-identical under the same
    /// password.
    #[test]
    fn change_mode_v2_to_v3_archive_with_ec() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        let src = dir.path().join("src");
        let (body_a, body_b) = write_sample_tree(&src);

        // Start as a Standard v2 vault with content.
        let opts = aerovault::CreateOptions::new(&vault, "pw-12345678")
            .with_mode(aerovault::EncryptionMode::Standard);
        aerovault::Vault::create(opts).unwrap();
        add_directory_v2(&vault, "pw-12345678", &src).unwrap();
        assert!(!aerovault::v3::VaultV3::is_vault_v3(&vault));

        let target = RepackTarget {
            security_level: Some("experimental".to_string()),
            compression_profile: Some("archive".to_string()),
            error_correction: true,
            error_correction_pct: ERROR_CORRECTION_DEFAULT_PCT,
        };
        let res = repack_vault_blocking(&vault, "pw-12345678", &target).unwrap();
        assert_eq!(res["format"], serde_json::json!(3));
        assert_eq!(res["error_correction"], serde_json::json!(true));
        assert_no_repack_artifacts(dir.path());

        assert!(aerovault::v3::VaultV3::is_vault_v3(&vault));
        let out = dir.path().join("out");
        {
            let v = aerovault::v3::VaultV3::open(&vault, "pw-12345678").unwrap();
            aerovault::v3::VaultV3::extract_all(&v, &out).unwrap();
        }
        assert_tree_matches(&out, &body_a, &body_b);
    }

    /// Full Change Mode v2 -> v2 toggle: a Standard vault repacked to Paranoid must
    /// flip on the cascade while keeping the v2 format and every byte.
    #[test]
    fn change_mode_v2_to_v2_toggles_cascade() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        let src = dir.path().join("src");
        let (body_a, body_b) = write_sample_tree(&src);

        let opts = aerovault::CreateOptions::new(&vault, "pw-12345678")
            .with_mode(aerovault::EncryptionMode::Standard);
        aerovault::Vault::create(opts).unwrap();
        add_directory_v2(&vault, "pw-12345678", &src).unwrap();

        let target = RepackTarget {
            security_level: Some("paranoid".to_string()),
            compression_profile: None,
            error_correction: false,
            error_correction_pct: ERROR_CORRECTION_DEFAULT_PCT,
        };
        let res = repack_vault_blocking(&vault, "pw-12345678", &target).unwrap();
        assert_eq!(res["format"], serde_json::json!(2));
        assert_eq!(res["cascade"], serde_json::json!(true));
        assert_no_repack_artifacts(dir.path());

        assert_eq!(
            aerovault::Vault::peek(&vault).unwrap().mode,
            aerovault::EncryptionMode::Cascade
        );
        let out = dir.path().join("out");
        extract_all_v2_preserving_tree(&vault, "pw-12345678", &out).unwrap();
        assert_tree_matches(&out, &body_a, &body_b);
    }

    // ---- Change Mode cross-format stress matrix ----------------------------
    // Round-trips a worst-case directory tree (duplicate basenames across dirs, deep
    // nesting, a multi-chunk file, an empty file, an empty dir, a unicode path)
    // through every source-format x target-level combination, asserting the extracted
    // tree is byte- and structure-identical to the original at each hop.

    const STRESS_PW: &str = "change-mode-stress-pw-123";

    /// Write the worst-case tree to `root` and return its snapshot (relative path ->
    /// `None` for a dir, `Some(bytes)` for a file).
    fn write_stress_tree(root: &Path) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
        let files: &[(&str, Vec<u8>)] = &[
            ("root.txt", b"root-level file".to_vec()),
            ("d1/x.txt", b"content of d1 x".to_vec()),
            ("d2/x.txt", b"DIFFERENT content of d2 x".to_vec()),
            ("d1/deep/deeper/leaf.bin", vec![0xABu8; 70_000]),
            ("d1/empty.txt", Vec::new()),
            (
                "café/ünïcode.txt",
                "naïve café façade \u{1F600}".as_bytes().to_vec(),
            ),
        ];
        std::fs::create_dir_all(root.join("empty_dir")).unwrap();
        for (rel, bytes) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, bytes).unwrap();
        }
        snapshot_dir(root)
    }

    /// Snapshot a directory tree for exact structural + byte comparison.
    fn snapshot_dir(root: &Path) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
        let mut map = std::collections::BTreeMap::new();
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path() == root {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if entry.file_type().is_dir() {
                map.insert(rel, None);
            } else {
                map.insert(rel, Some(std::fs::read(entry.path()).unwrap()));
            }
        }
        map
    }

    /// Create a vault of the given source kind at `path` and add the tree at `src`.
    fn build_source_vault(kind: &str, path: &Path, src: &Path) {
        match kind {
            "v3" => {
                let opts = apply_profile_level(
                    aerovault::v3::CreateOptionsV3::new(path, STRESS_PW),
                    Some("balanced"),
                )
                .unwrap();
                aerovault::v3::VaultV3::create(&opts).unwrap();
                let mut v = aerovault::v3::VaultV3::open(path, STRESS_PW).unwrap();
                aerovault::v3::VaultV3::add_directory(&mut v, src, None).unwrap();
            }
            "v2std" | "v2cascade" => {
                let mode = if kind == "v2cascade" {
                    aerovault::EncryptionMode::Cascade
                } else {
                    aerovault::EncryptionMode::Standard
                };
                let opts = aerovault::CreateOptions::new(path, STRESS_PW).with_mode(mode);
                aerovault::Vault::create(opts).unwrap();
                add_directory_v2(path, STRESS_PW, src).unwrap();
            }
            other => panic!("unknown source kind {other}"),
        }
    }

    /// Extract any-format vault into `dest`, preserving the tree, and snapshot it.
    fn extract_any_snapshot(
        path: &Path,
        dest: &Path,
    ) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
        if aerovault::v3::VaultV3::is_vault_v3(path) {
            let v = aerovault::v3::VaultV3::open(path, STRESS_PW).unwrap();
            aerovault::v3::VaultV3::extract_all(&v, dest).unwrap();
        } else {
            extract_all_v2_preserving_tree(path, STRESS_PW, dest).unwrap();
        }
        snapshot_dir(dest)
    }

    /// Assert that two snapshots agree on every FILE (path + bytes). Empty-dir entries
    /// are checked separately where they must survive; v3's pack/dedup layer does not
    /// re-materialise every intermediate empty dir, so file equality is the contract.
    fn assert_files_match(
        got: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
        want: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
        ctx: &str,
    ) {
        for (rel, v) in want.iter() {
            if let Some(bytes) = v {
                assert_eq!(
                    got.get(rel),
                    Some(&Some(bytes.clone())),
                    "{ctx}: file '{rel}' missing or wrong after repack"
                );
            }
        }
        // The duplicate-basename trap must never collapse.
        assert_ne!(
            got.get("d1/x.txt").unwrap(),
            got.get("d2/x.txt").unwrap(),
            "{ctx}: duplicate basenames collapsed (data loss)"
        );
    }

    #[test]
    fn change_mode_cross_format_matrix_preserves_tree() {
        // (source kind, target level, error-correction). Covers v3<->v2 both ways,
        // cascade on/off, and EC on a v3 target, against the worst-case tree.
        let matrix: &[(&str, &str, bool)] = &[
            ("v3", "standard", false),
            ("v3", "advanced", false),
            ("v3", "paranoid", false),
            ("v3", "archive", true),
            ("v2std", "archive", true),
            ("v2std", "archive", false),
            ("v2std", "paranoid", false),
            ("v2cascade", "standard", false),
            ("v2cascade", "archive", true),
            ("v2cascade", "paranoid", false),
        ];

        for (kind, level, ec) in matrix {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("src");
            let want = write_stress_tree(&src);
            let vault = dir.path().join("v.aerovault");
            build_source_vault(kind, &vault, &src);

            let target = RepackTarget {
                security_level: Some((*level).to_string()),
                compression_profile: Some("balanced".to_string()),
                error_correction: *ec,
                error_correction_pct: ERROR_CORRECTION_DEFAULT_PCT,
            };
            let ctx = format!("{kind} -> {level} (ec={ec})");
            let res = repack_vault_blocking(&vault, STRESS_PW, &target)
                .unwrap_or_else(|e| panic!("{ctx}: repack failed: {e}"));

            // Format + cascade landed as the level dictates; EC only on v3 targets.
            let expect_v3 = *level == "archive" || *level == "experimental";
            assert_eq!(
                res["format"],
                serde_json::json!(if expect_v3 { 3 } else { 2 }),
                "{ctx}: wrong target format"
            );
            assert_eq!(
                res["error_correction"],
                serde_json::json!(*ec && expect_v3),
                "{ctx}: EC flag wrong"
            );
            assert_no_repack_artifacts(dir.path());

            let out = dir.path().join("out");
            let got = extract_any_snapshot(&vault, &out);
            assert_files_match(&got, &want, &ctx);
        }
    }

    #[test]
    fn change_mode_repeated_chain_is_stable() {
        // Re-pack the same vault through a long chain of formats; the tree must stay
        // byte-identical after every hop (no cumulative drift from repeated
        // decrypt/re-encrypt).
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let want = write_stress_tree(&src);
        let vault = dir.path().join("v.aerovault");
        build_source_vault("v3", &vault, &src);

        let chain: &[(&str, bool)] = &[
            ("paranoid", false),
            ("archive", true),
            ("standard", false),
            ("archive", false),
            ("advanced", false),
            ("archive", true),
        ];
        for (i, (level, ec)) in chain.iter().enumerate() {
            let target = RepackTarget {
                security_level: Some((*level).to_string()),
                compression_profile: Some("balanced".to_string()),
                error_correction: *ec,
                error_correction_pct: ERROR_CORRECTION_DEFAULT_PCT,
            };
            repack_vault_blocking(&vault, STRESS_PW, &target)
                .unwrap_or_else(|e| panic!("hop {i} -> {level}: repack failed: {e}"));
            assert_no_repack_artifacts(dir.path());
            let out = dir.path().join(format!("out_{i}"));
            let got = extract_any_snapshot(&vault, &out);
            assert_files_match(&got, &want, &format!("chain hop {i} -> {level}"));
        }
    }

    /// The AppHandle-free `vault_v3_add_directory_core` (shared by the GUI command and
    /// the CLI `add-dir` verb) must add a nested tree and round-trip byte-identical.
    #[test]
    fn v3_add_directory_core_roundtrips_tree_without_apphandle() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let want = write_stress_tree(&src);
        let vault = dir.path().join("v.aerovault");
        let opts = apply_profile_level(
            aerovault::v3::CreateOptionsV3::new(&vault, STRESS_PW),
            Some("balanced"),
        )
        .unwrap();
        aerovault::v3::VaultV3::create(&opts).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (added_files, _added_dirs) = rt
            .block_on(vault_v3_add_directory_core(
                vault.to_string_lossy().to_string(),
                STRESS_PW.to_string(),
                src.to_string_lossy().to_string(),
                None,
            ))
            .unwrap();
        assert!(
            added_files >= 6,
            "expected all files added, got {added_files}"
        );

        let out = dir.path().join("out");
        let got = extract_any_snapshot(&vault, &out);
        assert_files_match(&got, &want, "v3 add_directory_core");
    }

    /// Audit M9: a lock orphaned by a crashed writer (its Drop never ran) names a
    /// dead pid; the next writer must reclaim it instead of blocking ~30s and then
    /// hard-erroring until manual cleanup.
    #[test]
    fn stale_lock_with_dead_pid_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        std::fs::write(&vault, b"placeholder").unwrap();
        let lock_path = lock_path_for(&vault).unwrap();

        // Plant a stale lock naming pid 0 (never a live process -> provably dead).
        std::fs::write(&lock_path, b"pid=0 created_at=2000-01-01T00:00:00Z\n").unwrap();
        assert!(lock_is_stale(&lock_path), "pid=0 lock must read as stale");

        // Acquire must reclaim quickly (well under the 30s busy timeout) and hold it.
        let started = Instant::now();
        let lock = acquire_vault_write_lock(&vault).expect("stale lock should be reclaimed");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "reclaim must be prompt"
        );
        assert!(lock_path.exists());
        drop(lock);
        assert!(!lock_path.exists());
    }

    /// A lock owned by a LIVE process (this test process) must NOT be treated as
    /// stale, so a genuine in-progress seal is never stolen.
    #[test]
    fn live_owner_lock_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("v.aerovault");
        std::fs::write(&vault, b"placeholder").unwrap();
        let lock_path = lock_path_for(&vault).unwrap();
        std::fs::write(
            &lock_path,
            format!(
                "pid={} created_at=2000-01-01T00:00:00Z\n",
                std::process::id()
            ),
        )
        .unwrap();
        assert!(
            !lock_is_stale(&lock_path),
            "a live owner's lock must never be reclaimed"
        );
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(0));
    }
}
