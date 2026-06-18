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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

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
fn level_to_profile(level: i32) -> &'static str {
    match level {
        3 => "fast",
        19 => "archive",
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
        Some("archive") => opts.with_zstd_level(19),
        Some("balanced") | None | Some("") => opts,
        Some(other) => return Err(format!("Unknown AeroVault v3 compression profile: {other}")),
    })
}

/// Forwards the crate's content-pipeline telemetry into a shared [`VaultReport`].
/// Attached to an opened crate vault via `set_telemetry_sink`; the handler keeps
/// the `Arc` to read the populated report back after the blocking op. Poison is
/// recovered (a sink panic must not poison subsequent receipts).
struct ReportSink(Arc<Mutex<VaultReport>>);

impl ReportSink {
    fn with<R>(&self, f: impl FnOnce(&mut VaultReport) -> R) -> R {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

impl aerovault::v3::VaultTelemetrySink for ReportSink {
    fn on_chunk(&mut self, is_new: bool, plaintext: u64, compressed: u64, encrypted: u64) {
        self.with(|r| r.on_chunk(is_new, plaintext, compressed, encrypted));
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
            vault.set_telemetry_sink(Box::new(ReportSink(report_for_task.clone())));
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
        .map_err(|e| format!("vault add directory task failed: {e}"))??;

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
            19
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
        assert!(started.elapsed() < Duration::from_secs(5), "reclaim must be prompt");
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
            format!("pid={} created_at=2000-01-01T00:00:00Z\n", std::process::id()),
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
