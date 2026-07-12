// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroFTP - Modern FTP Client with Tauri
// Real-time transfer progress with event emission

use futures_util::StreamExt;
use reqwest::Client as HttpClient;
use secrecy::{ExposeSecret, SecretString};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigstore::bundle::verify::{blocking::Verifier as SigstoreVerifier, policy};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use url::Url;
use zeroize::Zeroize;

/// Build a `std::process::Command` that never pops up a console window on
/// Windows. On non-Windows platforms this is exactly `Command::new`. Use this
/// for every subprocess spawned from the GUI process so console-subsystem
/// programs (powershell, schtasks, rclone, ...) do not flash a terminal window
/// (issue #351). GUI-subsystem programs (explorer, `open`) never flash, so they
/// do not need it, but using it anyway is harmless.
pub fn hidden_command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    // `mut` is only used inside the Windows-only block below.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub mod aerocrypt;
pub mod aerocrypt_provider;
pub mod aerovault;
pub mod aerovault_v2;
pub mod aerovault_v3;
pub mod agent_memory_db;
pub mod ai;
pub mod ai_core;
pub mod ai_stream;
mod ai_tools;
pub mod app_events;
mod archive_browse;
pub mod archive_progress;
pub mod aws_credentials_import;
pub mod bridge_commands;
pub mod bridge_shared;
mod chat_history;
mod cloud_config;
mod cloud_provider_factory;
mod cloud_service;
mod coding_checkpoints;
mod coding_checks;
mod coding_context;
mod coding_diagnostics;
mod coding_git;
mod coding_patches;
mod coding_rules;
mod coding_search;
mod context_intelligence;
pub mod credential_store;
mod cross_profile_commands;
pub mod cross_profile_transfer;
pub mod crypt_compare;
pub mod crypt_overlay_provider;
mod crypto;
pub mod cryptomator;
mod cyber_tools;
pub mod cyberduck_import;
mod debug_tests;
mod delta_sync;
pub mod dreamweaver_import;
pub mod duplicacy_import;
pub mod error_correction;
pub mod kopia_import;
pub mod lftp_import;
pub mod local_bridge;
pub mod mobaxterm_import;
pub mod panic_safe;
pub mod putty_import;
pub mod readable_vault;
pub mod s3cmd_import;
pub mod vault_mount;
pub mod vault_storage_provider;
pub mod vault_telemetry;
// `pub` only so `tests/integration_delta_sync.rs` (separate crate) can
// inject a MockDeltaTransport. Everything but the hidden inner helper
// remains semver-stable; a future `#[cfg(feature = "test-hooks")]` gate
// is backlogged (see P1-T03 audit, accepted debt).
//
// PR-T11 cross-OS: these modules are now compiled on every platform.
// Their internal Unix-only primitives (binary rsync spawn, SSH exec
// helpers) carry granular `#[cfg(unix)]` gates; the type surface and
// the `DeltaTransport` trait are cross-platform so the provider
// `delta_transport()` method can stay cross-OS.
pub mod delta_sync_rsync;
pub mod delta_transport;
#[cfg(feature = "aerorsync")]
pub mod local_sync;
mod number_parsing;
pub mod peer;
pub mod peer_commands;
pub mod peer_identity;
pub mod portable;
pub mod profile_loader;
mod rsync_output;
pub mod storage_dedup;
pub mod used_scan;
mod user_crypto;
pub mod user_partitions;
// Re-export the partition DEK type so the (public) peer_identity storage facade
// can name it in its signatures without widening the whole user_crypto module.
pub use user_crypto::SecretKey;
#[cfg(windows)]
pub mod windows_update_helper;
// `pub` transitively so integration tests can construct `RsyncStats`
// fixtures for MockDeltaTransport. Same accepted-debt note as above.
pub mod rsync_over_ssh;
mod ssh_exec;
pub mod util;
// Strada C: native rsync prototype (dev-only, gitignored, feature-gated).
// Does not affect production builds. See `src/aerorsync/README.md`.
#[cfg(feature = "aerorsync")]
pub mod aerorsync;
pub mod agent_session;
mod file_tags;
mod file_watcher;
mod filesystem;
pub mod filezilla_import;
mod ftp;
mod ftp_session_pool;
mod ftp_transfer_executor;
mod health_check;
mod host_key_check;
mod infinicloud;
pub mod keystore_export;
mod local_panel_watcher;
mod master_password;
pub mod mc_import;
pub mod mcp;
mod mount_manager;
mod plugin_registry;
mod plugins;
pub mod profile_auth_state;
mod profile_export;
mod provider_commands;
// PD-CLI-CONV-B: the next four modules are exposed for the `aeroftp-cli`
// bin so the CLI file-level batch can converge on the same provider
// executor + orchestrator the GUI uses (sink-agnostic after
// PD-CLI-CONV-A). Additive visibility only: no behaviour change, no API
// break (visibility only widens).
pub mod copy_fallback;
pub mod provider_transfer_executor;
pub mod providers;
mod pty;
pub mod rclone_crypt;
pub mod rclone_filter;
pub mod rclone_import;
pub mod restic_import;
pub mod restricted_chars;
mod session_commands;
mod session_manager;
#[cfg(all(not(target_os = "macos"), feature = "local-stt"))]
mod speech;
pub mod ssh_config_import;
mod ssh_shell;
pub mod sync;
mod sync_badge;
pub mod sync_core;
mod sync_ignore;
mod sync_scheduler;
pub mod sync_script;
mod sync_versioning;
mod totp;
pub mod transfer_dag;
pub mod transfer_dag_batch;
pub mod transfer_dag_single_file;
pub mod transfer_dag_sync;
pub mod transfer_domain;
pub mod transfer_event_sink;
pub mod transfer_orchestrator;
mod transfer_pool;
mod transfer_queue_scan;
pub mod transfer_router;
pub mod transfer_settings;
mod tray_badge;
mod vault_remote;
mod windows_acl;
pub mod winscp_import;
// Stub used when the `local-stt` feature is off OR when targeting macOS
// (where the frontend uses the WKWebView Web Speech API). Both paths
// converge on the same minimal surface so `lib.rs` can register the
// Tauri commands unconditionally and report a uniform "not available"
// status back to the frontend at runtime.
#[cfg(any(target_os = "macos", not(feature = "local-stt")))]
mod speech {
    //! Stub: macOS uses native Web Speech API via WKWebView: whisper.cpp not needed.
    use serde::Serialize;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct SpeechState {
        _dummy: Mutex<()>,
    }

    #[derive(Serialize, Clone)]
    pub struct SpeechModelStatus {
        pub available: bool,
        pub model_path: Option<String>,
        pub model_size_bytes: Option<u64>,
    }

    #[tauri::command]
    pub fn speech_model_status(
        _state: tauri::State<'_, SpeechState>,
    ) -> Result<SpeechModelStatus, String> {
        Ok(SpeechModelStatus {
            available: false,
            model_path: None,
            model_size_bytes: None,
        })
    }

    #[tauri::command]
    pub async fn download_speech_model(
        _app: tauri::AppHandle,
        _state: tauri::State<'_, SpeechState>,
    ) -> Result<String, String> {
        Err("Speech-to-text not available on macOS: use native voice input".to_string())
    }

    #[tauri::command]
    pub async fn speech_to_text(
        _audio_base64: String,
        _language: Option<String>,
        _app: tauri::AppHandle,
        _state: tauri::State<'_, SpeechState>,
    ) -> Result<serde_json::Value, String> {
        Err("Speech-to-text not available on macOS: use native voice input".to_string())
    }
}
#[cfg(windows)]
mod cloud_filter_badge;
mod image_edit;
pub mod server_health;
pub mod settings;
mod speedtest;
mod vault_history;

use filesystem::validate_path;
use ftp::{FtpManager, RemoteFile};
use host_key_check::{sftp_accept_host_key, sftp_check_host_key, sftp_remove_host_key};
use pty::{create_pty_state, pty_close, pty_resize, pty_write, spawn_shell};
use ssh_shell::{
    create_ssh_shell_state, ssh_shell_close, ssh_shell_open, ssh_shell_resize, ssh_shell_write,
};

struct AeroVaultOverlaySessionRuntime {
    vault_path: String,
    password: SecretString,
    version: u8,
    source: String,
    remote_vault_path: Option<String>,
    remote_local_path: Option<String>,
    current_dir: String,
    idle_timeout_secs: u64,
    last_activity: Instant,
    /// Non-zero while a batch transfer is acquiring this session.
    /// The sweeper skips sessions with `busy_holds > 0` even past the
    /// idle timeout so the unified planner (Z.3.6) can drive
    /// long-running overlay↔fs / overlay↔remote transfers without the
    /// background sweep evicting them mid-batch. Counted (rather than a
    /// bool) so nested holds or concurrent transfers on the same vault
    /// stay correct. See [APPENDIX-Z Z.3.6](../../docs/dev/roadmap/APPENDIX-Z_AeroRsync-and-AeroFile-Convergence.md).
    busy_holds: u32,
}

struct AeroVaultOverlayState {
    sessions: Arc<Mutex<HashMap<String, AeroVaultOverlaySessionRuntime>>>,
}

const OVERLAY_IDLE_TIMEOUT_DEFAULT_SECS: u64 = 30 * 60;
const OVERLAY_IDLE_TIMEOUT_MIN_SECS: u64 = 30;
const OVERLAY_IDLE_TIMEOUT_MAX_SECS: u64 = 24 * 60 * 60;
const OVERLAY_SWEEPER_INTERVAL_SECS: u64 = 15;

impl AeroVaultOverlayState {
    fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn sessions_handle(&self) -> Arc<Mutex<HashMap<String, AeroVaultOverlaySessionRuntime>>> {
        Arc::clone(&self.sessions)
    }
}

#[derive(Serialize)]
struct AeroVaultOverlayUnlockResponse {
    session_id: String,
    current_path: String,
}

#[derive(Serialize)]
struct AeroVaultOverlayListResponse {
    current_path: String,
    files: Vec<RemoteFile>,
}

fn normalize_overlay_relative_path(input: &str) -> Result<String, String> {
    let trimmed = input.trim().replace('\\', "/");
    let trimmed = trimmed.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    for part in trimmed.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err("Invalid overlay path".to_string());
        }
        if part.contains('\0') {
            return Err("Invalid overlay path".to_string());
        }
    }
    Ok(trimmed.to_string())
}

fn overlay_display_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", path)
    }
}

fn overlay_join(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", base, name)
    }
}

fn resolve_overlay_target(current: &str, target: &str) -> Result<String, String> {
    let t = target.trim();
    if t.is_empty() || t == "." {
        return Ok(current.to_string());
    }
    if t == ".." {
        if current.is_empty() {
            return Ok(String::new());
        }
        return Ok(current
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
            .unwrap_or_default());
    }

    if t.starts_with('/') {
        return normalize_overlay_relative_path(t);
    }

    let next = if current.is_empty() {
        t.to_string()
    } else {
        format!("{}/{}", current, t)
    };
    normalize_overlay_relative_path(&next)
}

fn persist_overlay_idle_timeout(seconds: u64) -> Result<(), String> {
    let config_dir = portable::aeroftp_data_root().ok_or("Cannot find AeroFTP data root")?;
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    std::fs::write(
        config_dir.join("aerovault_overlay_idle_timeout"),
        seconds.to_string(),
    )
    .map_err(|e| e.to_string())
}

fn load_persisted_overlay_idle_timeout() -> Option<u64> {
    portable::aeroftp_data_root()
        .map(|d| d.join("aerovault_overlay_idle_timeout"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|v| v.clamp(OVERLAY_IDLE_TIMEOUT_MIN_SECS, OVERLAY_IDLE_TIMEOUT_MAX_SECS))
}

fn normalize_overlay_idle_timeout_secs(input: Option<u64>) -> u64 {
    input
        .or_else(load_persisted_overlay_idle_timeout)
        .unwrap_or(OVERLAY_IDLE_TIMEOUT_DEFAULT_SECS)
        .clamp(OVERLAY_IDLE_TIMEOUT_MIN_SECS, OVERLAY_IDLE_TIMEOUT_MAX_SECS)
}

/// Sweep step: drain sessions whose `last_activity` is older than their idle
/// timeout. Returns the evicted (`session_id`, `source`) tuples so the caller
/// can emit notifications. Pure over the HashMap so it can be unit-tested
/// without touching the Tauri runtime.
fn drain_expired_overlay_sessions(
    sessions: &mut HashMap<String, AeroVaultOverlaySessionRuntime>,
    now: Instant,
) -> Vec<(String, String)> {
    if sessions.is_empty() {
        return Vec::new();
    }
    let to_evict: Vec<String> = sessions
        .iter()
        .filter(|(_, s)| {
            // Busy sessions skip the sweep even past the idle timeout:
            // a planner-driven batch transfer (Z.3.6) holds the lock for
            // the duration of the operation. Stale holds (e.g. process
            // crash mid-transfer) would resurface as soon as the next
            // sweep arrives because the in-memory state is rebuilt on
            // every restart.
            if s.busy_holds > 0 {
                return false;
            }
            now.duration_since(s.last_activity).as_secs() > s.idle_timeout_secs
        })
        .map(|(id, _)| id.clone())
        .collect();
    to_evict
        .into_iter()
        .filter_map(|id| sessions.remove(&id).map(|s| (id, s.source.clone())))
        .collect()
}

/// Live preview of the 6-digit code derived from a base32 TOTP secret,
/// plus the seconds left before it rolls over. Powers the connection-form
/// diagnostic next to the saved-secret field so the user can confirm the
/// app derives the same code as their authenticator app (issue #128).
/// Reuses the exact derivation used at connect time, so a match here means
/// a match at login.
#[tauri::command]
fn preview_provider_totp(secret: String) -> Result<serde_json::Value, String> {
    let (code, seconds_remaining) =
        providers::totp_helper::generate_totp_code_with_ttl(&secrecy::SecretString::from(secret))
            .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "code": code,
        "seconds_remaining": seconds_remaining,
    }))
}

#[tauri::command]
async fn aerovault_overlay_get_idle_timeout() -> Result<u64, String> {
    Ok(load_persisted_overlay_idle_timeout().unwrap_or(OVERLAY_IDLE_TIMEOUT_DEFAULT_SECS))
}

#[tauri::command]
async fn aerovault_overlay_set_idle_timeout(seconds: u64) -> Result<u64, String> {
    let clamped = seconds.clamp(OVERLAY_IDLE_TIMEOUT_MIN_SECS, OVERLAY_IDLE_TIMEOUT_MAX_SECS);
    persist_overlay_idle_timeout(clamped)?;
    Ok(clamped)
}

/// Acquire a busy hold on an overlay session: the sweeper will skip it
/// even past the idle timeout until the matching `aerovault_overlay_busy_release`
/// is called. Used by the unified transfer planner (Z.3.6) so an
/// overlay↔fs or overlay↔remote batch can run for longer than the idle
/// window without losing its session handle mid-flight. The counter is
/// nested-safe: paired acquire/release calls bracket each transfer, so
/// concurrent transfers on the same vault stay consistent.
#[tauri::command]
async fn aerovault_overlay_busy_acquire(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
) -> Result<u32, String> {
    let mut sessions = overlay_state.sessions.lock().await;
    let runtime = sessions
        .get_mut(&session_id)
        .ok_or_else(|| format!("overlay session '{}' not found", session_id))?;
    runtime.busy_holds = runtime.busy_holds.saturating_add(1);
    // Refresh activity so the sweeper sees a fresh timer when the lock
    // is finally released, instead of immediately evicting on the next
    // sweep after a long transfer.
    runtime.last_activity = Instant::now();
    Ok(runtime.busy_holds)
}

/// Release a previously-acquired busy hold. Saturating subtraction so
/// a stray double-release doesn't underflow the counter; the matching
/// `acquire` is responsible for getting the bookkeeping right.
#[tauri::command]
async fn aerovault_overlay_busy_release(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
) -> Result<u32, String> {
    let mut sessions = overlay_state.sessions.lock().await;
    let runtime = sessions
        .get_mut(&session_id)
        .ok_or_else(|| format!("overlay session '{}' not found", session_id))?;
    runtime.busy_holds = runtime.busy_holds.saturating_sub(1);
    runtime.last_activity = Instant::now();
    Ok(runtime.busy_holds)
}

#[tauri::command]
async fn aerovault_overlay_unlock(
    overlay_state: State<'_, AeroVaultOverlayState>,
    vault_path: String,
    password: String,
    source: Option<String>,
    remote_vault_path: Option<String>,
    remote_local_path: Option<String>,
    idle_timeout_seconds: Option<u64>,
) -> Result<AeroVaultOverlayUnlockResponse, String> {
    validate_path(&vault_path)?;
    let normalized_source = source.unwrap_or_else(|| "local".to_string());
    let idle_timeout_secs = normalize_overlay_idle_timeout_secs(idle_timeout_seconds);
    let now = Instant::now();

    // Validate credentials once and fail early.
    let version = if aerovault_v3::is_vault_v3(vault_path.clone()).await? {
        let _ = aerovault_v3::vault_v3_open(vault_path.clone(), password.clone()).await?;
        3
    } else {
        let _ = aerovault_v2::vault_v2_open(vault_path.clone(), password.clone()).await?;
        2
    };

    let mut sessions = overlay_state.sessions.lock().await;
    if let Some(existing_id) = sessions
        .iter()
        .find(|(_, s)| {
            s.vault_path == vault_path
                && s.password.expose_secret() == password
                && s.source == normalized_source
                && s.remote_vault_path == remote_vault_path
                && s.remote_local_path == remote_local_path
        })
        .map(|(id, _)| id.clone())
    {
        if let Some(existing) = sessions.get_mut(&existing_id) {
            let is_expired =
                now.duration_since(existing.last_activity).as_secs() > existing.idle_timeout_secs;
            if !is_expired {
                existing.last_activity = now;
                existing.idle_timeout_secs = idle_timeout_secs;
                return Ok(AeroVaultOverlayUnlockResponse {
                    session_id: existing_id,
                    current_path: overlay_display_path(&existing.current_dir),
                });
            }
        }
        sessions.remove(&existing_id);
    }

    let session_id = format!("avol_{}", uuid::Uuid::new_v4());
    sessions.insert(
        session_id.clone(),
        AeroVaultOverlaySessionRuntime {
            vault_path,
            password: SecretString::new(password.into_boxed_str()),
            version,
            source: normalized_source,
            remote_vault_path,
            remote_local_path,
            current_dir: String::new(),
            idle_timeout_secs,
            last_activity: now,
            busy_holds: 0,
        },
    );

    Ok(AeroVaultOverlayUnlockResponse {
        session_id,
        current_path: "/".to_string(),
    })
}

#[tauri::command]
async fn aerovault_overlay_lock(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
) -> Result<bool, String> {
    let mut sessions = overlay_state.sessions.lock().await;
    Ok(sessions.remove(&session_id).is_some())
}

#[tauri::command]
async fn aerovault_overlay_list(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    path: Option<String>,
) -> Result<AeroVaultOverlayListResponse, String> {
    let (vault_path, password, version, current_dir) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get_mut(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        if let Some(target) = path.as_deref() {
            session.current_dir = resolve_overlay_target(&session.current_dir, target)?;
        }
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
            session.current_dir.clone(),
        )
    };

    let prefix = if current_dir.is_empty() {
        String::new()
    } else {
        format!("{}/", current_dir)
    };

    let mut child_names_seen: HashSet<String> = HashSet::new();
    let mut files: Vec<RemoteFile> = Vec::new();

    if version == 3 {
        let opened = aerovault_v3::vault_v3_open(vault_path, password).await?;
        for entry in opened.files {
            let full_name = entry.name.replace('\\', "/");
            if full_name.is_empty() {
                continue;
            }
            if !prefix.is_empty() && !full_name.starts_with(&prefix) {
                continue;
            }

            let rel = if prefix.is_empty() {
                full_name.as_str()
            } else {
                &full_name[prefix.len()..]
            };
            if rel.is_empty() {
                continue;
            }

            let mut split = rel.splitn(2, '/');
            let first = split.next().unwrap_or_default();
            let has_child = split.next().is_some();
            if first.is_empty() {
                continue;
            }

            if has_child {
                if child_names_seen.insert(first.to_string()) {
                    files.push(RemoteFile {
                        name: first.to_string(),
                        path: format!("/{}", overlay_join(&current_dir, first)),
                        size: None,
                        is_dir: true,
                        modified: None,
                        permissions: None,
                    });
                }
                continue;
            }

            child_names_seen.insert(first.to_string());
            files.push(RemoteFile {
                name: first.to_string(),
                path: format!("/{}", overlay_join(&current_dir, first)),
                size: if entry.is_dir { None } else { Some(entry.size) },
                is_dir: entry.is_dir,
                modified: Some(entry.modified),
                permissions: None,
            });
        }
    } else {
        let opened = aerovault_v2::vault_v2_open(vault_path, password).await?;
        let entries = opened
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "Invalid AeroVault open response".to_string())?;

        for entry in entries {
            let full_name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .replace('\\', "/");
            if full_name.is_empty() {
                continue;
            }
            if !prefix.is_empty() && !full_name.starts_with(&prefix) {
                continue;
            }

            let rel = if prefix.is_empty() {
                full_name.as_str()
            } else {
                &full_name[prefix.len()..]
            };

            if rel.is_empty() {
                continue;
            }

            let mut split = rel.splitn(2, '/');
            let first = split.next().unwrap_or_default();
            let has_child = split.next().is_some();
            if first.is_empty() {
                continue;
            }

            if has_child {
                if child_names_seen.insert(first.to_string()) {
                    files.push(RemoteFile {
                        name: first.to_string(),
                        path: format!("/{}", overlay_join(&current_dir, first)),
                        size: None,
                        is_dir: true,
                        modified: None,
                        permissions: None,
                    });
                }
                continue;
            }

            child_names_seen.insert(first.to_string());
            let size = entry.get("size").and_then(|v| v.as_u64());
            let is_dir = entry
                .get("is_dir")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let modified = entry.get("modified").and_then(|v| {
                if v.is_null() {
                    None
                } else if let Some(s) = v.as_str() {
                    Some(s.to_string())
                } else {
                    Some(v.to_string())
                }
            });
            files.push(RemoteFile {
                name: first.to_string(),
                path: format!("/{}", overlay_join(&current_dir, first)),
                size,
                is_dir,
                modified,
                permissions: None,
            });
        }
    }

    files.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(AeroVaultOverlayListResponse {
        current_path: overlay_display_path(&current_dir),
        files,
    })
}

#[tauri::command]
async fn aerovault_overlay_extract_entry(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    entry_path: String,
    output_path: String,
) -> Result<String, String> {
    validate_path(&output_path)?;
    let entry_name = normalize_overlay_relative_path(&entry_path)?;

    let (vault_path, password, version) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
        )
    };

    let out_path = std::path::PathBuf::from(&output_path);
    let out_parent = out_path
        .parent()
        .ok_or_else(|| "Invalid output path".to_string())?;

    std::fs::create_dir_all(out_parent)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    let extracted = if version == 3 {
        aerovault_v3::vault_v3_extract_entry(
            vault_path,
            password,
            entry_name,
            out_parent.to_string_lossy().to_string(),
        )
        .await
        .map(std::path::PathBuf::from)?
    } else {
        aerovault_v2::vault_v2_extract_entry(
            vault_path,
            password,
            entry_name,
            out_parent.to_string_lossy().to_string(),
        )
        .await
        .map(std::path::PathBuf::from)?
    };

    if extracted != out_path {
        match std::fs::rename(&extracted, &out_path) {
            Ok(_) => {}
            Err(_) => {
                if extracted.is_dir() {
                    return Err("Failed to move extracted directory to destination".to_string());
                }
                std::fs::copy(&extracted, &out_path)
                    .map_err(|e| format!("Failed to move extracted file: {}", e))?;
                let _ = std::fs::remove_file(&extracted);
            }
        }
    }

    Ok(output_path)
}

#[tauri::command]
async fn aerovault_overlay_add_file(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    local_plaintext_path: String,
    remote_plain_name: Option<String>,
) -> Result<String, String> {
    validate_path(&local_plaintext_path)?;
    let local_meta = std::fs::symlink_metadata(std::path::Path::new(&local_plaintext_path))
        .map_err(|e| format!("Failed to inspect local file: {}", e))?;
    if local_meta.file_type().is_symlink() {
        return Err("Local plaintext path cannot be a symlink".to_string());
    }
    if !local_meta.is_file() {
        return Err("Local plaintext path must be a regular file".to_string());
    }

    let (vault_path, password, version, current_dir) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
            session.current_dir.clone(),
        )
    };

    let file_name = remote_plain_name
        .and_then(|s| {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .or_else(|| {
            std::path::Path::new(&local_plaintext_path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
        })
        .ok_or_else(|| "Cannot determine destination filename".to_string())?;

    if file_name.contains('/') || file_name.contains('\\') || file_name.contains('\0') {
        return Err("Invalid destination filename".to_string());
    }

    let source_basename = std::path::Path::new(&local_plaintext_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.clone());

    let mut temp_copy: Option<std::path::PathBuf> = None;
    let upload_source = if source_basename == file_name {
        std::path::PathBuf::from(&local_plaintext_path)
    } else {
        let temp_path = std::env::temp_dir().join(format!(
            "aeroftp_aerovault_overlay_{}_{}",
            chrono::Utc::now().timestamp_millis(),
            uuid::Uuid::new_v4()
        ));
        let target_path = temp_path.with_file_name(file_name.clone());
        std::fs::copy(&local_plaintext_path, &target_path)
            .map_err(|e| format!("Failed to prepare renamed upload file: {}", e))?;
        temp_copy = Some(target_path.clone());
        target_path
    };

    let add_result = if version == 3 {
        if current_dir.is_empty() {
            aerovault_v3::vault_v3_add_files_inner(
                vault_path.clone(),
                password.clone(),
                vec![upload_source.to_string_lossy().to_string()],
                None,
            )
            .await
            .map(|_| serde_json::json!({ "ok": true }))
        } else {
            aerovault_v3::vault_v3_add_files_to_dir(
                vault_path.clone(),
                password.clone(),
                vec![upload_source.to_string_lossy().to_string()],
                current_dir.clone(),
            )
            .await
        }
    } else if current_dir.is_empty() {
        aerovault_v2::vault_v2_add_files(
            vault_path.clone(),
            password.clone(),
            vec![upload_source.to_string_lossy().to_string()],
        )
        .await
    } else {
        aerovault_v2::vault_v2_add_files_to_dir(
            vault_path.clone(),
            password.clone(),
            vec![upload_source.to_string_lossy().to_string()],
            current_dir.clone(),
        )
        .await
    };

    if let Some(path) = temp_copy {
        let _ = std::fs::remove_file(path);
    }

    add_result.map_err(|e| e.to_string())?;
    Ok(format!("/{}", overlay_join(&current_dir, &file_name)))
}

#[tauri::command]
async fn aerovault_overlay_create_directory(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    dir_name: String,
) -> Result<String, String> {
    let folder = dir_name.trim();
    if folder.is_empty()
        || folder.contains('/')
        || folder.contains('\\')
        || folder.contains("..")
        || folder.contains('\0')
    {
        return Err("Invalid directory name".to_string());
    }

    let (vault_path, password, version, current_dir) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
            session.current_dir.clone(),
        )
    };

    let dir_rel = normalize_overlay_relative_path(&overlay_join(&current_dir, folder))?;
    if version == 3 {
        aerovault_v3::vault_v3_create_directory(vault_path, password, dir_rel.clone()).await?;
    } else {
        aerovault_v2::vault_v2_create_directory(vault_path, password, dir_rel.clone()).await?;
    }
    Ok(format!("/{}", dir_rel))
}

#[tauri::command]
async fn aerovault_overlay_delete_entries(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    entry_paths: Vec<String>,
    recursive: bool,
) -> Result<u64, String> {
    if entry_paths.is_empty() {
        return Ok(0);
    }

    let (vault_path, password, version) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
        )
    };

    let mut normalized: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for path in entry_paths {
        let p = normalize_overlay_relative_path(&path)?;
        if seen.insert(p.clone()) {
            normalized.push(p);
        }
    }

    let removed = if version == 3 {
        let result =
            aerovault_v3::vault_v3_delete_entries(vault_path, password, normalized, recursive)
                .await?;
        result.get("removed").and_then(|v| v.as_u64()).unwrap_or(0)
    } else {
        let result =
            aerovault_v2::vault_v2_delete_entries(vault_path, password, normalized, recursive)
                .await?;
        result.get("removed").and_then(|v| v.as_u64()).unwrap_or(0)
    };
    Ok(removed)
}

#[tauri::command]
async fn aerovault_overlay_move_entry(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    from_path: String,
    to_path: String,
) -> Result<String, String> {
    let from_rel = normalize_overlay_relative_path(&from_path)?;
    let to_rel = normalize_overlay_relative_path(&to_path)?;

    let (vault_path, password, version) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
        )
    };

    if version == 3 {
        aerovault_v3::vault_v3_move_entry(vault_path, password, from_rel, to_rel.clone()).await?;
    } else {
        aerovault_v2::vault_v2_move_entry(vault_path, password, from_rel, to_rel.clone()).await?;
    }
    Ok(format!("/{}", to_rel))
}

#[tauri::command]
async fn aerovault_overlay_rename_entry(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    entry_path: String,
    new_name: String,
) -> Result<String, String> {
    let entry_rel = normalize_overlay_relative_path(&entry_path)?;
    if new_name.trim().is_empty()
        || new_name.contains('/')
        || new_name.contains('\\')
        || new_name.contains("..")
        || new_name.contains('\0')
    {
        return Err("Invalid destination filename".to_string());
    }

    let (vault_path, password, version) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
        )
    };

    if version == 3 {
        aerovault_v3::vault_v3_rename_entry(
            vault_path,
            password,
            entry_rel.clone(),
            new_name.trim().to_string(),
        )
        .await?;
    } else {
        aerovault_v2::vault_v2_rename_entry(
            vault_path,
            password,
            entry_rel.clone(),
            new_name.trim().to_string(),
        )
        .await?;
    }

    let renamed = if let Some((parent, _)) = entry_rel.rsplit_once('/') {
        format!("{}/{}", parent, new_name.trim())
    } else {
        new_name.trim().to_string()
    };
    Ok(format!("/{}", renamed))
}

#[tauri::command]
async fn aerovault_overlay_copy_entry(
    overlay_state: State<'_, AeroVaultOverlayState>,
    session_id: String,
    from_path: String,
    to_path: String,
) -> Result<String, String> {
    let from_rel = normalize_overlay_relative_path(&from_path)?;
    let to_rel = normalize_overlay_relative_path(&to_path)?;

    let (vault_path, password, version) = {
        let mut sessions = overlay_state.sessions.lock().await;
        let now = Instant::now();
        let expired = {
            let session = sessions
                .get_mut(&session_id)
                .ok_or_else(|| "Overlay session not found".to_string())?;
            let expired =
                now.duration_since(session.last_activity).as_secs() > session.idle_timeout_secs;
            if !expired {
                session.last_activity = now;
            }
            expired
        };
        if expired {
            sessions.remove(&session_id);
            return Err("Overlay session expired due to inactivity".to_string());
        }
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| "Overlay session not found".to_string())?;
        (
            session.vault_path.clone(),
            session.password.expose_secret().to_string(),
            session.version,
        )
    };

    if version == 3 {
        aerovault_v3::vault_v3_copy_entry(vault_path, password, from_rel, to_rel.clone()).await?;
    } else {
        aerovault_v2::vault_v2_copy_entry(vault_path, password, from_rel, to_rel.clone()).await?;
    }
    Ok(format!("/{}", to_rel))
}

/// Join a remote base directory and a name into a single remote path, tolerant
/// of an empty / root base and an existing trailing slash. Shared by the kept
/// crypt-overlay commands (rclone create-remote, aerocrypt read-config/create).
fn join_remote_path(base: &str, name: &str) -> String {
    if base.is_empty() || base == "/" {
        format!("/{}", name)
    } else if base.ends_with('/') {
        format!("{}{}", base, name)
    } else {
        format!("{}/{}", base, name)
    }
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn rclone_crypt_provider_create_remote(
    provider_state: State<'_, provider_commands::ProviderState>,
    rclone_state: State<'_, rclone_crypt::RcloneCryptState>,
    password: String,
    salt: Option<String>,
    filename_encryption: Option<String>,
    suffix: Option<String>,
    directory_name_encryption: Option<bool>,
    target_subpath: Option<String>,
) -> Result<rclone_crypt::RcloneCryptVaultInfo, String> {
    let (name_key, data_key, name_tweak) =
        rclone_crypt::derive_keys_with_tweak(&password, salt.as_deref().unwrap_or(""))?;
    let mode = match filename_encryption.as_deref() {
        Some("off") => rclone_crypt::FilenameEncryption::Off,
        Some("obfuscate") => rclone_crypt::FilenameEncryption::Obfuscate,
        _ => rclone_crypt::FilenameEncryption::Standard,
    };
    let off_suffix = rclone_crypt::resolve_off_suffix(suffix.as_deref());
    let dir_name_enc = directory_name_encryption.unwrap_or(true);

    {
        let mut provider_lock = provider_state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;
        let saved_pwd = provider.pwd().await.unwrap_or_else(|_| "/".to_string());

        let target = target_subpath
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        let init_result: Result<(), String> = async {
            if let Some(sub) = target {
                let _ = provider.mkdir(sub).await; // idempotent
                provider
                    .cd(sub)
                    .await
                    .map_err(|e| format!("Failed to cd into {}: {}", sub, e))?;
            }

            Ok(())
        }
        .await;

        let _ = provider.cd(&saved_pwd).await;
        init_result?;
    }

    let vault_id = uuid::Uuid::new_v4().to_string();
    let info = rclone_crypt::RcloneCryptVaultInfo {
        vault_id: vault_id.clone(),
        filename_encryption: mode,
        off_suffix: off_suffix.clone(),
        directory_name_encryption: dir_name_enc,
    };
    let keys = rclone_crypt::RcloneCryptKeys {
        name_key,
        data_key,
        name_tweak,
        filename_encryption: mode,
        off_suffix,
        directory_name_encryption: dir_name_enc,
    };
    rclone_state.vaults.lock().await.insert(vault_id, keys);
    Ok(info)
}

/// Global transfer speed limits (bytes per second, 0 = unlimited)
pub struct SpeedLimits {
    pub download_bps: std::sync::atomic::AtomicU64,
    pub upload_bps: std::sync::atomic::AtomicU64,
}

impl SpeedLimits {
    fn new() -> Self {
        Self {
            download_bps: std::sync::atomic::AtomicU64::new(0),
            upload_bps: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

/// Guard to ensure exit cleanup runs at most once.
static EXITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Gracefully exit the application, performing cleanup before terminating.
/// Used by both the app menu Quit and the tray Quit so that an explicit quit
/// always exits even when AeroCloud's hide-to-tray is active.
/// Safe to call multiple times: cleanup runs only on the first invocation.
fn exit_app(app: &tauri::AppHandle) {
    if EXITING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return; // Already exiting
    }
    info!("exit_app: explicit quit requested, shutting down");
    #[cfg(windows)]
    {
        if let Err(e) = crate::cloud_filter_badge::cleanup_all_roots() {
            warn!("Cloud Filter cleanup on exit: {}", e);
        }
    }
    // Unmount any GUI-spawned mounts so the kernel does not keep dangling
    // FUSE handles. Daemon-spawned mounts (Phase B autostart) are independent.
    let app_clone = app.clone();
    tauri::async_runtime::block_on(async move {
        crate::mount_manager::stop_all().await;
        // Unmount any ephemeral read-only vault mounts (#322 Deliverable B): they
        // must not outlive the app holding the keys.
        crate::vault_mount::stop_all().await;
        let _ = app_clone;
    });
    app.exit(0);
}

/// Apply rate limiting by sleeping after transferring a chunk.
/// Returns immediately if limit is 0 (unlimited).
pub async fn throttle_transfer(
    bytes_transferred: u64,
    elapsed: std::time::Duration,
    limit_bps: u64,
) {
    if limit_bps == 0 {
        return;
    }
    let expected_duration =
        std::time::Duration::from_secs_f64(bytes_transferred as f64 / limit_bps as f64);
    if expected_duration > elapsed {
        tokio::time::sleep(expected_duration - elapsed).await;
    }
}

// Shared application state
pub(crate) struct AppState {
    pub(crate) ftp_manager: Mutex<FtpManager>,
    pub(crate) cancel_flag: Arc<AtomicBool>,
    cancel_token: Mutex<CancellationToken>,
    speed_limits: SpeedLimits,
}

impl AppState {
    fn new() -> Self {
        Self {
            ftp_manager: Mutex::new(FtpManager::new()),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            cancel_token: Mutex::new(CancellationToken::new()),
            speed_limits: SpeedLimits::new(),
        }
    }

    async fn reset_cancel_state(&self) -> CancellationToken {
        self.cancel_flag.store(false, Ordering::Relaxed);
        let token = CancellationToken::new();
        *self.cancel_token.lock().await = token.clone();
        token
    }

    async fn request_cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.cancel_token.lock().await.cancel();
    }
}

// ============ Request/Response Structs ============

#[derive(Serialize, Deserialize)]
pub struct ConnectionParams {
    server: String,
    username: String,
    password: String,
    /// W3.1 (#270.5): frontend-generated token identifying this connection
    /// attempt. When present, `connect_ftp` registers a cancellation token
    /// under it so an Esc / "still connecting" Cancel can abort the connect
    /// via `cancel_connection`. Unknown extra fields on the wire are ignored,
    /// so legacy callers that omit it keep working.
    #[serde(default, alias = "connectToken")]
    connect_token: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DownloadParams {
    remote_path: String,
    local_path: String,
    /// Remote file modification timestamp (ISO 8601) for mtime preservation
    #[serde(default)]
    modified: Option<String>,
    #[serde(default = "default_true")]
    use_delta: bool,
}

#[derive(Serialize, Deserialize)]
pub struct UploadParams {
    local_path: String,
    remote_path: String,
    #[serde(default = "default_true")]
    use_delta: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
pub struct DownloadFolderParams {
    remote_path: String,
    local_path: String,
    #[serde(default)]
    file_exists_action: String,
    #[serde(default)]
    max_concurrent: Option<u32>,
    #[serde(default)]
    retry_count: Option<u32>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct UploadFolderParams {
    local_path: String,
    remote_path: String,
    #[serde(default)]
    file_exists_action: String,
    #[serde(default)]
    max_concurrent: Option<u32>,
    #[serde(default)]
    retry_count: Option<u32>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct FileTransferBatchParams {
    entries: Vec<transfer_domain::TransferEntry>,
    #[serde(default)]
    max_concurrent: Option<u32>,
    #[serde(default)]
    retry_count: Option<u32>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Serialize)]
pub struct FileListResponse {
    files: Vec<RemoteFile>,
    current_path: String,
}

// ============ Transfer Progress Events ============

#[derive(Clone, Serialize)]
pub struct TransferProgress {
    pub transfer_id: String,
    pub filename: String,
    pub transferred: u64,
    pub total: u64,
    pub percentage: u8,
    pub speed_bps: u64,
    pub eta_seconds: u32,
    pub direction: String, // "download" or "upload"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_files: Option<u64>, // When set, transferred/total are file counts (folder transfer)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>, // Full path for context
}

#[derive(Clone, Serialize)]
pub struct TransferEvent {
    pub event_type: String, // "start", "progress", "complete", "error", "cancelled"
    pub transfer_id: String,
    pub filename: String,
    pub direction: String,
    pub message: Option<String>,
    pub progress: Option<TransferProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>, // Full path for context (file or folder)
    /// Populated only on `event_type == "complete"` when the rsync delta
    /// path actually serviced the transfer (SFTP + key-auth + rsync on the
    /// remote). Absent for classic transfers and for providers that don't
    /// support delta. Frontend uses this to render the per-file delta badge
    /// and accumulate the end-of-sync savings card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_stats: Option<sync::DeltaTransferStats>,
    /// Populated only on `event_type == "complete"` when delta sync was
    /// attempted for this file but declined to the classic path
    /// transparently. Pure classic transfers keep this absent so the UI can
    /// distinguish "classic by design" from "classic after delta attempt".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// Fix A: build a GUI progress sink for the interactive native-delta path.
///
/// Emits the same `transfer_event { event_type: "progress" }` shape the classic
/// transfer path emits, keyed by the same `transfer_id`, so AeroProgress renders
/// a delta transfer identically (no frontend change). `transferred` / `total`
/// are wire bytes: for upload `total` is the full delta payload (the bar fills
/// accurately); for download it is the remote file size hint, so on a real delta
/// hit the bar under-fills and the final `complete` event takes it to 100%.
/// Speed and ETA are derived from wire bytes over elapsed time, exactly like the
/// classic callback. The driver throttles calls (~1% steps), so this closure and
/// its IPC emit fire sparingly. Only the GUI command path passes one of these;
/// AeroSync and the CLI pass `None`.
pub(crate) fn make_delta_progress_sink(
    app: tauri::AppHandle,
    transfer_id: String,
    filename: String,
    direction: &'static str,
) -> crate::delta_transport::DeltaProgressSink {
    use tauri::Emitter;
    let start = std::time::Instant::now();
    Box::new(move |transferred: u64, total: u64| {
        let percentage = if total > 0 {
            ((transferred as f64 / total as f64) * 100.0).min(100.0) as u8
        } else {
            0
        };
        let elapsed = start.elapsed().as_secs_f64();
        let speed_bps = if elapsed > 0.1 {
            (transferred as f64 / elapsed) as u64
        } else {
            0
        };
        let eta_seconds = if speed_bps > 0 && transferred < total {
            ((total - transferred) as f64 / speed_bps as f64) as u32
        } else {
            0
        };
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: transfer_id.clone(),
                filename: filename.clone(),
                direction: direction.to_string(),
                message: None,
                progress: Some(TransferProgress {
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    transferred,
                    total,
                    percentage,
                    speed_bps,
                    eta_seconds,
                    direction: direction.to_string(),
                    total_files: None,
                    path: None,
                }),
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
    })
}

// ============ Local File Info ============

#[derive(Serialize)]
pub struct LocalFileInfo {
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
    pub is_dir: bool,
    pub modified: Option<String>,
}

// ============ Updater Structs ============

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Serialize)]
struct UpdateInfo {
    has_update: bool,
    latest_version: Option<String>,
    download_url: Option<String>,
    current_version: String,
    install_format: String,
}

#[derive(Clone, Debug)]
struct ReleaseAssetSelection {
    tag: String,
    asset_name: String,
    download_url: String,
    bundle_url: String,
}

#[derive(Serialize, Clone)]
struct UpdateDownloadProgress {
    downloaded: u64,
    total: u64,
    percentage: u8,
    speed_bps: u64,
    eta_seconds: u64,
    filename: String,
}

const GITHUB_RELEASES_API_URL: &str =
    "https://api.github.com/repos/axpdev-lab/aeroftp/releases/latest";
const GITHUB_RELEASES_HOST: &str = "github.com";
const GITHUB_RELEASES_OWNER: &str = "axpdev-lab";
const GITHUB_RELEASES_REPO: &str = "aeroftp";
const SIGSTORE_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
const SIGSTORE_WORKFLOW_IDENTITY_PREFIX: &str =
    "https://github.com/axpdev-lab/aeroftp/.github/workflows/build.yml@refs/tags/";

// ============ Updater Command ============

/// Directory where self-extracting auto-update artifacts (portable .zip,
/// .AppImage) are staged before being applied. Kept out of the user-visible
/// `~/Downloads/` so we don't pollute it with archive boxes that have no
/// independent life once the swap is done.
///
/// Linux:   `$XDG_CACHE_HOME/aeroftp/updates/` (or `~/.cache/aeroftp/updates/`)
/// macOS:   `~/Library/Caches/aeroftp/updates/`
/// Windows: `%LOCALAPPDATA%\aeroftp\updates\` (or `%TEMP%\aeroftp-updates\`)
///
/// Falls back to a sub-folder under the OS temp dir if no cache dir is
/// resolvable. Issue #176.
fn updates_staging_dir() -> PathBuf {
    if let Some(cache) = dirs::cache_dir() {
        return cache
            .join(portable::aeroftp_data_leaf_for_debug(cfg!(
                debug_assertions
            )))
            .join("updates");
    }
    std::env::temp_dir().join("aeroftp-updates")
}

/// True when the asset name describes a self-extracting artifact: portable
/// `.zip` (Windows) or `.AppImage` (Linux). Both decompose into the running
/// application after install, so the source archive has no independent life
/// (no installer to re-run, no separate `.exe`/`.deb` to keep around).
/// Issue #176.
fn is_self_extracting_format(asset_name: &str) -> bool {
    let lower = asset_name.to_ascii_lowercase();
    lower.ends_with(".appimage") || (lower.contains("portable") && lower.ends_with(".zip"))
}

fn update_download_supported(install_format: &str) -> bool {
    matches!(
        install_format,
        "appimage" | "deb" | "rpm" | "msi" | "exe" | "portable" | "dmg"
    )
}

fn asset_matches_install_format(name: &str, install_format: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match install_format {
        "appimage" => lower.ends_with(".appimage"),
        "deb" => lower.ends_with(".deb"),
        "rpm" => lower.ends_with(".rpm"),
        "msi" => lower.ends_with(".msi"),
        // The NSIS installer is named `*_x64-setup.exe`. Anchor on
        // `-setup.exe` so we don't accidentally match the portable
        // .zip's inner name in any future pattern change.
        "exe" => {
            lower.ends_with("-setup.exe")
                || (lower.ends_with(".exe") && !lower.contains("portable"))
        }
        // Portable ships as `AeroFTP-<ver>-portable-windows-x64.zip`.
        // The "portable" + ".zip" pair is unambiguous.
        "portable" => lower.contains("portable") && lower.ends_with(".zip"),
        "dmg" => lower.ends_with(".dmg"),
        _ => false,
    }
}

fn asset_matches_current_arch(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.contains("universal") {
        return true;
    }

    let known_markers = [
        "x86_64", "amd64", "x64", "aarch64", "arm64", "armv7", "armhf", "i386", "i686", "x86",
    ];
    let has_arch_marker = known_markers.iter().any(|marker| lower.contains(marker));
    if !has_arch_marker {
        return true;
    }

    let expected_markers: &[&str] = match std::env::consts::ARCH {
        "x86_64" => &["x86_64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64", "universal"],
        "x86" => &["x86", "i386", "i686"],
        other => &[other],
    };

    expected_markers.iter().any(|marker| lower.contains(marker))
}

fn select_release_asset(
    release: &GitHubRelease,
    install_format: &str,
) -> Option<ReleaseAssetSelection> {
    if !update_download_supported(install_format) {
        return None;
    }

    let candidates: Vec<&GitHubAsset> = release
        .assets
        .iter()
        .filter(|asset| {
            !asset.name.ends_with(".sigstore.json")
                && asset_matches_install_format(&asset.name, install_format)
        })
        .collect();

    let asset = candidates
        .iter()
        .copied()
        .find(|asset| asset_matches_current_arch(&asset.name))
        .or_else(|| candidates.first().copied())?;

    let bundle_name = format!("{}.sigstore.json", asset.name);
    let bundle_asset = release
        .assets
        .iter()
        .find(|candidate| candidate.name == bundle_name)?;

    Some(ReleaseAssetSelection {
        tag: release.tag_name.clone(),
        asset_name: asset.name.clone(),
        download_url: asset.browser_download_url.clone(),
        bundle_url: bundle_asset.browser_download_url.clone(),
    })
}

fn unique_download_path(directory: &Path, file_name: &str) -> PathBuf {
    let base_path = directory.join(file_name);
    if !base_path.exists() {
        return base_path;
    }

    let file_path = Path::new(file_name);
    let stem = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("AeroFTP-update");
    let extension = file_path.extension().and_then(|s| s.to_str());

    for index in 1..1000 {
        let candidate_name = match extension {
            Some(ext) if !ext.is_empty() => format!("{}-{}.{}", stem, index, ext),
            _ => format!("{}-{}", stem, index),
        };
        let candidate_path = directory.join(candidate_name);
        if !candidate_path.exists() {
            return candidate_path;
        }
    }

    directory.join(format!("{}-{}", uuid::Uuid::new_v4(), file_name))
}

fn compute_update_download_progress(downloaded: u64, total: u64, completed: bool) -> u8 {
    if completed {
        return 100;
    }
    if total == 0 {
        return 0;
    }

    let raw = ((downloaded as f64 / total as f64) * 100.0).floor() as u8;
    raw.min(99)
}

fn emit_update_download_progress(
    app: &AppHandle,
    filename: &str,
    downloaded: u64,
    total: u64,
    started_at: Instant,
    completed: bool,
) {
    let elapsed = started_at.elapsed().as_secs_f64();
    let speed_bps = if elapsed > 0.0 {
        (downloaded as f64 / elapsed) as u64
    } else {
        0
    };
    let eta_seconds = if completed || speed_bps == 0 || total <= downloaded {
        0
    } else {
        (total - downloaded) / speed_bps
    };

    let _ = app.emit(
        "update-download-progress",
        UpdateDownloadProgress {
            downloaded,
            total,
            percentage: compute_update_download_progress(downloaded, total, completed),
            speed_bps,
            eta_seconds,
            filename: filename.to_string(),
        },
    );
}

fn parse_release_download_url(download_url: &str) -> Result<ReleaseAssetSelection, String> {
    let parsed =
        Url::parse(download_url).map_err(|error| format!("Invalid update URL: {}", error))?;

    if parsed.scheme() != "https" || parsed.host_str() != Some(GITHUB_RELEASES_HOST) {
        return Err("Update URL rejected: expected an HTTPS GitHub Releases URL".to_string());
    }

    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|parts| parts.collect())
        .ok_or_else(|| "Update URL rejected: malformed GitHub path".to_string())?;

    if segments.len() != 6
        || segments[0] != GITHUB_RELEASES_OWNER
        || segments[1] != GITHUB_RELEASES_REPO
        || segments[2] != "releases"
        || segments[3] != "download"
    {
        return Err("Update URL rejected: not an AeroFTP release artifact".to_string());
    }

    let tag = segments[4].to_string();
    let asset_name = segments[5].to_string();
    let file_name = Path::new(&asset_name)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Update URL rejected: invalid asset name".to_string())?;
    if file_name != asset_name {
        return Err("Update URL rejected: asset name traversal detected".to_string());
    }

    Ok(ReleaseAssetSelection {
        tag,
        asset_name: asset_name.clone(),
        download_url: download_url.to_string(),
        bundle_url: format!("{}.sigstore.json", download_url),
    })
}

async fn download_optional_file_to_path(
    client: &HttpClient,
    url: &str,
    destination: &Path,
    user_agent: &str,
) -> Result<bool, String> {
    let response = client
        .get(url)
        .header("User-Agent", user_agent)
        .send()
        .await
        .map_err(|error| format!("Failed to download {}: {}", url, error))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }

    if !response.status().is_success() {
        return Err(format!(
            "Download failed for {}: HTTP {}",
            url,
            response.status()
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Failed to read {}: {}", url, error))?;

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .await
        .map_err(|error| format!("Failed to create {}: {}", destination.display(), error))?;
    file.write_all(&bytes)
        .await
        .map_err(|error| format!("Failed to write {}: {}", destination.display(), error))?;
    file.flush()
        .await
        .map_err(|error| format!("Failed to flush {}: {}", destination.display(), error))?;

    Ok(true)
}

async fn download_update_artifact(
    app: &AppHandle,
    client: &HttpClient,
    url: &str,
    destination: &Path,
    filename: &str,
) -> Result<(), String> {
    let response = client
        .get(url)
        .header("User-Agent", "AeroFTP")
        .send()
        .await
        .map_err(|error| format!("Failed to start update download: {}", error))?;

    if !response.status().is_success() {
        return Err(format!(
            "Update download failed: HTTP {}",
            response.status()
        ));
    }

    let total = response.content_length().unwrap_or(0);
    let mut stream = response.bytes_stream();
    // Open exclusively: unique_download_path picks a friendly candidate, but
    // create_new is the actual TOCTOU/symlink boundary if another process
    // creates that path between selection and open.
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .await
        .map_err(|error| format!("Failed to create update file: {}", error))?;

    let started_at = Instant::now();
    let mut last_emit = Instant::now();
    let mut last_percentage = 0u8;
    let mut downloaded = 0u64;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("Failed while downloading update: {}", error))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Failed to write update file: {}", error))?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);

        let percentage = compute_update_download_progress(downloaded, total, false);
        let should_emit = last_emit.elapsed().as_millis() >= 150
            || percentage.saturating_sub(last_percentage) >= 2;
        if should_emit {
            emit_update_download_progress(app, filename, downloaded, total, started_at, false);
            last_emit = Instant::now();
            last_percentage = percentage;
        }
    }

    file.flush()
        .await
        .map_err(|error| format!("Failed to flush update file: {}", error))?;

    Ok(())
}

#[derive(Serialize, Clone)]
enum VerificationMode {
    SigstoreVerified,
    VerificationUnavailable,
    #[allow(dead_code)]
    VerificationFailed,
}

#[derive(Serialize, Clone)]
struct UpdateVerificationInfo {
    mode: VerificationMode,
    workflow_identity: Option<String>,
    oidc_issuer: Option<String>,
    artifact_sha256: String,
    bundle_present: bool,
    bundle_parsed: bool,
    bundle_fetch_failed: bool,
    message: String,
}

#[derive(Serialize, Clone)]
struct DownloadUpdateResponse {
    path: String,
    verification: UpdateVerificationInfo,
}

/// Verify a Sigstore bundle against a downloaded artifact.
///
/// # Return value contract
/// Returns `Ok(UpdateVerificationInfo)` for ALL verification outcomes, including
/// `VerificationMode::VerificationFailed`. This is intentional: callers MUST inspect
/// `.mode` on the returned value to distinguish success from failure. The `Err` variant
/// is reserved for infrastructure errors (e.g. unable to open the artifact file or
/// initialize the Sigstore trust root) that prevent verification from even being attempted.
///
/// The `download_update` caller relies on this contract to delete the artifact and
/// return a user-facing error when `mode == VerificationFailed`.
fn verify_sigstore_bundle(
    artifact_path: &Path,
    bundle_path: &Path,
    tag: &str,
) -> Result<UpdateVerificationInfo, String> {
    let artifact_sha256 = sha256_file_hex(artifact_path).unwrap_or_else(|_| "unknown".to_string());

    let bundle_file = match std::fs::File::open(bundle_path) {
        Ok(f) => f,
        Err(_) => {
            return Ok(UpdateVerificationInfo {
                mode: VerificationMode::VerificationUnavailable,
                workflow_identity: None,
                oidc_issuer: None,
                artifact_sha256,
                bundle_present: false,
                bundle_parsed: false,
                bundle_fetch_failed: false,
                message: "Sigstore bundle not found on GitHub Release".to_string(),
            });
        }
    };

    let bundle: sigstore::bundle::Bundle = match serde_json::from_reader(bundle_file) {
        Ok(b) => b,
        Err(e) => {
            return Ok(UpdateVerificationInfo {
                mode: VerificationMode::VerificationUnavailable,
                workflow_identity: None,
                oidc_issuer: None,
                artifact_sha256,
                bundle_present: true,
                bundle_parsed: false,
                bundle_fetch_failed: false,
                message: format!("Sigstore bundle unparseable: {}", e),
            });
        }
    };

    let mut artifact_file = std::fs::File::open(artifact_path)
        .map_err(|error| format!("Failed to open downloaded artifact: {}", error))?;

    let verifier = SigstoreVerifier::production()
        .map_err(|error| format!("Failed to initialize Sigstore trust root: {}", error))?;
    let identity = format!("{}{}", SIGSTORE_WORKFLOW_IDENTITY_PREFIX, tag);
    let verification_policy = policy::Identity::new(identity.clone(), SIGSTORE_OIDC_ISSUER);

    match verifier.verify(&mut artifact_file, bundle, &verification_policy, true) {
        Ok(_) => Ok(UpdateVerificationInfo {
            mode: VerificationMode::SigstoreVerified,
            workflow_identity: Some(identity),
            oidc_issuer: Some(SIGSTORE_OIDC_ISSUER.to_string()),
            artifact_sha256,
            bundle_present: true,
            bundle_parsed: true,
            bundle_fetch_failed: false,
            message: "Successfully verified against GitHub Actions Sigstore transparency log"
                .to_string(),
        }),
        Err(e) => {
            // Sigstore verification errors should NEVER block the user from installing.
            // The artifact is already downloaded and SHA256-verified. Sigstore is a supply-chain
            // transparency bonus, not a gate. Treat all verification errors as non-blocking.
            Ok(UpdateVerificationInfo {
                mode: VerificationMode::VerificationUnavailable,
                workflow_identity: Some(identity),
                oidc_issuer: Some(SIGSTORE_OIDC_ISSUER.to_string()),
                artifact_sha256,
                bundle_present: true,
                bundle_parsed: true,
                bundle_fetch_failed: false,
                message: format!("Signature verification unavailable: {}", e),
            })
        }
    }
}

/// True on Arch and its pacman-based derivatives. `/etc/arch-release` covers
/// Arch, Manjaro and EndeavourOS; `/etc/pacman.conf` also catches Artix and
/// Parabola, which do not ship the former. Debian and Fedora have neither.
///
/// Takes the existence check as a parameter so the branch is unit-testable
/// without touching the real filesystem.
fn is_pacman_system(path_exists: impl Fn(&str) -> bool) -> bool {
    path_exists("/etc/arch-release") || path_exists("/etc/pacman.conf")
}

/// Detect how the app was installed (deb, appimage, snap, flatpak, rpm, exe, dmg)
fn detect_install_format() -> String {
    let os = std::env::consts::OS;

    match os {
        "linux" => {
            // Check for Snap
            if std::env::var("SNAP").is_ok() {
                return "snap".to_string();
            }
            // Check for Flatpak
            if std::env::var("FLATPAK_ID").is_ok() {
                return "flatpak".to_string();
            }
            // Check for AppImage - the executable path contains "AppImage"
            if let Ok(exe_path) = std::env::current_exe() {
                let path_str = exe_path.to_string_lossy();
                if path_str.contains("AppImage") || path_str.contains(".AppImage") {
                    return "appimage".to_string();
                }
            }
            // Check for RPM-based distros (Fedora, CentOS, RHEL)
            if std::path::Path::new("/etc/redhat-release").exists()
                || std::path::Path::new("/etc/fedora-release").exists()
            {
                return "rpm".to_string();
            }
            // Check for Arch and its derivatives (Manjaro, EndeavourOS, Artix).
            // The AUR package is installed by pacman, and the in-app updater can
            // only install a .deb through pkexec. Report a format that
            // `update_download_supported` excludes so the updater degrades to
            // notify-only, exactly as it already does for Snap and Flatpak.
            if is_pacman_system(|path| std::path::Path::new(path).exists()) {
                return "pacman".to_string();
            }
            // Default to DEB for Debian/Ubuntu based
            "deb".to_string()
        }
        "windows" => portable::detect_windows_install_format(),
        "macos" => "dmg".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Surface portable-mode state to the frontend.
///
/// Used by the IntroHub first-run banner to explain to portable users
/// that their data lives in `<exe-dir>/data/...` and that, starting from
/// v3.7.8, every portable folder is an isolated universe (WebView2 state
/// included). When `shared_legacy_present` is true, there is a system-wide
/// `%LOCALAPPDATA%\com.aeroftp.AeroFTP\EBWebView\` folder on disk left
/// over from an earlier non-portable install (or from a pre-v3.7.8
/// portable that shared that folder): the banner offers a one-click
/// "Open folder" / "Dismiss" so the user can clean it up manually.
#[derive(serde::Serialize)]
struct PortableInfo {
    is_portable: bool,
    data_root: Option<String>,
    webview_data_dir: Option<String>,
    credential_store_dir: Option<String>,
    shared_legacy_present: bool,
}

#[tauri::command]
fn portable_info(app: tauri::AppHandle) -> PortableInfo {
    let is_portable = portable::is_portable();
    let data_root = if is_portable {
        portable::app_data_dir(&app)
            .ok()
            .map(|p| p.display().to_string())
    } else {
        None
    };
    PortableInfo {
        is_portable,
        data_root,
        webview_data_dir: portable::webview_data_dir().map(|p| p.display().to_string()),
        credential_store_dir: portable::credential_store_dir().map(|p| p.display().to_string()),
        shared_legacy_present: portable::shared_webview_data_present(),
    }
}

#[tauri::command]
fn copy_to_clipboard(text: String) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Clipboard init failed: {}", e))?;
    #[cfg(target_os = "linux")]
    {
        // On Linux/X11, spawn the clipboard operation in a separate thread.
        // Using .wait() blocks until a clipboard manager reads the content,
        // which can hang indefinitely if no manager is active.
        // We spawn a detached thread to handle this without blocking the UI.
        use arboard::SetExtLinux;
        let text_clone = text.clone();
        std::thread::spawn(move || {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set().wait().text(text_clone);
            }
        });
        // Also set without wait as immediate fallback
        clipboard
            .set_text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    #[cfg(target_os = "windows")]
    {
        // On Windows, spawn clipboard write in a separate thread to avoid
        // potential UI freeze when Credential Manager or Windows Hello is active
        let text_clone = text.clone();
        std::thread::spawn(move || {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(text_clone);
            }
        });
        clipboard
            .set_text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        clipboard
            .set_text(text)
            .map_err(|e| format!("Clipboard write failed: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
async fn resolve_hostname(hostname: String, port: u16) -> Result<String, String> {
    let addr = format!("{}:{}", hostname, port);
    let mut addrs = tokio::net::lookup_host(&addr)
        .await
        .map_err(|e| format!("DNS resolution failed: {}", e))?;
    addrs
        .next()
        .map(|a| a.ip().to_string())
        .ok_or_else(|| "No addresses found".to_string())
}

#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<UpdateInfo, String> {
    let current_version = app.package_info().version.to_string();
    let install_format = detect_install_format();

    info!(
        "Checking for updates... Current: v{}, Format: {}",
        current_version, install_format
    );

    let client = HttpClient::new();

    let response = client
        .get(GITHUB_RELEASES_API_URL)
        .header("User-Agent", "AeroFTP")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("GitHub API error: {}", response.status()));
    }

    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse release info: {}", e))?;

    // Parse versions (remove 'v' prefix if present)
    let latest_tag = release.tag_name.trim_start_matches('v');
    let current = Version::parse(&current_version)
        .map_err(|e| format!("Failed to parse current version: {}", e))?;
    let latest =
        Version::parse(latest_tag).map_err(|e| format!("Failed to parse latest version: {}", e))?;

    if latest > current {
        if let Some(asset) = select_release_asset(&release, &install_format) {
            info!(
                "Update v{} available with signed asset {} for format {}",
                latest_tag, asset.asset_name, install_format
            );

            return Ok(UpdateInfo {
                has_update: true,
                latest_version: Some(latest_tag.to_string()),
                download_url: Some(asset.download_url),
                current_version: current_version.clone(),
                install_format,
            });
        }

        if update_download_supported(&install_format) {
            info!(
                "Update v{} released, but no signed asset pair is ready for format {}",
                latest_tag, install_format
            );
        } else {
            info!(
                "Update v{} exists, but install format {} is not handled by the in-app updater",
                latest_tag, install_format
            );
        }

        return Ok(UpdateInfo {
            has_update: false,
            latest_version: Some(latest_tag.to_string()),
            download_url: None,
            current_version: current_version.clone(),
            install_format,
        });
    }

    info!(
        "No update available. Current: v{}, Latest: v{}",
        current_version, latest_tag
    );

    Ok(UpdateInfo {
        has_update: false,
        latest_version: Some(latest_tag.to_string()),
        download_url: None,
        current_version,
        install_format,
    })
}

#[tauri::command]
fn log_update_detection(version: String) {
    info!("New version detected: v{}", version);
}

/// A8-03: Validate that update file path is in Downloads, temp directory, or
/// the AeroFTP staging dir (issue #176: portable .zip / .AppImage live there
/// instead of `~/Downloads/`).
#[allow(dead_code)]
fn validate_update_path(path: &str) -> Result<(), String> {
    let canonical = std::path::Path::new(path)
        .canonicalize()
        .map_err(|e| format!("Invalid update path: {}", e))?;

    let allowed_dirs: Vec<std::path::PathBuf> = vec![
        dirs::download_dir().unwrap_or_default(),
        dirs::home_dir()
            .map(|h| h.join("Downloads"))
            .unwrap_or_default(),
        std::env::temp_dir(),
        updates_staging_dir(),
    ];

    // Component-aware containment: `Path::starts_with` compares whole path
    // components, so a sibling directory that merely shares a string prefix
    // (e.g. `/tmproot` vs `/tmp`) is correctly rejected, unlike a raw string
    // `starts_with`.
    let in_allowed = allowed_dirs.iter().any(|dir| {
        if dir.as_os_str().is_empty() {
            return false;
        }
        match dir.canonicalize() {
            Ok(canon_dir) => canonical.starts_with(&canon_dir),
            Err(_) => false,
        }
    });

    if !in_allowed {
        return Err("Update path rejected: must be in Downloads or temp directory".to_string());
    }
    Ok(())
}

fn sha256_file_hex(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("Failed to read file for SHA-256: {}", error))?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("{:x}", digest))
}

/// Registry of update artifacts this process has itself downloaded AND
/// Sigstore-verified, keyed by canonical path with the verified SHA-256.
///
/// The privileged install commands (`install_deb_update` / `install_rpm_update`
/// / `install_appimage_update`) re-accept a caller-supplied `downloaded_path`,
/// so a compromised webview could otherwise steer them at an arbitrary
/// attacker-staged file (confused deputy: pkexec install as root, or overwrite
/// of the running executable). Installs are gated on membership here plus an
/// on-disk digest match, so a privileged install can only ever run against
/// bytes this process just verified, never a path the caller merely asserts.
fn verified_update_registry(
) -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, String>> {
    static REGISTRY: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, String>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    &REGISTRY
}

fn record_verified_update(path: &Path, sha256_hex: String) {
    if let Ok(canonical) = path.canonicalize() {
        if let Ok(mut registry) = verified_update_registry().lock() {
            registry.insert(canonical, sha256_hex);
        }
    }
}

/// Fail closed unless `downloaded_path` is an artifact this process downloaded
/// and Sigstore-verified, and its bytes are still identical to what was
/// verified. Returns the verified SHA-256 (hex) so callers can hand the helper
/// the verified digest rather than a fresh self-computed one.
fn ensure_update_artifact_verified(downloaded_path: &str) -> Result<String, String> {
    let canonical = Path::new(downloaded_path)
        .canonicalize()
        .map_err(|error| format!("Invalid update path: {}", error))?;
    let expected_hex = verified_update_registry()
        .lock()
        .map_err(|_| "Update verification registry unavailable".to_string())?
        .get(&canonical)
        .cloned();
    let Some(expected_hex) = expected_hex else {
        return Err(
            "Refusing to install: this artifact was not downloaded and verified by AeroFTP in this session".to_string(),
        );
    };
    let actual_hex = sha256_file_hex(&canonical)?;
    if actual_hex != expected_hex {
        return Err(
            "Refusing to install: the update artifact changed on disk after verification"
                .to_string(),
        );
    }
    Ok(expected_hex)
}

/// Download an update file with progress events
#[tauri::command]
async fn download_update(app: AppHandle, url: String) -> Result<DownloadUpdateResponse, String> {
    let asset = parse_release_download_url(&url)?;

    // Issue #176: portable .zip and .AppImage stage into a private cache
    // directory rather than `~/Downloads/`. They have no independent life
    // once the swap is done (the new exe is already living in its install
    // location), so leaving them in Downloads erodes the no-trace contract
    // of "portable" and adds clutter for AppImage users. Installer formats
    // (msi, exe, deb, rpm, dmg) keep `~/Downloads/`: they ARE the artifact,
    // useful to re-run, copy to USB, hand to a colleague.
    let download_directory = if is_self_extracting_format(&asset.asset_name) {
        updates_staging_dir()
    } else {
        dirs::download_dir().unwrap_or_else(std::env::temp_dir)
    };
    tokio::fs::create_dir_all(&download_directory)
        .await
        .map_err(|error| format!("Failed to prepare download directory: {}", error))?;

    let destination = unique_download_path(&download_directory, &asset.asset_name);
    let bundle_file_name = format!(
        "{}.sigstore.json",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&asset.asset_name)
    );
    let bundle_path = destination.with_file_name(bundle_file_name);
    let client = HttpClient::new();

    download_update_artifact(
        &app,
        &client,
        &asset.download_url,
        &destination,
        &asset.asset_name,
    )
    .await?;
    // A missing optional bundle (404) is a legitimate SHA-only release. Any
    // other fetch failure must remain visible to the verifier/UI instead of
    // masquerading as "no signature published".
    let bundle_fetch_error =
        download_optional_file_to_path(&client, &asset.bundle_url, &bundle_path, "AeroFTP")
            .await
            .err();

    let verify_destination = destination.clone();
    let verify_bundle = bundle_path.clone();
    let verify_tag = asset.tag.clone();
    let mut verification_info = tokio::task::spawn_blocking(move || {
        verify_sigstore_bundle(&verify_destination, &verify_bundle, &verify_tag)
    })
    .await
    .map_err(|error| format!("Sigstore verification task failed: {}", error))??;

    if let Some(error) = bundle_fetch_error {
        verification_info.bundle_fetch_failed = true;
        verification_info.message = format!("Sigstore bundle download failed: {error}");
    }

    if matches!(verification_info.mode, VerificationMode::VerificationFailed) {
        let _ = tokio::fs::remove_file(&destination).await;
        let _ = tokio::fs::remove_file(&bundle_path).await;
        return Err(verification_info.message);
    }

    validate_update_path(destination.to_string_lossy().as_ref())?;
    emit_update_download_progress(&app, &asset.asset_name, 1, 1, Instant::now(), true);

    // Record the just-verified artifact so the privileged install commands can
    // fail closed against a caller-supplied path (see verified_update_registry).
    // Hash with sha256_file_hex so the recorded digest matches exactly what the
    // install-time re-check computes.
    let recorded_digest = sha256_file_hex(&destination)?;
    record_verified_update(&destination, recorded_digest);

    let _ = tokio::fs::remove_file(&bundle_path).await;

    Ok(DownloadUpdateResponse {
        path: destination.to_string_lossy().to_string(),
        verification: verification_info,
    })
}

/// Spawn a fully detached relaunch process using setsid.
/// The child runs in its own session so it survives when the parent exits.
/// Uses direct exec (no shell) to prevent shell injection via exe_path.
#[cfg(unix)]
#[allow(dead_code)]
fn spawn_detached_relaunch(exe_path: &str) {
    let helper = std::path::Path::new("/usr/lib/aeroftp/aeroftp-restart-helper");
    let parent_pid = std::process::id().to_string();

    if helper.exists() {
        // Preferred: external helper survives parent exit
        match std::process::Command::new("setsid")
            .arg("--fork")
            .arg(helper)
            .arg(exe_path)
            .arg(&parent_pid)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => {
                tracing::info!("Restart helper spawned for PID {}", parent_pid);
                return;
            }
            Err(e) => {
                tracing::warn!("Restart helper failed: {}, falling back to direct spawn", e);
            }
        }
    }

    // Fallback: inline PID-polling via sh (same logic as helper script).
    // Waits until parent PID exits, then relaunches. Works on fast and slow PCs.
    // Arguments passed via $0/$1 to prevent shell injection.
    let script = r#"i=0; while kill -0 "$1" 2>/dev/null; do sleep 1; i=$((i+1)); [ "$i" -ge 60 ] && exit 1; done; sleep 3; exec "$0""#;

    // Try setsid --fork first (fully detached from parent session)
    if std::process::Command::new("setsid")
        .arg("--fork")
        .arg("sh")
        .arg("-c")
        .arg(script)
        .arg(exe_path)
        .arg(&parent_pid)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
    {
        tracing::info!("Inline restart (setsid) spawned for PID {}", parent_pid);
        return;
    }

    // Last resort: pre_exec with libc::setsid()
    use std::os::unix::process::CommandExt;
    let exe_owned = exe_path.to_string();
    let pid_owned = parent_pid.clone();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .arg(&exe_owned)
        .arg(&pid_owned)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_) => tracing::info!("Inline restart (pre_exec) spawned for PID {}", parent_pid),
        Err(e) => tracing::warn!("Failed to spawn relaunch: {}", e),
    }
}

fn write_update_marker(
    app: &AppHandle,
    from: &str,
    to: &str,
    format: &str,
    verification_mode: &str,
) {
    let verified =
        verification_mode == "SigstoreVerified" || verification_mode == "VerificationUnavailable";
    if let Ok(config_dir) = portable::app_config_dir(app) {
        let marker = config_dir.join("last-update.json");
        let data = serde_json::json!({
            "updated_from": from,
            "updated_to": to,
            "install_format": format,
            "verified": verified,
            "verification_mode": verification_mode,
            "timestamp": chrono::Utc::now().to_rfc3339()
        });
        let _ = std::fs::write(&marker, data.to_string());
    }
}

#[tauri::command]
async fn read_update_marker(app: AppHandle) -> Result<Option<String>, String> {
    if let Ok(config_dir) = portable::app_config_dir(&app) {
        let marker = config_dir.join("last-update.json");
        if marker.exists() {
            return std::fs::read_to_string(&marker)
                .map(Some)
                .map_err(|e| e.to_string());
        }
    }
    Ok(None)
}

#[tauri::command]
async fn clear_update_marker(app: AppHandle) -> Result<(), String> {
    if let Ok(config_dir) = portable::app_config_dir(&app) {
        let marker = config_dir.join("last-update.json");
        if marker.exists() {
            let _ = std::fs::remove_file(marker);
        }
    }
    Ok(())
}

/// Replace current AppImage with downloaded update and restart
#[tauri::command]
async fn install_appimage_update(
    app: AppHandle,
    downloaded_path: String,
    verification_mode: String,
) -> Result<(), String> {
    validate_update_path(&downloaded_path)?;
    // Fail closed: only install an artifact this process downloaded and verified,
    // with bytes unchanged since verification. Neutralizes a confused-deputy
    // install of an attacker-staged AppImage over the running executable.
    ensure_update_artifact_verified(&downloaded_path)?;

    let downloaded = PathBuf::from(&downloaded_path);
    if !downloaded.exists() {
        return Err("Downloaded AppImage not found".to_string());
    }

    let current_exe = std::env::current_exe()
        .map_err(|error| format!("Failed to resolve current AppImage path: {}", error))?;
    let current_name = current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Failed to resolve current executable name".to_string())?;
    if !current_name.to_ascii_lowercase().contains("appimage") {
        return Err("Current executable is not an AppImage path".to_string());
    }

    let current_parent = current_exe
        .parent()
        .ok_or_else(|| "Failed to resolve AppImage directory".to_string())?;
    let staged_path = current_parent.join(format!(".{}.update", current_name));
    let backup_path = current_parent.join(format!(".{}.backup", current_name));

    if backup_path.exists() {
        std::fs::remove_file(&backup_path)
            .map_err(|error| format!("Failed to remove stale AppImage backup: {}", error))?;
    }

    std::fs::copy(&downloaded, &staged_path)
        .map_err(|error| format!("Failed to stage AppImage update: {}", error))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&staged_path, permissions)
            .map_err(|error| format!("Failed to set AppImage permissions: {}", error))?;
    }

    std::fs::rename(&current_exe, &backup_path)
        .map_err(|error| format!("Failed to move current AppImage aside: {}", error))?;

    if let Err(error) = std::fs::rename(&staged_path, &current_exe) {
        let _ = std::fs::rename(&backup_path, &current_exe);
        return Err(format!("Failed to install new AppImage: {}", error));
    }

    let _ = std::fs::remove_file(&backup_path);

    // Issue #176: the downloaded AppImage was copied (not moved) into the
    // running install location, so the source in our staging dir is now
    // an unused archive. Drop it. Best-effort: a leftover file is not a
    // user-visible bug since the staging dir is hidden under the cache.
    let _ = std::fs::remove_file(&downloaded);

    let from_version = app.package_info().version.to_string();
    write_update_marker(
        &app,
        &from_version,
        "unknown",
        "appimage",
        &verification_mode,
    );
    let _ = app.emit("update_install_phase", "restart");

    // Release DBus single-instance lock before restart to prevent race condition
    tauri_plugin_single_instance::destroy(&app);
    app.restart();
}

/// F-012 W1/W4: restart AeroFTP on request. A keystore import that touched the
/// on-disk SQLite databases (user partitions, chat history, file tags, plugins)
/// leaves the running app holding stale long-lived connections, so the imported
/// state only takes effect after a relaunch. Also used by the manual "Restart
/// AeroFTP" button and after a multi-user partition rebuild. Releases the
/// single-instance lock first so the relaunch is not rejected as a duplicate.
#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    tauri_plugin_single_instance::destroy(&app);
    app.restart();
}

/// Install a .deb package via pkexec with branded Polkit dialog and restart the app.
/// Uses /usr/lib/aeroftp/aeroftp-update-helper (installed by .deb) for branded auth dialog.
/// Falls back to generic `pkexec dpkg -i` if helper is not found.
#[tauri::command]
async fn install_deb_update(
    app: AppHandle,
    downloaded_path: String,
    verification_mode: String,
) -> Result<(), String> {
    validate_update_path(&downloaded_path)?;
    if !downloaded_path.to_ascii_lowercase().ends_with(".deb") {
        return Err("Downloaded update is not a .deb package".to_string());
    }

    let helper = Path::new("/usr/lib/aeroftp/aeroftp-update-helper");
    if !helper.exists() {
        return Err("Secure update helper not found; aborting privileged install".to_string());
    }

    // Fail closed AND derive the helper's hash from the verified artifact: only
    // install bytes this process downloaded and Sigstore-verified, and pass the
    // verified digest to pkexec (not a fresh self-computed hash that proves
    // nothing about authenticity).
    let package_hash = ensure_update_artifact_verified(&downloaded_path)?;
    let _ = app.emit("update_install_phase", "auth");
    let status = tokio::process::Command::new("pkexec")
        .arg(helper)
        .arg(&downloaded_path)
        .arg(&package_hash)
        .status()
        .await
        .map_err(|error| format!("Failed to launch AeroFTP update helper: {}", error))?;

    if !status.success() {
        return Err(format!(
            ".deb installation failed with exit status {:?}",
            status.code()
        ));
    }

    let from_version = app.package_info().version.to_string();
    write_update_marker(&app, &from_version, "unknown", "deb", &verification_mode);
    let _ = app.emit("update_install_phase", "restart");

    // Release DBus single-instance lock before restart to prevent race condition
    tauri_plugin_single_instance::destroy(&app);
    app.restart();
}

/// Install an .rpm package via pkexec with branded Polkit dialog and restart the app.
/// Same helper/fallback pattern as install_deb_update.
#[tauri::command]
async fn install_rpm_update(
    app: AppHandle,
    downloaded_path: String,
    verification_mode: String,
) -> Result<(), String> {
    validate_update_path(&downloaded_path)?;
    if !downloaded_path.to_ascii_lowercase().ends_with(".rpm") {
        return Err("Downloaded update is not an .rpm package".to_string());
    }

    let helper = Path::new("/usr/lib/aeroftp/aeroftp-update-helper");
    if !helper.exists() {
        return Err("Secure update helper not found; aborting privileged install".to_string());
    }

    // Fail closed AND derive the helper's hash from the verified artifact: only
    // install bytes this process downloaded and Sigstore-verified, and pass the
    // verified digest to pkexec (not a fresh self-computed hash that proves
    // nothing about authenticity).
    let package_hash = ensure_update_artifact_verified(&downloaded_path)?;
    let _ = app.emit("update_install_phase", "auth");
    let status = tokio::process::Command::new("pkexec")
        .arg(helper)
        .arg(&downloaded_path)
        .arg(&package_hash)
        .status()
        .await
        .map_err(|error| format!("Failed to launch AeroFTP update helper: {}", error))?;

    if !status.success() {
        return Err(format!(
            ".rpm installation failed with exit status {:?}",
            status.code()
        ));
    }

    let from_version = app.package_info().version.to_string();
    write_update_marker(&app, &from_version, "unknown", "rpm", &verification_mode);
    let _ = app.emit("update_install_phase", "restart");

    // Release DBus single-instance lock before restart to prevent race condition
    tauri_plugin_single_instance::destroy(&app);
    app.restart();
}

/// Install a Windows update artifact (.msi, .exe, or portable .zip) silently
/// and restart AeroFTP from the updated location.
///
/// Dispatch is deterministic from the install_format detected at startup
/// (see `portable::detect_windows_install_format`), but as a defensive
/// guard we also classify by extension here. Three paths:
///
///   - .msi: silent upgrade via `msiexec /i ... /qb /norestart`. The
///     install location is unchanged (MSI overwrites in place); the
///     helper script relaunches the same exe path after install.
///
///   - .exe (NSIS setup): silent install via `setup.exe /S`. The Tauri
///     NSIS template + our `installer/hooks.nsh` handle silent mode
///     correctly (PATH registration, .aerovault association, VC++
///     runtime check, no UAC re-prompts on per-user installs).
///
///   - .zip (portable): extract into `%TEMP%`, helper script renames
///     the running exe to `*.old`, swaps in the new exe, copies the
///     marker/README/LICENSE, relaunches with `--post-update-cleanup`.
///
/// All three paths use a transient `.cmd` helper in `%TEMP%` spawned
/// detached (`CREATE_NO_WINDOW | DETACHED_PROCESS`); the helper waits
/// 2s for AeroFTP to exit, runs the install, relaunches, and self-deletes.
#[tauri::command]
async fn install_windows_update(
    #[allow(unused_variables)] app: AppHandle,
    #[allow(unused_variables)] downloaded_path: String,
    #[allow(unused_variables)] verification_mode: String,
) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        Err("Windows installer is only available on Windows".to_string())
    }

    #[cfg(windows)]
    {
        let downloaded = std::path::Path::new(&downloaded_path);
        if !downloaded.exists() {
            return Err("Downloaded file not found".to_string());
        }

        let ext = downloaded
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let format = match ext.as_str() {
            "msi" => "msi",
            "exe" => "exe",
            "zip" => "portable",
            other => return Err(format!("Unknown Windows update format: .{other}")),
        };

        info!("Installing Windows update: format={format}, path={downloaded_path}");

        windows_update_helper::install_with_helper(&app, format, downloaded)?;

        let from_version = app.package_info().version.to_string();
        write_update_marker(&app, &from_version, "unknown", format, &verification_mode);

        // Give the helper script time to spawn before we exit. The script
        // pings 127.0.0.1 -n 3 (~2s) before doing anything destructive, so
        // 500ms here is comfortably ahead of it.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        app.exit(0);

        Ok(())
    }
}

// ============ FTP Commands ============

// IPC panic safety net: the real work runs in `connect_ftp_inner`, wrapped by
// `panic_safe::catch` so a panic on the connect/login path (a rustls provider
// panic, a suppaftp bug, ...) becomes an `Err` the UI renders instead of a
// promise that hangs forever. See `panic_safe`.
#[tauri::command]
async fn connect_ftp(
    state: State<'_, AppState>,
    cancel_registry: State<'_, provider_commands::ConnectionCancelRegistry>,
    params: ConnectionParams,
) -> Result<(), String> {
    panic_safe::catch(
        "connect_ftp",
        connect_ftp_inner(state, cancel_registry, params),
    )
    .await
}

async fn connect_ftp_inner(
    state: State<'_, AppState>,
    cancel_registry: State<'_, provider_commands::ConnectionCancelRegistry>,
    params: ConnectionParams,
) -> Result<(), String> {
    info!("Connecting to FTP server: {}", params.server);

    // W3.1 (#270.5): register a cancellation token under the frontend-supplied
    // connect token so an Esc / "still connecting" Cancel can abort the connect
    // (and the slow AUTH TLS handshake on login). The guard de-registers it on
    // every exit path. suppaftp's connect/login are async, so dropping the
    // future on cancel tears the in-flight TCP/TLS handshake down cleanly.
    let connect_key = params.connect_token.clone();
    let cancel_token = connect_key
        .as_deref()
        .map(|key| cancel_registry.register(key));
    let _cancel_guard = connect_key
        .as_deref()
        .map(|key| provider_commands::ConnectTokenGuard::new(&cancel_registry, key.to_string()));

    let mut ftp_manager = state.ftp_manager.lock().await;

    let do_connect = async {
        ftp_manager
            .connect(&params.server)
            .await
            .map_err(|e| format!("Connection failed: {}", e))?;

        ftp_manager
            .login(&params.username, &params.password)
            .await
            .map_err(|e| format!("Login failed: {}", e))?;

        Ok::<(), String>(())
    };

    match cancel_token.as_ref() {
        Some(token) => tokio::select! {
            res = do_connect => res,
            _ = token.cancelled() => Err(provider_commands::CONNECT_CANCELLED.to_string()),
        },
        None => do_connect.await,
    }
}

#[tauri::command]
async fn disconnect_ftp(state: State<'_, AppState>) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager
        .disconnect()
        .await
        .map_err(|e| format!("Disconnect failed: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn check_connection(state: State<'_, AppState>) -> Result<bool, String> {
    let ftp_manager = state.ftp_manager.lock().await;
    Ok(ftp_manager.is_connected())
}

#[tauri::command]
async fn ftp_noop(state: State<'_, AppState>) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager
        .noop()
        .await
        .map_err(|e| format!("NOOP failed: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn reconnect_ftp(state: State<'_, AppState>) -> Result<(), String> {
    info!("Attempting FTP reconnection");
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager
        .reconnect()
        .await
        .map_err(|e| format!("Reconnection failed: {}", e))?;
    info!("FTP reconnection successful");
    Ok(())
}

#[tauri::command]
async fn list_files(state: State<'_, AppState>) -> Result<FileListResponse, String> {
    let mut ftp_manager = state.ftp_manager.lock().await;

    let files = ftp_manager
        .list_files()
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;

    let current_path = ftp_manager.current_path();

    Ok(FileListResponse {
        files,
        current_path,
    })
}

#[tauri::command]
async fn change_directory(
    state: State<'_, AppState>,
    path: String,
) -> Result<FileListResponse, String> {
    let mut ftp_manager = state.ftp_manager.lock().await;

    ftp_manager
        .change_dir(&path)
        .await
        .map_err(|e| format!("Failed to change directory: {}", e))?;

    let files = ftp_manager
        .list_files()
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;

    let current_path = ftp_manager.current_path();

    Ok(FileListResponse {
        files,
        current_path,
    })
}

// ============ Transfer Commands with Progress ============

#[tauri::command]
async fn download_file(
    app: AppHandle,
    state: State<'_, AppState>,
    params: DownloadParams,
) -> Result<String, String> {
    // Check if already cancelled (batch stop): bail immediately
    if state.cancel_flag.load(Ordering::Relaxed) {
        return Err("Transfer cancelled by user".to_string());
    }

    let cancel_flag = state.cancel_flag.clone();
    let filename = PathBuf::from(&params.remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let transfer_id = format!("dl-{}", chrono::Utc::now().timestamp_millis());

    // Emit start event
    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: filename.clone(),
            direction: "download".to_string(),
            message: Some(format!("Starting download: {}", filename)),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let mut ftp_manager = state.ftp_manager.lock().await;

    // Get file size first
    let file_size = ftp_manager
        .get_file_size(&params.remote_path)
        .await
        .unwrap_or(0);

    let start_time = Instant::now();
    let mut last_emit_time = Instant::now();
    let mut last_emit_pct = 0u8;

    // Download with progress (throttled: emit every 150ms or 2% delta)
    match ftp_manager
        .download_file_with_progress(&params.remote_path, &params.local_path, |transferred| {
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                (transferred as f64 / elapsed) as u64
            } else {
                0
            };
            let percentage = if file_size > 0 {
                ((transferred as f64 / file_size as f64) * 100.0) as u8
            } else {
                0
            };

            let is_complete = transferred >= file_size && file_size > 0;
            let time_delta = last_emit_time.elapsed().as_millis() >= 150;
            let pct_delta = percentage.saturating_sub(last_emit_pct) >= 2;
            if time_delta || pct_delta || is_complete {
                last_emit_time = Instant::now();
                last_emit_pct = percentage;
                let eta = if speed > 0 && file_size > transferred {
                    ((file_size - transferred) / speed) as u32
                } else {
                    0
                };

                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "progress".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        direction: "download".to_string(),
                        message: None,
                        progress: Some(TransferProgress {
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            transferred,
                            total: file_size,
                            percentage,
                            speed_bps: speed,
                            eta_seconds: eta,
                            direction: "download".to_string(),
                            total_files: None,
                            path: None,
                        }),
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
            }
            !cancel_flag.load(Ordering::Relaxed)
        })
        .await
    {
        Ok(_) => {
            // Preserve remote mtime on the local file
            preserve_remote_mtime(&params.local_path, params.modified.as_deref());

            // Emit complete event
            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Download complete: {}", filename)),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Ok(format!("Downloaded: {}", filename))
        }
        Err(e) => {
            // Emit error event
            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "download".to_string(),
                    message: Some(format!("Download failed: {}", e)),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Err(format!("Download failed: {}", e))
        }
    }
}

#[tauri::command]
async fn upload_file(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
    params: UploadParams,
) -> Result<String, String> {
    // Check if already cancelled (batch stop): bail immediately
    if state.cancel_flag.load(Ordering::Relaxed) {
        return Err("Transfer cancelled by user".to_string());
    }

    let cancel_flag_upload = state.cancel_flag.clone();
    let filename = PathBuf::from(&params.local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let transfer_id = format!("ul-{}", chrono::Utc::now().timestamp_millis());

    // Get local file size
    let file_size = tokio::fs::metadata(&params.local_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    // Emit start event
    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: filename.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Starting upload: {}", filename)),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    // Try provider path first (cloud providers, GitHub, etc.)
    {
        let provider_connected = {
            let guard = provider_state.provider.lock().await;
            guard.is_some()
        };
        if provider_connected {
            let mut guard = provider_state.provider.lock().await;
            if let Some(provider) = guard.as_mut() {
                let mut delta_fallback_reason: Option<String> = None;
                // Delta path (SFTP + key-auth + rsync on remote): attempted
                // before the classic provider upload. `try_delta_transfer`
                // is self-gated: `None` for non-SFTP / password-only /
                // missing SSH handle, `used_delta=true` when rsync ran,
                // `hard_error` when security (host-key, permission) said
                // no: in which case we must NOT silently fall back to
                // the classic provider path. Same contract as
                // `sync::perform_upload`.
                {
                    let local_path_buf = std::path::PathBuf::from(&params.local_path);
                    if delta_sync_rsync::gui_delta_enabled() && params.use_delta {
                        if let Some(result) = delta_sync_rsync::try_delta_transfer_with_progress(
                            provider.as_mut(),
                            delta_sync_rsync::SyncDirection::Upload,
                            &local_path_buf,
                            &params.remote_path,
                            Some(crate::make_delta_progress_sink(
                                app.clone(),
                                transfer_id.clone(),
                                filename.clone(),
                                "upload",
                            )),
                        )
                        .await
                        {
                            if result.used_delta {
                                let delta_stats = result
                                    .stats
                                    .as_ref()
                                    .map(sync::DeltaTransferStats::from_rsync);
                                let _ = app.emit(
                                    "transfer_event",
                                    TransferEvent {
                                        event_type: "complete".to_string(),
                                        transfer_id: transfer_id.clone(),
                                        filename: filename.clone(),
                                        direction: "upload".to_string(),
                                        message: Some(format!(
                                            "Uploaded: {} (via delta)",
                                            filename
                                        )),
                                        progress: None,
                                        path: None,
                                        delta_stats,
                                        fallback_reason: None,
                                    },
                                );
                                return Ok(format!("Uploaded: {}", filename));
                            }
                            if let Some(hard_err) = result.hard_error {
                                let err_msg = format!("delta hard rejection: {}", hard_err);
                                let _ = app.emit(
                                    "transfer_event",
                                    TransferEvent {
                                        event_type: "error".to_string(),
                                        transfer_id: transfer_id.clone(),
                                        filename: filename.clone(),
                                        direction: "upload".to_string(),
                                        message: Some(err_msg.clone()),
                                        progress: None,
                                        path: None,
                                        delta_stats: None,
                                        fallback_reason: None,
                                    },
                                );
                                return Err(err_msg);
                            }
                            // fallback_reason populated: fall through to the
                            // classic provider upload below.
                            delta_fallback_reason = result.fallback_reason;
                        }
                    }
                }

                let result = provider
                    .upload(&params.local_path, &params.remote_path, None)
                    .await;
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: if result.is_ok() {
                            "complete".to_string()
                        } else {
                            "error".to_string()
                        },
                        transfer_id: transfer_id.clone(),
                        filename: filename.clone(),
                        direction: "upload".to_string(),
                        message: Some(if result.is_ok() {
                            format!("Uploaded: {}", filename)
                        } else {
                            format!("Upload failed: {}", filename)
                        }),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: if result.is_ok() {
                            delta_fallback_reason
                        } else {
                            None
                        },
                    },
                );
                return result
                    .map(|_| format!("Uploaded: {}", filename))
                    .map_err(|e| format!("Failed to upload file: {}", e));
            }
        }
    }

    let mut ftp_manager = state.ftp_manager.lock().await;
    let start_time = Instant::now();
    let mut last_emit_time_ul = Instant::now();
    let mut last_emit_pct_ul = 0u8;

    // Upload with progress (throttled: emit every 150ms or 2% delta)
    match ftp_manager
        .upload_file_with_progress(
            &params.local_path,
            &params.remote_path,
            file_size,
            |transferred| {
                let elapsed = start_time.elapsed().as_secs_f64();
                let speed = if elapsed > 0.0 {
                    (transferred as f64 / elapsed) as u64
                } else {
                    0
                };
                let percentage = if file_size > 0 {
                    ((transferred as f64 / file_size as f64) * 100.0) as u8
                } else {
                    0
                };

                let is_complete = transferred >= file_size && file_size > 0;
                let time_delta = last_emit_time_ul.elapsed().as_millis() >= 150;
                let pct_delta = percentage.saturating_sub(last_emit_pct_ul) >= 2;
                if time_delta || pct_delta || is_complete {
                    last_emit_time_ul = Instant::now();
                    last_emit_pct_ul = percentage;
                    let eta = if speed > 0 && file_size > transferred {
                        ((file_size - transferred) / speed) as u32
                    } else {
                        0
                    };

                    let _ = app.emit(
                        "transfer_event",
                        TransferEvent {
                            event_type: "progress".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "upload".to_string(),
                            message: None,
                            progress: Some(TransferProgress {
                                transfer_id: transfer_id.clone(),
                                filename: filename.clone(),
                                transferred,
                                total: file_size,
                                percentage,
                                speed_bps: speed,
                                eta_seconds: eta,
                                direction: "upload".to_string(),
                                total_files: None,
                                path: None,
                            }),
                            path: None,
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                }
                !cancel_flag_upload.load(Ordering::Relaxed)
            },
        )
        .await
    {
        Ok(_) => {
            // Emit complete event
            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Upload complete: {}", filename)),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Ok(format!("Uploaded: {}", filename))
        }
        Err(e) => {
            // Emit error event
            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!("Upload failed: {}", e)),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Err(format!("Upload failed: {}", e))
        }
    }
}

#[tauri::command]
async fn download_files_batch(
    app: AppHandle,
    state: State<'_, AppState>,
    params: FileTransferBatchParams,
) -> Result<String, String> {
    if state.cancel_flag.load(Ordering::Relaxed) {
        return Err("Transfer cancelled by user".to_string());
    }

    if params.entries.is_empty() {
        return Ok("Downloaded 0 files, 0 errors".to_string());
    }

    let runtime_settings = transfer_settings::resolve_ftp_transfer_settings(
        transfer_settings::TransferSettingsInput {
            max_concurrent: params.max_concurrent,
            retry_count: params.retry_count,
            timeout_seconds: params.timeout_seconds,
            // GTC-1: FTP GUI batch stays on `FtpDownloadExecutor`
            // (no-double-pool invariant); the segments knob only
            // matters on the `ProviderDownloadExecutor` path.
            download_segments: None,
        },
    );

    let cancel_token = state.reset_cancel_state().await;

    let transfer_id = format!("dl-files-{}", chrono::Utc::now().timestamp_millis());
    let display_name = format!(
        "{} file{}",
        params.entries.len(),
        if params.entries.len() == 1 { "" } else { "s" }
    );
    let batch_path = params
        .entries
        .first()
        .map(|entry| {
            PathBuf::from(&entry.remote_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: display_name.clone(),
            direction: "download".to_string(),
            message: Some(format!("Starting batch download: {}", display_name)),
            progress: Some(TransferProgress {
                transfer_id: transfer_id.clone(),
                filename: display_name.clone(),
                transferred: 0,
                total: params.entries.len() as u64,
                percentage: 0,
                speed_bps: 0,
                eta_seconds: 0,
                direction: "download".to_string(),
                total_files: Some(params.entries.len() as u64),
                path: Some(batch_path.clone()),
            }),
            path: Some(batch_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    for entry in &params.entries {
        if let Some(parent) = Path::new(&entry.local_path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                format!(
                    "Failed to create local directory {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }
    }

    let connection_spec = {
        let mut ftp_manager = state.ftp_manager.lock().await;
        ftp_manager.apply_transfer_timeout(runtime_settings.timeout_seconds);
        ftp_manager
            .connection_spec()
            .map_err(|e| format!("Failed to derive FTP pool config: {}", e))?
    };

    let batch_entries = params
        .entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| transfer_domain::TransferEntry {
            id: format!("{}-{}", transfer_id, index),
            ..entry
        })
        .collect::<Vec<_>>();

    let total_files_for_progress = batch_entries.len() as u32;
    let batch = transfer_orchestrator::TransferBatch {
        id: transfer_id.clone(),
        display_name: display_name.clone(),
        direction: transfer_domain::TransferDirection::Download,
        config: transfer_domain::TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
        },
        entries: batch_entries,
    };

    let pool = Arc::new(
        ftp_session_pool::FtpSessionPool::create(ftp_session_pool::FtpPoolConfig::from_connection(
            connection_spec,
            runtime_settings.max_concurrent.max(1) as usize,
            1,
            runtime_settings.timeout_seconds * 1000,
        ))
        .await
        .map_err(|e| format!("Failed to create FTP session pool: {}", e))?,
    );

    let progress_app = app.clone();
    let progress_transfer_id = transfer_id.clone();
    let progress_display_name = display_name.clone();
    let progress_batch_path = batch_path.clone();
    let progress_observer: transfer_orchestrator::ProgressObserver = Arc::new(move |snapshot| {
        let processed = snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        let _ = progress_app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_display_name.clone(),
                direction: "download".to_string(),
                message: Some(format!(
                    "Downloaded {} / {} files ({} errors)",
                    snapshot.completed, total_files_for_progress, snapshot.failed
                )),
                progress: Some(TransferProgress {
                    transfer_id: progress_transfer_id.clone(),
                    filename: progress_display_name.clone(),
                    transferred: processed as u64,
                    total: total_files_for_progress as u64,
                    percentage,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "download".to_string(),
                    total_files: Some(total_files_for_progress as u64),
                    path: Some(progress_batch_path.clone()),
                }),
                path: Some(progress_batch_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    });

    let executor = Arc::new(ftp_transfer_executor::FtpDownloadExecutor::new(
        app.clone(),
        pool.clone(),
        runtime_settings,
        cancel_token,
    ));

    // PD-CLI-CONV-B: the orchestrator is now sink-agnostic. The FTP
    // executors keep their own `AppHandle` for `transfer_event`; only the
    // three batch-lifecycle events move behind the sink. `AppHandleSink`
    // is a 1:1 adapter, so the emitted payloads are byte-identical.
    let batch_sink: std::sync::Arc<dyn crate::transfer_event_sink::TransferEventSink> =
        std::sync::Arc::new(crate::transfer_event_sink::AppHandleSink::new(app.clone()));
    let batch_result = transfer_orchestrator::execute_batch(
        batch_sink,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    if let Err(e) = pool.close().await {
        warn!(
            "Failed to close FTP session pool cleanly after batch file download: {}",
            e
        );
    }

    let files_downloaded = batch_result.completed;
    let files_errored = batch_result.failed;
    let result_message = if batch_result.cancelled {
        format!(
            "Download cancelled after {} files",
            files_downloaded + files_errored
        )
    } else {
        format!(
            "Downloaded {} files, {} errors",
            files_downloaded, files_errored
        )
    };

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: if batch_result.cancelled {
                "cancelled".to_string()
            } else {
                "complete".to_string()
            },
            transfer_id,
            filename: display_name,
            direction: "download".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: Some(batch_path),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    Ok(result_message)
}

#[tauri::command]
async fn upload_files_batch(
    app: AppHandle,
    state: State<'_, AppState>,
    params: FileTransferBatchParams,
) -> Result<String, String> {
    if state.cancel_flag.load(Ordering::Relaxed) {
        return Err("Transfer cancelled by user".to_string());
    }

    if params.entries.is_empty() {
        return Ok("Uploaded 0 files, 0 errors".to_string());
    }

    let runtime_settings = transfer_settings::resolve_ftp_transfer_settings(
        transfer_settings::TransferSettingsInput {
            max_concurrent: params.max_concurrent,
            retry_count: params.retry_count,
            timeout_seconds: params.timeout_seconds,
            // GTC-1: FTP GUI batch stays on `FtpDownloadExecutor`
            // (no-double-pool invariant); the segments knob only
            // matters on the `ProviderDownloadExecutor` path.
            download_segments: None,
        },
    );

    let cancel_token = state.reset_cancel_state().await;

    let transfer_id = format!("ul-files-{}", chrono::Utc::now().timestamp_millis());
    let display_name = format!(
        "{} file{}",
        params.entries.len(),
        if params.entries.len() == 1 { "" } else { "s" }
    );
    let batch_path = params
        .entries
        .first()
        .map(|entry| {
            PathBuf::from(&entry.remote_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: display_name.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Starting batch upload: {}", display_name)),
            progress: Some(TransferProgress {
                transfer_id: transfer_id.clone(),
                filename: display_name.clone(),
                transferred: 0,
                total: params.entries.len() as u64,
                percentage: 0,
                speed_bps: 0,
                eta_seconds: 0,
                direction: "upload".to_string(),
                total_files: Some(params.entries.len() as u64),
                path: Some(batch_path.clone()),
            }),
            path: Some(batch_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let connection_spec = {
        let mut ftp_manager = state.ftp_manager.lock().await;
        ftp_manager.apply_transfer_timeout(runtime_settings.timeout_seconds);
        ftp_manager
            .connection_spec()
            .map_err(|e| format!("Failed to derive FTP pool config: {}", e))?
    };

    let batch_entries = params
        .entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| transfer_domain::TransferEntry {
            id: format!("{}-{}", transfer_id, index),
            ..entry
        })
        .collect::<Vec<_>>();

    let total_files_for_progress = batch_entries.len() as u32;
    let batch = transfer_orchestrator::TransferBatch {
        id: transfer_id.clone(),
        display_name: display_name.clone(),
        direction: transfer_domain::TransferDirection::Upload,
        config: transfer_domain::TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
        },
        entries: batch_entries,
    };

    let pool = Arc::new(
        ftp_session_pool::FtpSessionPool::create(ftp_session_pool::FtpPoolConfig::from_connection(
            connection_spec,
            runtime_settings.max_concurrent.max(1) as usize,
            1,
            runtime_settings.timeout_seconds * 1000,
        ))
        .await
        .map_err(|e| format!("Failed to create FTP session pool: {}", e))?,
    );

    let progress_app = app.clone();
    let progress_transfer_id = transfer_id.clone();
    let progress_display_name = display_name.clone();
    let progress_batch_path = batch_path.clone();
    let progress_observer: transfer_orchestrator::ProgressObserver = Arc::new(move |snapshot| {
        let processed = snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        let _ = progress_app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_display_name.clone(),
                direction: "upload".to_string(),
                message: Some(format!(
                    "Uploaded {} / {} files ({} errors)",
                    snapshot.completed, total_files_for_progress, snapshot.failed
                )),
                progress: Some(TransferProgress {
                    transfer_id: progress_transfer_id.clone(),
                    filename: progress_display_name.clone(),
                    transferred: processed as u64,
                    total: total_files_for_progress as u64,
                    percentage,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "upload".to_string(),
                    total_files: Some(total_files_for_progress as u64),
                    path: Some(progress_batch_path.clone()),
                }),
                path: Some(progress_batch_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    });

    let executor = Arc::new(ftp_transfer_executor::FtpUploadExecutor::new(
        app.clone(),
        pool.clone(),
        runtime_settings,
        cancel_token,
    ));

    // PD-CLI-CONV-B: the orchestrator is now sink-agnostic. The FTP
    // executors keep their own `AppHandle` for `transfer_event`; only the
    // three batch-lifecycle events move behind the sink. `AppHandleSink`
    // is a 1:1 adapter, so the emitted payloads are byte-identical.
    let batch_sink: std::sync::Arc<dyn crate::transfer_event_sink::TransferEventSink> =
        std::sync::Arc::new(crate::transfer_event_sink::AppHandleSink::new(app.clone()));
    let batch_result = transfer_orchestrator::execute_batch(
        batch_sink,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    if let Err(e) = pool.close().await {
        warn!(
            "Failed to close FTP session pool cleanly after batch file upload: {}",
            e
        );
    }

    let files_uploaded = batch_result.completed;
    let files_errored = batch_result.failed;
    let result_message = if batch_result.cancelled {
        format!(
            "Upload cancelled after {} files",
            files_uploaded + files_errored
        )
    } else {
        format!(
            "Uploaded {} files, {} errors",
            files_uploaded, files_errored
        )
    };

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: if batch_result.cancelled {
                "cancelled".to_string()
            } else {
                "complete".to_string()
            },
            transfer_id,
            filename: display_name,
            direction: "upload".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: Some(batch_path),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    Ok(result_message)
}

/// Preserve remote file modification time on a downloaded local file.
/// Parses common ISO 8601 / timestamp formats and sets the file's mtime via `filetime`.
/// Best-effort: silently ignores failures (e.g. permission denied, unparseable timestamp).
pub fn preserve_remote_mtime(local_path: &str, remote_modified: Option<&str>) {
    let Some(modified_str) = remote_modified else {
        return;
    };
    // Strip trailing 'Z' suffix (UTC marker added in v2.9.6) before NaiveDateTime parsing
    let clean_str = modified_str.strip_suffix('Z').unwrap_or(modified_str);
    let ts = chrono::NaiveDateTime::parse_from_str(clean_str, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(clean_str, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(clean_str, "%Y-%m-%dT%H:%M:%S%.f"))
        .or_else(|_| {
            // Try parsing full RFC 3339 (with timezone) → strip tz suffix
            chrono::DateTime::parse_from_rfc3339(modified_str).map(|dt| dt.naive_utc())
        })
        .ok();
    if let Some(ndt) = ts {
        let secs = ndt.and_utc().timestamp();
        let ft = filetime::FileTime::from_unix_time(secs, 0);
        let _ = filetime::set_file_mtime(local_path, ft);
    }
}

/// Preserve remote mtime from a `chrono::DateTime<Utc>`.
pub fn preserve_remote_mtime_dt(
    local_path: &std::path::Path,
    remote_modified: Option<chrono::DateTime<chrono::Utc>>,
) {
    if let Some(dt) = remote_modified {
        let ft = filetime::FileTime::from_unix_time(dt.timestamp(), 0);
        let _ = filetime::set_file_mtime(local_path, ft);
    }
}

/// Check if a file should be skipped during folder download based on the file_exists_action setting.
/// Used for download: source is remote, destination is local filesystem.
pub fn should_skip_file_download(
    action: &str,
    source_modified: Option<chrono::DateTime<chrono::Utc>>,
    source_size: u64,
    dest_meta: &std::fs::Metadata,
) -> bool {
    use chrono::DateTime;
    let dest_size = dest_meta.len();
    let dest_modified: Option<DateTime<chrono::Utc>> =
        dest_meta.modified().ok().map(DateTime::<chrono::Utc>::from);
    const TOLERANCE_SECS: i64 = 2;

    match action {
        "skip" => true,
        "overwrite_if_newer" | "merge_overwrite_newer" => {
            // Skip if source is NOT newer than destination
            match (source_modified, dest_modified) {
                (Some(src), Some(dst)) => src.timestamp() <= dst.timestamp() + TOLERANCE_SECS,
                _ => false, // If unknown dates, don't skip (overwrite)
            }
        }
        "overwrite_if_different" | "skip_if_identical" | "merge_skip_identical" => {
            // Skip if date AND size are the same
            let size_same = source_size == dest_size;
            let date_same = match (source_modified, dest_modified) {
                (Some(src), Some(dst)) => {
                    (src.timestamp() - dst.timestamp()).abs() <= TOLERANCE_SECS
                }
                _ => false,
            };
            size_same && date_same
        }
        _ => false, // "overwrite" or empty → don't skip
    }
}

/// Check if a file should be skipped during folder upload based on the file_exists_action setting.
/// Used for upload: source is local filesystem, destination is remote.
pub(crate) fn should_skip_file_upload(
    action: &str,
    local_meta: &std::fs::Metadata,
    remote_size: u64,
    remote_modified: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    use chrono::DateTime;
    let local_size = local_meta.len();
    let local_modified: Option<DateTime<chrono::Utc>> = local_meta
        .modified()
        .ok()
        .map(DateTime::<chrono::Utc>::from);
    const TOLERANCE_SECS: i64 = 2;

    match action {
        "skip" => true,
        "overwrite_if_newer" | "merge_overwrite_newer" => {
            // Skip if local (source) is NOT newer than remote (dest)
            match (local_modified, remote_modified) {
                (Some(src), Some(dst)) => src.timestamp() <= dst.timestamp() + TOLERANCE_SECS,
                _ => false,
            }
        }
        "overwrite_if_different" | "skip_if_identical" | "merge_skip_identical" => {
            let size_same = local_size == remote_size;
            let date_same = match (local_modified, remote_modified) {
                (Some(src), Some(dst)) => {
                    (src.timestamp() - dst.timestamp()).abs() <= TOLERANCE_SECS
                }
                _ => false,
            };
            size_same && date_same
        }
        _ => false,
    }
}

#[derive(Debug, Default)]
struct FtpDownloadScanResult {
    entries: Vec<transfer_domain::TransferEntry>,
    total_files_discovered: u32,
    files_skipped: u32,
    scan_errors: u32,
    cancelled: bool,
}

pub(crate) fn parse_remote_modified_datetime(
    remote_modified: Option<&str>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let modified_str = remote_modified?;
    let clean_str = modified_str.strip_suffix('Z').unwrap_or(modified_str);

    chrono::NaiveDateTime::parse_from_str(clean_str, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(clean_str, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(clean_str, "%Y-%m-%dT%H:%M:%S%.f"))
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(modified_str).map(|dt| dt.naive_utc()))
        .ok()
        .map(|ndt| ndt.and_utc())
}

#[allow(clippy::too_many_arguments)]
async fn scan_ftp_download_entries(
    app: &AppHandle,
    cancel_flag: &Arc<AtomicBool>,
    ftp_manager: &mut ftp::FtpManager,
    remote_path: &str,
    local_folder_path: &Path,
    file_exists_action: &str,
    transfer_id: &str,
    folder_name: &str,
) -> Result<FtpDownloadScanResult, String> {
    let mut result = FtpDownloadScanResult::default();
    let base_local = local_folder_path.to_path_buf();
    let mut dirs_to_scan: Vec<(String, PathBuf)> =
        vec![(remote_path.to_string(), local_folder_path.to_path_buf())];
    let mut last_scan_emit = std::time::Instant::now();
    let mut file_index: u32 = 0;

    while let Some((remote_dir, local_dir)) = dirs_to_scan.pop() {
        if cancel_flag.load(Ordering::Relaxed) {
            result.cancelled = true;
            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "cancelled".to_string(),
                    transfer_id: transfer_id.to_string(),
                    filename: folder_name.to_string(),
                    direction: "download".to_string(),
                    message: Some("Download cancelled during scan".to_string()),
                    progress: None,
                    path: Some(remote_path.to_string()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return Ok(result);
        }

        if let Err(e) = ftp_manager.change_dir(&remote_dir).await {
            warn!("Cannot access remote directory {}: {}", remote_dir, e);
            continue;
        }

        let files = match ftp_manager.list_files().await {
            Ok(files) => files,
            Err(e) => {
                warn!("Cannot list files in {}: {}", remote_dir, e);
                continue;
            }
        };

        for file in files {
            let safe_name = match crate::provider_commands::sanitize_remote_filename(&file.name) {
                Ok(name) => name,
                Err(error) => {
                    warn!("Skipping unsafe FTP remote entry {}: {}", file.name, error);
                    result.scan_errors += 1;
                    continue;
                }
            };
            let remote_file_path = format!("{}/{}", remote_dir.trim_end_matches('/'), file.name);
            let local_file_path = local_dir.join(&safe_name);

            if file.is_dir {
                if let Err(e) = tokio::fs::create_dir_all(&local_file_path).await {
                    warn!(
                        "Failed to create local directory {}: {}",
                        local_file_path.display(),
                        e
                    );
                    result.scan_errors += 1;
                    continue;
                }

                if let Err(error) =
                    crate::provider_commands::verify_path_containment(&base_local, &local_file_path)
                {
                    warn!(
                        "Skipping FTP directory {} due to unsafe local target {}: {}",
                        remote_file_path,
                        local_file_path.display(),
                        error
                    );
                    result.scan_errors += 1;
                    continue;
                }

                dirs_to_scan.push((remote_file_path, local_file_path));
                continue;
            }

            result.total_files_discovered += 1;
            let modified_dt = parse_remote_modified_datetime(file.modified.as_deref());

            if let Some(parent) = local_file_path.parent() {
                if parent.exists() {
                    if let Err(error) = crate::provider_commands::verify_path_containment(
                        &base_local,
                        &local_file_path,
                    ) {
                        warn!(
                            "Skipping FTP file {} due to unsafe local target {}: {}",
                            remote_file_path,
                            local_file_path.display(),
                            error
                        );
                        result.scan_errors += 1;
                        continue;
                    }
                }
            }

            if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
                if let Ok(local_meta) = std::fs::metadata(&local_file_path) {
                    if local_meta.is_file()
                        && should_skip_file_download(
                            file_exists_action,
                            modified_dt,
                            file.size.unwrap_or(0),
                            &local_meta,
                        )
                    {
                        result.files_skipped += 1;
                        let _ = app.emit(
                            "transfer_event",
                            TransferEvent {
                                event_type: "file_skip".to_string(),
                                transfer_id: transfer_id.to_string(),
                                filename: safe_name.clone(),
                                direction: "download".to_string(),
                                message: Some(format!("Skipped (identical): {}", safe_name)),
                                progress: None,
                                path: Some(remote_file_path.clone()),
                                delta_stats: None,
                                fallback_reason: None,
                            },
                        );
                        continue;
                    }
                }
            }

            let file_transfer_id = format!("{}-{}", transfer_id, file_index);
            file_index += 1;

            result.entries.push(transfer_domain::TransferEntry {
                id: file_transfer_id,
                display_name: safe_name,
                remote_path: remote_file_path,
                local_path: local_file_path.to_string_lossy().to_string(),
                size: file.size.unwrap_or(0),
                modified: file.modified,
            });
        }

        if last_scan_emit.elapsed().as_millis() > 500 || result.total_files_discovered <= 1 {
            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "scanning".to_string(),
                    transfer_id: transfer_id.to_string(),
                    filename: folder_name.to_string(),
                    direction: "download".to_string(),
                    message: Some(format!(
                        "Scanning... {} files found, {} skipped ({} dirs queued)",
                        result.total_files_discovered,
                        result.files_skipped,
                        dirs_to_scan.len()
                    )),
                    progress: None,
                    path: Some(remote_path.to_string()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            last_scan_emit = std::time::Instant::now();
        }
    }

    Ok(result)
}

#[tauri::command]
async fn download_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    params: DownloadFolderParams,
) -> Result<String, String> {
    let runtime_settings = transfer_settings::resolve_ftp_transfer_settings(
        transfer_settings::TransferSettingsInput {
            max_concurrent: params.max_concurrent,
            retry_count: params.retry_count,
            timeout_seconds: params.timeout_seconds,
            // GTC-1: FTP GUI batch stays on `FtpDownloadExecutor`
            // (no-double-pool invariant); the segments knob only
            // matters on the `ProviderDownloadExecutor` path.
            download_segments: None,
        },
    );
    info!(
        "Downloading folder: {} -> {} (concurrency={}, retries={}, timeout={}s)",
        params.remote_path,
        params.local_path,
        runtime_settings.max_concurrent,
        runtime_settings.retry_count,
        runtime_settings.timeout_seconds
    );

    let cancel_token = state.reset_cancel_state().await;

    let folder_name = PathBuf::from(&params.remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());

    let transfer_id = format!("dl-folder-{}", chrono::Utc::now().timestamp_millis());

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "download".to_string(),
            message: Some(format!("Starting folder download: {}", folder_name)),
            progress: None,
            path: Some(params.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let local_folder_path = PathBuf::from(&params.local_path);
    if let Err(e) = tokio::fs::create_dir_all(&local_folder_path).await {
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "error".to_string(),
                transfer_id: transfer_id.clone(),
                filename: folder_name.clone(),
                direction: "download".to_string(),
                message: Some(format!("Failed to create local directory: {}", e)),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        return Err(format!("Failed to create local directory: {}", e));
    }

    let (scan_result, connection_spec) = {
        let mut ftp_manager = state.ftp_manager.lock().await;
        ftp_manager.apply_transfer_timeout(runtime_settings.timeout_seconds);
        let original_path = ftp_manager.current_path();

        let scan_result = scan_ftp_download_entries(
            &app,
            &state.cancel_flag,
            &mut ftp_manager,
            &params.remote_path,
            &local_folder_path,
            params.file_exists_action.as_str(),
            &transfer_id,
            &folder_name,
        )
        .await?;

        let _ = ftp_manager.change_dir(&original_path).await;

        let connection_spec = if scan_result.cancelled {
            None
        } else {
            let mut connection_spec = ftp_manager
                .connection_spec()
                .map_err(|e| format!("Failed to derive FTP pool config: {}", e))?;
            connection_spec.initial_path = original_path;
            Some(connection_spec)
        };

        (scan_result, connection_spec)
    };

    if scan_result.cancelled {
        return Ok("Download cancelled after 0 files".to_string());
    }

    let batch = transfer_orchestrator::TransferBatch {
        id: transfer_id.clone(),
        display_name: folder_name.clone(),
        direction: transfer_domain::TransferDirection::Download,
        config: transfer_domain::TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
        },
        entries: scan_result.entries,
    };

    let pool = Arc::new(
        ftp_session_pool::FtpSessionPool::create(ftp_session_pool::FtpPoolConfig::from_connection(
            connection_spec.ok_or("FTP pool configuration unavailable".to_string())?,
            runtime_settings.max_concurrent.max(1) as usize,
            1,
            runtime_settings.timeout_seconds * 1000,
        ))
        .await
        .map_err(|e| format!("Failed to create FTP session pool: {}", e))?,
    );

    let progress_app = app.clone();
    let total_files_for_progress = scan_result.total_files_discovered;
    let initial_skipped = scan_result.files_skipped;
    let progress_transfer_id = transfer_id.clone();
    let progress_folder_name = folder_name.clone();
    let progress_remote_path = params.remote_path.clone();
    let progress_observer: transfer_orchestrator::ProgressObserver = Arc::new(move |snapshot| {
        let processed = initial_skipped + snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        let _ = progress_app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_folder_name.clone(),
                direction: "download".to_string(),
                message: Some(format!(
                    "Downloaded {} / {} files ({} skipped, {} errors)",
                    snapshot.completed, total_files_for_progress, initial_skipped, snapshot.failed
                )),
                progress: Some(TransferProgress {
                    transfer_id: progress_transfer_id.clone(),
                    filename: progress_folder_name.clone(),
                    transferred: processed as u64,
                    total: total_files_for_progress as u64,
                    percentage,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "download".to_string(),
                    total_files: Some(total_files_for_progress as u64),
                    path: Some(progress_remote_path.clone()),
                }),
                path: Some(progress_remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    });

    let executor = Arc::new(ftp_transfer_executor::FtpDownloadExecutor::new(
        app.clone(),
        pool.clone(),
        runtime_settings,
        cancel_token,
    ));

    // PD-CLI-CONV-B: the orchestrator is now sink-agnostic. The FTP
    // executors keep their own `AppHandle` for `transfer_event`; only the
    // three batch-lifecycle events move behind the sink. `AppHandleSink`
    // is a 1:1 adapter, so the emitted payloads are byte-identical.
    let batch_sink: std::sync::Arc<dyn crate::transfer_event_sink::TransferEventSink> =
        std::sync::Arc::new(crate::transfer_event_sink::AppHandleSink::new(app.clone()));
    let batch_result = transfer_orchestrator::execute_batch(
        batch_sink,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    if let Err(e) = pool.close().await {
        warn!("Failed to close FTP session pool cleanly: {}", e);
    }

    let files_downloaded = batch_result.completed;
    let files_errored = batch_result.failed + scan_result.scan_errors;
    let result_message = if batch_result.cancelled {
        format!(
            "Download cancelled after {} files",
            files_downloaded + scan_result.files_skipped + files_errored
        )
    } else {
        format!(
            "Downloaded {} files, {} skipped, {} errors",
            files_downloaded, scan_result.files_skipped, files_errored
        )
    };

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: if batch_result.cancelled {
                "cancelled".to_string()
            } else {
                "complete".to_string()
            },
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "download".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    Ok(result_message)
}

/// Upload an entire folder to the FTP server with full recursive support.
/// Uses stack-based iterative traversal to upload ALL files in ALL subdirectories.
/// Emits per-file events for activity log visibility.
#[derive(Debug, Default)]
struct FtpUploadPreparationResult {
    entries: Vec<transfer_domain::TransferEntry>,
    total_files_discovered: u32,
    total_dirs_discovered: u32,
    files_skipped: u32,
    scan_errors: u32,
    cancelled: bool,
}

#[allow(clippy::too_many_arguments)]
async fn prepare_ftp_upload_entries(
    app: &AppHandle,
    cancel_flag: &Arc<AtomicBool>,
    ftp_manager: &mut ftp::FtpManager,
    local_base_path: &Path,
    remote_base_path: &str,
    file_exists_action: &str,
    transfer_id: &str,
    folder_name: &str,
) -> Result<FtpUploadPreparationResult, String> {
    #[derive(Debug)]
    struct UploadItem {
        local_path: PathBuf,
        remote_path: String,
        size: u64,
        name: String,
    }

    let mut result = FtpUploadPreparationResult::default();
    let mut files_to_upload: Vec<UploadItem> = Vec::new();
    let mut dirs_to_create: Vec<String> = vec![remote_base_path.to_string()];
    let mut dirs_to_scan: Vec<(PathBuf, String)> =
        vec![(local_base_path.to_path_buf(), remote_base_path.to_string())];
    let mut scan_counter: u64 = 0;
    let mut last_scan_emit = std::time::Instant::now();

    while let Some((current_local_dir, current_remote_dir)) = dirs_to_scan.pop() {
        if cancel_flag.load(Ordering::Relaxed) {
            result.cancelled = true;
            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "cancelled".to_string(),
                    transfer_id: transfer_id.to_string(),
                    filename: folder_name.to_string(),
                    direction: "upload".to_string(),
                    message: Some("Upload cancelled during scan".to_string()),
                    progress: None,
                    path: Some(remote_base_path.to_string()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return Ok(result);
        }

        let mut read_dir = match tokio::fs::read_dir(&current_local_dir).await {
            Ok(rd) => rd,
            Err(e) => {
                warn!("Failed to read directory {:?}: {}", current_local_dir, e);
                result.scan_errors += 1;
                continue;
            }
        };

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let local_path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let remote_path = format!("{}/{}", current_remote_dir, name);

            let file_type = match entry.file_type().await {
                Ok(file_type) => file_type,
                Err(error) => {
                    warn!("Failed to read file type for {:?}: {}", local_path, error);
                    result.scan_errors += 1;
                    continue;
                }
            };

            if file_type.is_symlink() {
                result.files_skipped += 1;
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "file_skip".to_string(),
                        transfer_id: transfer_id.to_string(),
                        filename: name.clone(),
                        direction: "upload".to_string(),
                        message: Some(format!("Skipped symlink: {}", name)),
                        progress: None,
                        path: Some(remote_path.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                continue;
            }

            if file_type.is_dir() {
                dirs_to_scan.push((local_path.clone(), remote_path.clone()));
                dirs_to_create.push(remote_path);
                result.total_dirs_discovered += 1;
            } else if file_type.is_file() {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                files_to_upload.push(UploadItem {
                    local_path,
                    remote_path,
                    size,
                    name,
                });
                result.total_files_discovered += 1;
            }

            scan_counter += 1;
            if last_scan_emit.elapsed().as_millis() > 500 || scan_counter % 100 == 0 {
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "scanning".to_string(),
                        transfer_id: transfer_id.to_string(),
                        filename: folder_name.to_string(),
                        direction: "upload".to_string(),
                        message: Some(format!(
                            "Scanning... {} files, {} folders found",
                            result.total_files_discovered,
                            result.total_dirs_discovered + 1
                        )),
                        progress: None,
                        path: Some(remote_base_path.to_string()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                last_scan_emit = std::time::Instant::now();
            }
        }
    }

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: "scanning".to_string(),
            transfer_id: transfer_id.to_string(),
            filename: folder_name.to_string(),
            direction: "upload".to_string(),
            message: Some(format!(
                "Scan complete: {} files in {} folders",
                result.total_files_discovered,
                result.total_dirs_discovered + 1
            )),
            progress: None,
            path: Some(remote_base_path.to_string()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let mut dirs_sorted = dirs_to_create;
    dirs_sorted.sort_by_key(|a| a.matches('/').count());

    for remote_dir in &dirs_sorted {
        if cancel_flag.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }

        match ftp_manager.mkdir(remote_dir).await {
            Ok(_) => {}
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if !err_str.contains("exist") && !err_str.contains("550") {
                    warn!("Could not create directory {}: {}", remote_dir, e);
                    result.scan_errors += 1;
                }
            }
        }
    }

    if result.cancelled {
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "cancelled".to_string(),
                transfer_id: transfer_id.to_string(),
                filename: folder_name.to_string(),
                direction: "upload".to_string(),
                message: Some("Upload cancelled before transfer execution".to_string()),
                progress: None,
                path: Some(remote_base_path.to_string()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
        return Ok(result);
    }

    let mut remote_index: std::collections::HashMap<
        String,
        (u64, Option<chrono::DateTime<chrono::Utc>>),
    > = std::collections::HashMap::new();

    if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
        let saved_path = ftp_manager.current_path();
        for remote_dir in &dirs_sorted {
            if cancel_flag.load(Ordering::Relaxed) {
                result.cancelled = true;
                break;
            }

            if ftp_manager.change_dir(remote_dir).await.is_ok() {
                if let Ok(entries) = ftp_manager.list_files().await {
                    for entry in entries {
                        if !entry.is_dir {
                            let remote_file_path =
                                format!("{}/{}", remote_dir.trim_end_matches('/'), entry.name);
                            let modified_dt =
                                parse_remote_modified_datetime(entry.modified.as_deref());
                            remote_index
                                .insert(remote_file_path, (entry.size.unwrap_or(0), modified_dt));
                        }
                    }
                }
            }
        }
        let _ = ftp_manager.change_dir(&saved_path).await;
    }

    if result.cancelled {
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "cancelled".to_string(),
                transfer_id: transfer_id.to_string(),
                filename: folder_name.to_string(),
                direction: "upload".to_string(),
                message: Some("Upload cancelled before transfer execution".to_string()),
                progress: None,
                path: Some(remote_base_path.to_string()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
        return Ok(result);
    }

    let mut file_index: u32 = 0;
    for item in files_to_upload {
        if cancel_flag.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }

        if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
            if let Some(&(remote_size, remote_modified)) = remote_index.get(&item.remote_path) {
                if let Ok(local_meta) = std::fs::metadata(&item.local_path) {
                    if should_skip_file_upload(
                        file_exists_action,
                        &local_meta,
                        remote_size,
                        remote_modified,
                    ) {
                        result.files_skipped += 1;
                        let _ = app.emit(
                            "transfer_event",
                            TransferEvent {
                                event_type: "file_skip".to_string(),
                                transfer_id: transfer_id.to_string(),
                                filename: item.name.clone(),
                                direction: "upload".to_string(),
                                message: Some(format!("Skipped (identical): {}", item.name)),
                                progress: None,
                                path: Some(item.remote_path.clone()),
                                delta_stats: None,
                                fallback_reason: None,
                            },
                        );
                        continue;
                    }
                }
            }
        }

        result.entries.push(transfer_domain::TransferEntry {
            id: format!("ul-{}-{}", transfer_id, file_index),
            display_name: item.name,
            remote_path: item.remote_path,
            local_path: item.local_path.to_string_lossy().to_string(),
            size: item.size,
            modified: None,
        });
        file_index += 1;
    }

    Ok(result)
}

#[tauri::command]
async fn upload_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    params: UploadFolderParams,
) -> Result<String, String> {
    let runtime_settings = transfer_settings::resolve_ftp_transfer_settings(
        transfer_settings::TransferSettingsInput {
            max_concurrent: params.max_concurrent,
            retry_count: params.retry_count,
            timeout_seconds: params.timeout_seconds,
            // GTC-1: FTP GUI batch stays on `FtpDownloadExecutor`
            // (no-double-pool invariant); the segments knob only
            // matters on the `ProviderDownloadExecutor` path.
            download_segments: None,
        },
    );
    info!(
        "Uploading folder recursively: {} -> {} (concurrency={}, retries={}, timeout={}s)",
        params.local_path,
        params.remote_path,
        runtime_settings.max_concurrent,
        runtime_settings.retry_count,
        runtime_settings.timeout_seconds
    );

    let cancel_token = state.reset_cancel_state().await;

    let folder_name = PathBuf::from(&params.local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());

    let transfer_id = format!("ul-folder-{}", chrono::Utc::now().timestamp_millis());

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Scanning folder: {}", folder_name)),
            progress: None,
            path: Some(params.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let local_base_path = PathBuf::from(&params.local_path);
    if !local_base_path.is_dir() {
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "error".to_string(),
                transfer_id: transfer_id.clone(),
                filename: folder_name.clone(),
                direction: "upload".to_string(),
                message: Some("Source is not a directory".to_string()),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        return Err("Source is not a directory".to_string());
    }

    let (prep_result, remote_base_path, connection_spec) = {
        let mut ftp_manager = state.ftp_manager.lock().await;
        ftp_manager.apply_transfer_timeout(runtime_settings.timeout_seconds);
        let current_remote_path = ftp_manager.current_path();
        let remote_base_path = if params.remote_path.is_empty() || params.remote_path == "." {
            if current_remote_path == "/" {
                format!("/{}", folder_name)
            } else {
                format!("{}/{}", current_remote_path, folder_name)
            }
        } else {
            params.remote_path.clone()
        };

        let prep_result = prepare_ftp_upload_entries(
            &app,
            &state.cancel_flag,
            &mut ftp_manager,
            &local_base_path,
            &remote_base_path,
            params.file_exists_action.as_str(),
            &transfer_id,
            &folder_name,
        )
        .await?;

        let mut connection_spec = ftp_manager
            .connection_spec()
            .map_err(|e| format!("Failed to derive FTP pool config: {}", e))?;
        connection_spec.initial_path = current_remote_path;

        (prep_result, remote_base_path, connection_spec)
    };

    if prep_result.cancelled {
        return Ok("Upload cancelled after 0 files".to_string());
    }

    let batch = transfer_orchestrator::TransferBatch {
        id: transfer_id.clone(),
        display_name: folder_name.clone(),
        direction: transfer_domain::TransferDirection::Upload,
        config: transfer_domain::TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
        },
        entries: prep_result.entries,
    };

    let pool = Arc::new(
        ftp_session_pool::FtpSessionPool::create(ftp_session_pool::FtpPoolConfig::from_connection(
            connection_spec,
            runtime_settings.max_concurrent.max(1) as usize,
            1,
            runtime_settings.timeout_seconds * 1000,
        ))
        .await
        .map_err(|e| format!("Failed to create FTP session pool: {}", e))?,
    );

    let progress_app = app.clone();
    let total_files_for_progress = prep_result.total_files_discovered;
    let initial_skipped = prep_result.files_skipped;
    let progress_transfer_id = transfer_id.clone();
    let progress_folder_name = folder_name.clone();
    let progress_remote_path = remote_base_path.clone();
    let progress_observer: transfer_orchestrator::ProgressObserver = Arc::new(move |snapshot| {
        let processed = initial_skipped + snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        let _ = progress_app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_folder_name.clone(),
                direction: "upload".to_string(),
                message: Some(format!(
                    "Uploaded {} / {} files ({} skipped, {} errors)",
                    snapshot.completed, total_files_for_progress, initial_skipped, snapshot.failed
                )),
                progress: Some(TransferProgress {
                    transfer_id: progress_transfer_id.clone(),
                    filename: progress_folder_name.clone(),
                    transferred: processed as u64,
                    total: total_files_for_progress as u64,
                    percentage,
                    speed_bps: 0,
                    eta_seconds: 0,
                    direction: "upload".to_string(),
                    total_files: Some(total_files_for_progress as u64),
                    path: Some(progress_remote_path.clone()),
                }),
                path: Some(progress_remote_path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    });

    let executor = Arc::new(ftp_transfer_executor::FtpUploadExecutor::new(
        app.clone(),
        pool.clone(),
        runtime_settings,
        cancel_token,
    ));

    // PD-CLI-CONV-B: the orchestrator is now sink-agnostic. The FTP
    // executors keep their own `AppHandle` for `transfer_event`; only the
    // three batch-lifecycle events move behind the sink. `AppHandleSink`
    // is a 1:1 adapter, so the emitted payloads are byte-identical.
    let batch_sink: std::sync::Arc<dyn crate::transfer_event_sink::TransferEventSink> =
        std::sync::Arc::new(crate::transfer_event_sink::AppHandleSink::new(app.clone()));
    let batch_result = transfer_orchestrator::execute_batch(
        batch_sink,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    if let Err(e) = pool.close().await {
        warn!(
            "Failed to close FTP session pool cleanly after upload: {}",
            e
        );
    }

    let files_uploaded = batch_result.completed;
    let files_errored = batch_result.failed + prep_result.scan_errors;
    let result_message = if batch_result.cancelled {
        format!(
            "Upload cancelled after {} files",
            files_uploaded + prep_result.files_skipped + files_errored
        )
    } else {
        format!(
            "Uploaded {} files, {} skipped, {} errors",
            files_uploaded, prep_result.files_skipped, files_errored
        )
    };

    let _ = app.emit(
        "transfer_event",
        TransferEvent {
            event_type: if batch_result.cancelled {
                "cancelled".to_string()
            } else {
                "complete".to_string()
            },
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "upload".to_string(),
            message: Some(result_message.clone()),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        },
    );

    Ok(result_message)
}

#[tauri::command]
async fn cancel_transfer(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
) -> Result<(), String> {
    // Set cancel flag on both FTP and provider states
    state.request_cancel().await;
    provider_state.request_cancel().await;
    info!("Transfer cancellation requested");
    Ok(())
}

#[tauri::command]
async fn reset_cancel_flag(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
) -> Result<(), String> {
    state.reset_cancel_state().await;
    provider_state.reset_cancel_state().await;
    Ok(())
}

// ============ Bandwidth Throttling ============

/// Set global transfer speed limits (KB/s, 0 = unlimited)
#[tauri::command]
async fn set_speed_limit(
    state: State<'_, AppState>,
    download_kb: u64,
    upload_kb: u64,
) -> Result<(), String> {
    state
        .speed_limits
        .download_bps
        .store(download_kb * 1024, std::sync::atomic::Ordering::Relaxed);
    state
        .speed_limits
        .upload_bps
        .store(upload_kb * 1024, std::sync::atomic::Ordering::Relaxed);
    info!(
        "Speed limits set: download={}KB/s upload={}KB/s (0=unlimited)",
        download_kb, upload_kb
    );
    Ok(())
}

/// Get current global transfer speed limits (KB/s)
#[tauri::command]
async fn get_speed_limit(state: State<'_, AppState>) -> Result<(u64, u64), String> {
    let dl = state
        .speed_limits
        .download_bps
        .load(std::sync::atomic::Ordering::Relaxed)
        / 1024;
    let ul = state
        .speed_limits
        .upload_bps
        .load(std::sync::atomic::Ordering::Relaxed)
        / 1024;
    Ok((dl, ul))
}

// ============ Environment Detection ============

/// Check if the application is running as a Snap package
#[tauri::command]
fn is_running_as_snap() -> bool {
    std::env::var("SNAP").is_ok()
}

// ============ Debug & Dependencies Commands ============

#[derive(Clone, serde::Serialize)]
struct DependencyInfo {
    name: String,
    version: String,
    category: String,
}

#[derive(Clone, serde::Serialize)]
struct CrateVersionResult {
    name: String,
    latest_version: Option<String>,
    error: Option<String>,
}

#[derive(Clone, serde::Serialize)]
struct SystemInfo {
    app_version: String,
    os: String,
    os_version: String,
    arch: String,
    tauri_version: String,
    rust_version: String,
    keyring_backend: String,
    config_dir: String,
    vault_exists: bool,
    known_hosts_exists: bool,
    dep_versions: std::collections::HashMap<String, String>,
}

#[tauri::command]
fn get_dependencies() -> Vec<DependencyInfo> {
    vec![
        // Core Framework (versions from Cargo.lock via build.rs)
        DependencyInfo {
            name: "tauri".into(),
            version: env!("DEP_VERSION_TAURI").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "tokio".into(),
            version: env!("DEP_VERSION_TOKIO").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "serde".into(),
            version: env!("DEP_VERSION_SERDE").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "serde_json".into(),
            version: env!("DEP_VERSION_SERDE_JSON").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "anyhow".into(),
            version: env!("DEP_VERSION_ANYHOW").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "thiserror".into(),
            version: env!("DEP_VERSION_THISERROR").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "chrono".into(),
            version: env!("DEP_VERSION_CHRONO").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "log".into(),
            version: env!("DEP_VERSION_LOG").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "tracing".into(),
            version: env!("DEP_VERSION_TRACING").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "portable-pty".into(),
            version: env!("DEP_VERSION_PORTABLE_PTY").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "notify".into(),
            version: env!("DEP_VERSION_NOTIFY").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "image".into(),
            version: env!("DEP_VERSION_IMAGE").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "tokio-util".into(),
            version: env!("DEP_VERSION_TOKIO_UTIL").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "futures-util".into(),
            version: env!("DEP_VERSION_FUTURES_UTIL").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "async-trait".into(),
            version: env!("DEP_VERSION_ASYNC_TRAIT").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "tracing-subscriber".into(),
            version: env!("DEP_VERSION_TRACING_SUBSCRIBER").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "toml".into(),
            version: env!("DEP_VERSION_TOML").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "semver".into(),
            version: env!("DEP_VERSION_SEMVER").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "uuid".into(),
            version: env!("DEP_VERSION_UUID").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "regex".into(),
            version: env!("DEP_VERSION_REGEX").into(),
            category: "Core".into(),
        },
        DependencyInfo {
            name: "notify-debouncer-full".into(),
            version: env!("DEP_VERSION_NOTIFY_DEBOUNCER_FULL").into(),
            category: "Core".into(),
        },
        // Protocols
        DependencyInfo {
            name: "suppaftp".into(),
            version: env!("DEP_VERSION_SUPPAFTP").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "russh".into(),
            version: env!("DEP_VERSION_RUSSH").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "russh-sftp".into(),
            version: env!("DEP_VERSION_RUSSH_SFTP").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "reqwest".into(),
            version: env!("DEP_VERSION_REQWEST").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "quick-xml".into(),
            version: env!("DEP_VERSION_QUICK_XML").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "oauth2".into(),
            version: env!("DEP_VERSION_OAUTH2").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "rustls".into(),
            version: env!("DEP_VERSION_RUSTLS").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "ssh2".into(),
            version: env!("DEP_VERSION_SSH2").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "tokio-rustls".into(),
            version: env!("DEP_VERSION_TOKIO_RUSTLS").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "rustls-native-certs".into(),
            version: env!("DEP_VERSION_RUSTLS_NATIVE_CERTS").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "webpki-roots".into(),
            version: env!("DEP_VERSION_WEBPKI_ROOTS").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "axum".into(),
            version: env!("DEP_VERSION_AXUM").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "http".into(),
            version: env!("DEP_VERSION_HTTP").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "url".into(),
            version: env!("DEP_VERSION_URL").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "urlencoding".into(),
            version: env!("DEP_VERSION_URLENCODING").into(),
            category: "Protocols".into(),
        },
        DependencyInfo {
            name: "percent-encoding".into(),
            version: env!("DEP_VERSION_PERCENT_ENCODING").into(),
            category: "Protocols".into(),
        },
        // Security
        DependencyInfo {
            name: "argon2".into(),
            version: env!("DEP_VERSION_ARGON2").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "aes-gcm".into(),
            version: env!("DEP_VERSION_AES_GCM").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "aes-gcm-siv".into(),
            version: env!("DEP_VERSION_AES_GCM_SIV").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "chacha20poly1305".into(),
            version: env!("DEP_VERSION_CHACHA20POLY1305").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "hkdf".into(),
            version: env!("DEP_VERSION_HKDF").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "aes-kw".into(),
            version: env!("DEP_VERSION_AES_KW").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "aes-siv".into(),
            version: env!("DEP_VERSION_AES_SIV").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "scrypt".into(),
            version: env!("DEP_VERSION_SCRYPT").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "ring".into(),
            version: env!("DEP_VERSION_RING").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "secrecy".into(),
            version: env!("DEP_VERSION_SECRECY").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "sha2".into(),
            version: env!("DEP_VERSION_SHA2").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "hmac".into(),
            version: env!("DEP_VERSION_HMAC").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "blake3".into(),
            version: env!("DEP_VERSION_BLAKE3").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "jsonwebtoken".into(),
            version: env!("DEP_VERSION_JSONWEBTOKEN").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "aerovault".into(),
            version: env!("DEP_VERSION_AEROVAULT").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "keyring".into(),
            version: env!("DEP_VERSION_KEYRING").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "aes".into(),
            version: env!("DEP_VERSION_AES").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "cbc".into(),
            version: env!("DEP_VERSION_CBC").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "ctr".into(),
            version: env!("DEP_VERSION_CTR").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "crypto_secretbox".into(),
            version: env!("DEP_VERSION_CRYPTO_SECRETBOX").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "pbkdf2".into(),
            version: env!("DEP_VERSION_PBKDF2").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "sha1".into(),
            version: env!("DEP_VERSION_SHA1").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "ripemd".into(),
            version: env!("DEP_VERSION_RIPEMD").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "md-5".into(),
            version: env!("DEP_VERSION_MD_5").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "zeroize".into(),
            version: env!("DEP_VERSION_ZEROIZE").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "subtle".into(),
            version: env!("DEP_VERSION_SUBTLE").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "data-encoding".into(),
            version: env!("DEP_VERSION_DATA_ENCODING").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "base64".into(),
            version: env!("DEP_VERSION_BASE64").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "hex".into(),
            version: env!("DEP_VERSION_HEX").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "num-bigint-dig".into(),
            version: env!("DEP_VERSION_NUM_BIGINT_DIG").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "totp-rs".into(),
            version: env!("DEP_VERSION_TOTP_RS").into(),
            category: "Security".into(),
        },
        DependencyInfo {
            name: "sigstore".into(),
            version: env!("DEP_VERSION_SIGSTORE").into(),
            category: "Security".into(),
        },
        // Archives
        DependencyInfo {
            name: "sevenz-rust2".into(),
            version: env!("DEP_VERSION_SEVENZ_RUST2").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "zip".into(),
            version: env!("DEP_VERSION_ZIP").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "tar".into(),
            version: env!("DEP_VERSION_TAR").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "flate2".into(),
            version: env!("DEP_VERSION_FLATE2").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "xz2".into(),
            version: env!("DEP_VERSION_XZ2").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "bzip2".into(),
            version: env!("DEP_VERSION_BZIP2").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "unrar".into(),
            version: env!("DEP_VERSION_UNRAR").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "zstd".into(),
            version: env!("DEP_VERSION_ZSTD").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "reed-solomon-erasure".into(),
            version: env!("DEP_VERSION_REED_SOLOMON_ERASURE").into(),
            category: "Archives".into(),
        },
        DependencyInfo {
            name: "xxhash-rust".into(),
            version: env!("DEP_VERSION_XXHASH_RUST").into(),
            category: "Archives".into(),
        },
        // CLI & Tools
        DependencyInfo {
            name: "clap".into(),
            version: env!("DEP_VERSION_CLAP").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "clap_complete".into(),
            version: env!("DEP_VERSION_CLAP_COMPLETE").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "indicatif".into(),
            version: env!("DEP_VERSION_INDICATIF").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "rpassword".into(),
            version: env!("DEP_VERSION_RPASSWORD").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "ctrlc".into(),
            version: env!("DEP_VERSION_CTRLC").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "globset".into(),
            version: env!("DEP_VERSION_GLOBSET").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "ratatui".into(),
            version: env!("DEP_VERSION_RATATUI").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "crossterm".into(),
            version: env!("DEP_VERSION_CROSSTERM").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "libunftp".into(),
            version: env!("DEP_VERSION_LIBUNFTP").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "unftp-core".into(),
            version: env!("DEP_VERSION_UNFTP_CORE").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "rusqlite".into(),
            version: env!("DEP_VERSION_RUSQLITE").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "dirs".into(),
            version: env!("DEP_VERSION_DIRS").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "filetime".into(),
            version: env!("DEP_VERSION_FILETIME").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "tempfile".into(),
            version: env!("DEP_VERSION_TEMPFILE").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "walkdir".into(),
            version: env!("DEP_VERSION_WALKDIR").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "mime_guess".into(),
            version: env!("DEP_VERSION_MIME_GUESS").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "open".into(),
            version: env!("DEP_VERSION_OPEN").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "similar".into(),
            version: env!("DEP_VERSION_SIMILAR").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "trash".into(),
            version: env!("DEP_VERSION_TRASH").into(),
            category: "CLI & Tools".into(),
        },
        DependencyInfo {
            name: "arboard".into(),
            version: env!("DEP_VERSION_ARBOARD").into(),
            category: "CLI & Tools".into(),
        },
        // System
        DependencyInfo {
            name: "libc".into(),
            version: env!("DEP_VERSION_LIBC").into(),
            category: "System".into(),
        },
        DependencyInfo {
            name: "windows".into(),
            version: env!("DEP_VERSION_WINDOWS").into(),
            category: "System".into(),
        },
        DependencyInfo {
            name: "winreg".into(),
            version: env!("DEP_VERSION_WINREG").into(),
            category: "System".into(),
        },
        DependencyInfo {
            name: "fuser".into(),
            version: env!("DEP_VERSION_FUSER").into(),
            category: "System".into(),
        },
        DependencyInfo {
            name: "gtk".into(),
            version: env!("DEP_VERSION_GTK").into(),
            category: "System".into(),
        },
        DependencyInfo {
            name: "hound".into(),
            version: env!("DEP_VERSION_HOUND").into(),
            category: "System".into(),
        },
        DependencyInfo {
            name: "whisper-rs".into(),
            version: env!("DEP_VERSION_WHISPER_RS").into(),
            category: "System".into(),
        },
        // Tauri Plugins
        DependencyInfo {
            name: "tauri-plugin-fs".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_FS").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-dialog".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_DIALOG").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-shell".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_SHELL").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-notification".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_NOTIFICATION").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-log".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_LOG").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-single-instance".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_SINGLE_INSTANCE").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-localhost".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_LOCALHOST").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-autostart".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_AUTOSTART").into(),
            category: "Plugins".into(),
        },
        DependencyInfo {
            name: "tauri-plugin-window-state".into(),
            version: env!("DEP_VERSION_TAURI_PLUGIN_WINDOW_STATE").into(),
            category: "Plugins".into(),
        },
    ]
}

#[tauri::command]
async fn check_crate_versions(crate_names: Vec<String>) -> Vec<CrateVersionResult> {
    let client = reqwest::Client::builder()
        .user_agent("AeroFTP (https://github.com/axpdev-lab/aeroftp)")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut results = Vec::new();
    for chunk in crate_names.chunks(5) {
        let mut handles = Vec::new();
        for name in chunk {
            let client = client.clone();
            let name = name.clone();
            handles.push(tokio::spawn(async move {
                match client
                    .get(format!("https://crates.io/api/v1/crates/{}", name))
                    .send()
                    .await
                {
                    Ok(res) if res.status().is_success() => {
                        match res.json::<serde_json::Value>().await {
                            Ok(data) => {
                                // Prefer max_stable_version to skip pre-releases (beta, rc, alpha)
                                let version = data["crate"]["max_stable_version"]
                                    .as_str()
                                    .or_else(|| data["crate"]["newest_version"].as_str())
                                    .or_else(|| data["crate"]["max_version"].as_str())
                                    .map(|s| s.to_string());
                                CrateVersionResult {
                                    name,
                                    latest_version: version,
                                    error: None,
                                }
                            }
                            Err(e) => CrateVersionResult {
                                name,
                                latest_version: None,
                                error: Some(format!("Parse error: {}", e)),
                            },
                        }
                    }
                    Ok(res) => CrateVersionResult {
                        name,
                        latest_version: None,
                        error: Some(format!("HTTP {}", res.status())),
                    },
                    Err(e) => CrateVersionResult {
                        name,
                        latest_version: None,
                        error: Some(format!("{}", e)),
                    },
                }
            }));
        }
        for handle in handles {
            if let Ok(result) = handle.await {
                results.push(result);
            }
        }
        // Small delay between batches
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    results
}

#[tauri::command]
fn get_system_info() -> SystemInfo {
    let config_dir = portable::aeroftp_data_root()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let vault_exists = portable::aeroftp_data_root()
        .map(|d| d.join("vault.db").exists())
        .unwrap_or(false);

    let known_hosts_exists = dirs::home_dir()
        .map(|d| d.join(".ssh").join("known_hosts").exists())
        .unwrap_or(false);

    let keyring_backend = if cfg!(target_os = "linux") {
        // Detect actual keyring provider from desktop environment.
        // The `keyring` crate uses the D-Bus Secret Service API (org.freedesktop.secrets),
        // which is provided by different daemons depending on the DE:
        // KDE → kwalletd, GNOME/XFCE/MATE/Cinnamon → gnome-keyring
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let desktop_upper = desktop.to_uppercase();
        if desktop_upper.contains("KDE") {
            "KDE Wallet (Secret Service API)"
        } else if desktop_upper.contains("GNOME")
            || desktop_upper.contains("UNITY")
            || desktop_upper.contains("CINNAMON")
            || desktop_upper.contains("MATE")
            || desktop_upper.contains("XFCE")
        {
            "GNOME Keyring (Secret Service API)"
        } else {
            "Secret Service API (D-Bus)"
        }
    } else if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else if cfg!(target_os = "windows") {
        "Windows Credential Manager"
    } else {
        "unknown"
    };

    let mut dep_versions = std::collections::HashMap::new();
    dep_versions.insert("russh".into(), env!("DEP_VERSION_RUSSH").into());
    dep_versions.insert("russh-sftp".into(), env!("DEP_VERSION_RUSSH_SFTP").into());
    dep_versions.insert("suppaftp".into(), env!("DEP_VERSION_SUPPAFTP").into());
    dep_versions.insert("reqwest".into(), env!("DEP_VERSION_REQWEST").into());
    dep_versions.insert("aes-gcm".into(), env!("DEP_VERSION_AES_GCM").into());
    dep_versions.insert("argon2".into(), env!("DEP_VERSION_ARGON2").into());
    dep_versions.insert("zip".into(), env!("DEP_VERSION_ZIP").into());
    dep_versions.insert(
        "sevenz-rust2".into(),
        env!("DEP_VERSION_SEVENZ_RUST2").into(),
    );
    dep_versions.insert("quick-xml".into(), env!("DEP_VERSION_QUICK_XML").into());
    dep_versions.insert("oauth2".into(), env!("DEP_VERSION_OAUTH2").into());
    dep_versions.insert("aes-gcm-siv".into(), env!("DEP_VERSION_AES_GCM_SIV").into());
    dep_versions.insert(
        "chacha20poly1305".into(),
        env!("DEP_VERSION_CHACHA20POLY1305").into(),
    );
    dep_versions.insert("hkdf".into(), env!("DEP_VERSION_HKDF").into());
    dep_versions.insert("aes-kw".into(), env!("DEP_VERSION_AES_KW").into());
    dep_versions.insert("aes-siv".into(), env!("DEP_VERSION_AES_SIV").into());
    dep_versions.insert("scrypt".into(), env!("DEP_VERSION_SCRYPT").into());
    dep_versions.insert("blake3".into(), env!("DEP_VERSION_BLAKE3").into());
    dep_versions.insert("rustls".into(), env!("DEP_VERSION_RUSTLS").into());
    dep_versions.insert("aerovault".into(), env!("DEP_VERSION_AEROVAULT").into());
    dep_versions.insert("keyring".into(), env!("DEP_VERSION_KEYRING").into());

    SystemInfo {
        app_version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        os_version: std::env::consts::ARCH.into(),
        arch: std::env::consts::ARCH.into(),
        tauri_version: env!("DEP_VERSION_TAURI").into(),
        rust_version: env!("RUSTC_VERSION").into(),
        keyring_backend: keyring_backend.into(),
        config_dir,
        vault_exists,
        known_hosts_exists,
        dep_versions,
    }
}

// ============ Local File System Commands ============

#[tauri::command]
async fn get_local_files(
    path: String,
    show_hidden: Option<bool>,
) -> Result<Vec<LocalFileInfo>, String> {
    validate_path(&path)?;
    let path = PathBuf::from(&path);
    let show_hidden = show_hidden.unwrap_or(true); // Developer-first: show all files by default

    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }

    let mut files = Vec::new();

    // Parent directory (..) removed - use "Up" button in toolbar for navigation

    let mut entries = tokio::fs::read_dir(&path)
        .await
        .map_err(|e| format!("Failed to read directory: {}", e))?;

    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        let metadata = entry.metadata().await.ok();
        let file_name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files unless show_hidden is enabled
        if !show_hidden && file_name.starts_with('.') {
            continue;
        }

        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = if is_dir {
            None
        } else {
            metadata.as_ref().map(|m| m.len())
        };

        let modified = metadata.as_ref().and_then(|m| {
            m.modified().ok().map(|t| {
                let datetime: chrono::DateTime<chrono::Local> = t.into();
                datetime.format("%Y-%m-%d %H:%M").to_string()
            })
        });

        files.push(LocalFileInfo {
            name: file_name,
            path: entry.path().to_string_lossy().replace('\\', "/"),
            size,
            is_dir,
            modified,
        });
    }

    // Sort: directories first, then alphabetically
    files.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(files)
}

/// BFS-flatten the directory tree rooted at `base_path`. Returns one entry per
/// descendant file/dir with the `name` field carrying the relative path from
/// `base_path` (e.g. `subdir/file.txt`) so the existing local panel rendering
/// can surface depth without a schema change. Capped at `max_entries`
/// (default 5000) to keep the UI responsive on large trees, with `truncated`
/// signalling that the cap was hit.
#[derive(Serialize)]
pub struct FlattenLocalResult {
    pub entries: Vec<LocalFileInfo>,
    pub truncated: bool,
    pub total_scanned: u64,
}

#[tauri::command]
async fn flatten_local_descendants(
    base_path: String,
    max_entries: Option<u32>,
    show_hidden: Option<bool>,
) -> Result<FlattenLocalResult, String> {
    use std::collections::VecDeque;

    validate_path(&base_path)?;
    let base = PathBuf::from(&base_path);
    if !base.exists() {
        return Err(format!("Path does not exist: {}", base.display()));
    }
    if !base.is_dir() {
        return Err(format!("Not a directory: {}", base.display()));
    }
    let cap = max_entries.unwrap_or(5000).min(20_000) as usize;
    let show_hidden = show_hidden.unwrap_or(false);

    // BFS with a max depth ceiling to avoid runaway scans on symlink loops.
    const MAX_DEPTH: usize = 32;

    let mut entries: Vec<LocalFileInfo> = Vec::new();
    let mut total_scanned: u64 = 0;
    let mut truncated = false;
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((base.clone(), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            continue;
        }
        let mut read = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        loop {
            match read.next_entry().await {
                Ok(Some(entry)) => {
                    total_scanned += 1;
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    if !show_hidden && file_name.starts_with('.') {
                        continue;
                    }
                    let entry_path = entry.path();
                    let metadata = match tokio::fs::symlink_metadata(&entry_path).await {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if metadata.file_type().is_symlink() {
                        // Skip symlinks to avoid cycles. We could follow with a
                        // visited-set, but symlink loops are the common case
                        // and a single-step skip is the safe default.
                        continue;
                    }
                    let is_dir = metadata.is_dir();
                    // Compute relative path. base.join(rel) == entry_path, so
                    // strip_prefix gives us the right substring with the OS
                    // separator. Normalize to forward slashes for the UI.
                    let relative = entry_path
                        .strip_prefix(&base)
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|_| file_name.clone());

                    let modified = metadata.modified().ok().map(|t| {
                        let datetime: chrono::DateTime<chrono::Local> = t.into();
                        datetime.format("%Y-%m-%d %H:%M").to_string()
                    });

                    entries.push(LocalFileInfo {
                        name: relative,
                        path: entry_path.to_string_lossy().replace('\\', "/"),
                        size: if is_dir { None } else { Some(metadata.len()) },
                        is_dir,
                        modified,
                    });

                    if entries.len() >= cap {
                        truncated = true;
                        break;
                    }

                    if is_dir {
                        queue.push_back((entry_path, depth + 1));
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        if truncated {
            break;
        }
    }

    // Sort by relative path so siblings cluster naturally.
    entries.sort_by_key(|e| e.name.to_lowercase());

    Ok(FlattenLocalResult {
        entries,
        truncated,
        total_scanned,
    })
}

#[tauri::command]
async fn open_in_file_manager(path: String) -> Result<(), String> {
    validate_path(&path)?;
    #[cfg(target_os = "linux")]
    {
        // Reveal the file in the file manager (select it), to match the Windows
        // `/select,` and macOS `-R` behaviour. Plain `xdg-open` on a file would
        // OPEN it in its default app instead of revealing it, so for a file we
        // ask the file manager to show-and-select it via the D-Bus
        // org.freedesktop.FileManager1.ShowItems method (Nautilus, Dolphin,
        // Nemo, Caja), falling back to opening the parent directory when that
        // service is unavailable (minimal distros). Directories open directly.
        let is_file = std::fs::metadata(&path)
            .map(|m| m.is_file())
            .unwrap_or(false);
        if is_file {
            // Percent-encode the path into a file:// URI (keep `/` separators),
            // so names with spaces or quotes cannot break the D-Bus array literal.
            let encoded: String = path
                .chars()
                .map(|c| match c {
                    'A'..='Z' | 'a'..='z' | '0'..='9' | '/' | '-' | '_' | '.' | '~' => {
                        c.to_string()
                    }
                    _ => c
                        .to_string()
                        .bytes()
                        .map(|b| format!("%{:02X}", b))
                        .collect(),
                })
                .collect();
            let uri = format!("file://{}", encoded);
            let revealed = std::process::Command::new("gdbus")
                .args([
                    "call",
                    "--session",
                    "--dest",
                    "org.freedesktop.FileManager1",
                    "--object-path",
                    "/org/freedesktop/FileManager1",
                    "--method",
                    "org.freedesktop.FileManager1.ShowItems",
                    &format!("[\"{}\"]", uri),
                    "",
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !revealed {
                let parent = std::path::Path::new(&path)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                std::process::Command::new("xdg-open")
                    .arg(&parent)
                    .spawn()
                    .map_err(|e| format!("Failed to open file manager: {}", e))?;
            }
        } else {
            std::process::Command::new("xdg-open")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Use /select, for files or plain path for directories
        let normalized = path.replace('/', "\\");
        let metadata = std::fs::metadata(&normalized);
        if metadata.map(|m| m.is_file()).unwrap_or(false) {
            std::process::Command::new("explorer")
                .args(["/select,", &normalized])
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        } else {
            std::process::Command::new("explorer")
                .arg(&normalized)
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        }
    }

    #[cfg(target_os = "macos")]
    {
        let metadata = std::fs::metadata(&path);
        if metadata.map(|m| m.is_file()).unwrap_or(false) {
            // Use -R to reveal file in Finder (selects it)
            std::process::Command::new("open")
                .args(["-R", &path])
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        } else {
            std::process::Command::new("open")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        }
    }

    Ok(())
}

#[tauri::command]
async fn open_local_file(path: String) -> Result<(), String> {
    validate_path(&path)?;
    let metadata =
        std::fs::metadata(&path).map_err(|_| "Failed to read file metadata".to_string())?;
    if !metadata.is_file() {
        return Err("Path is not a file".to_string());
    }

    open::that(&path).map_err(|e| format!("Failed to open file: {}", e))?;
    Ok(())
}

/// Resolve a directory that is SAFE to hand to a native folder picker as its
/// starting location.
///
/// A profile's stored local path is machine-relative: after importing a profile
/// from another machine (or deleting the folder, or a manual typo), that path may
/// not exist here. Passing a non-existent `defaultPath` to the GTK file chooser
/// crashes the app (heap corruption), same class of bug as the tray "open
/// AeroCloud folder" link on an inactive mount. Fix it at the point of use: never
/// let a path that does not exist reach the native dialog.
///
/// If `path` exists and is a directory it is returned unchanged; otherwise we
/// walk up to the nearest existing ancestor directory so the picker opens near
/// where the user intended. Returns `None` when nothing valid is found (e.g. a
/// foreign absolute path whose whole chain is absent), so the caller omits
/// `defaultPath` and the picker opens at the OS default location.
#[tauri::command]
fn safe_picker_start_dir(path: Option<String>) -> Option<String> {
    let raw = path?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut current = std::path::Path::new(trimmed);
    loop {
        if current.is_dir() {
            return Some(current.to_string_lossy().into_owned());
        }
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => current = parent,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod safe_picker_start_dir_tests {
    use super::safe_picker_start_dir;

    #[test]
    fn none_and_empty_yield_none() {
        assert_eq!(safe_picker_start_dir(None), None);
        assert_eq!(safe_picker_start_dir(Some(String::new())), None);
        assert_eq!(safe_picker_start_dir(Some("   ".to_string())), None);
    }

    #[test]
    fn existing_directory_is_returned_unchanged() {
        let dir = std::env::temp_dir();
        let got = safe_picker_start_dir(Some(dir.to_string_lossy().into_owned()));
        assert_eq!(got, Some(dir.to_string_lossy().into_owned()));
    }

    #[test]
    fn nonexistent_child_walks_up_to_existing_ancestor() {
        // A stale local path from an imported profile: the leaf does not exist
        // but an ancestor does. The picker must open at the ancestor, never at
        // the missing leaf (which would crash the native dialog).
        let base = std::env::temp_dir();
        let stale = base
            .join("aeroftp-does-not-exist-xyz")
            .join("nested-missing");
        let got = safe_picker_start_dir(Some(stale.to_string_lossy().into_owned()));
        assert_eq!(got, Some(base.to_string_lossy().into_owned()));
    }

    #[test]
    fn windows_style_path_on_unix_has_no_ancestor_and_yields_none() {
        // On a Unix host, backslashes are not separators, so an imported Windows
        // local path is one opaque component with no existing ancestor: the
        // caller omits defaultPath and the picker opens at the OS default.
        #[cfg(unix)]
        {
            let got = safe_picker_start_dir(Some(r"C:\Users\other\Downloads\AeroFTP".to_string()));
            assert_eq!(got, None);
        }
    }

    #[test]
    fn result_is_always_none_or_an_existing_directory() {
        // The safety invariant the whole fix rests on: whatever we return is
        // never a non-existent path, so the native folder chooser can never be
        // handed a missing directory (the crash condition).
        for candidate in [
            "",
            "   ",
            "/aeroftp-nonexistent-root-abc/sub/leaf",
            r"C:\Windows\Imported\Path",
            "relative/does/not/exist",
        ] {
            if let Some(dir) = safe_picker_start_dir(Some(candidate.to_string())) {
                assert!(
                    std::path::Path::new(&dir).is_dir(),
                    "returned start dir {dir:?} for input {candidate:?} must exist",
                );
            }
        }
    }
}

// ============ File Operations Commands ============

/// Delete a remote file or folder with detailed event emission for each deleted item.
/// For folders, recursively scans and emits events for each file deleted.
#[tauri::command]
async fn delete_remote_file(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    is_dir: bool,
) -> Result<String, String> {
    let mut ftp_manager = state.ftp_manager.lock().await;

    let file_name = path.split('/').next_back().unwrap_or(&path).to_string();
    let delete_id = format!("del-remote-{}", chrono::Utc::now().timestamp_millis());

    if !is_dir {
        // Single file delete - simple case
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "delete_start".to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "remote".to_string(),
                message: Some(format!("Deleting remote file: {}", file_name)),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        match ftp_manager.remove(&path).await {
            Ok(_) => {
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "delete_complete".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: file_name.clone(),
                        direction: "remote".to_string(),
                        message: Some(format!("Deleted remote file: {}", file_name)),
                        progress: None,
                        path: Some(path.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                Ok(format!("Deleted: {}", file_name))
            }
            Err(e) => {
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "delete_error".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: file_name.clone(),
                        direction: "remote".to_string(),
                        message: Some(format!("Failed to delete: {}", e)),
                        progress: None,
                        path: Some(path.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                Err(format!("Failed to delete file: {}", e))
            }
        }
    } else {
        // Folder delete - scan first, then delete with events
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "delete_start".to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "remote".to_string(),
                message: Some(format!("Scanning remote folder: {}", file_name)),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        let original_path = ftp_manager.current_path();

        // Build absolute target path
        let target_path = if path.starts_with('/') {
            path.clone()
        } else {
            format!("{}/{}", original_path, path)
        };

        // Phase 1: Collect all files and directories recursively
        struct DeleteItem {
            path: String,
            name: String,
        }

        let mut files_to_delete: Vec<DeleteItem> = Vec::new();
        let mut dirs_to_delete: Vec<String> = Vec::new();
        let mut dirs_to_scan: Vec<String> = vec![target_path.clone()];
        let mut last_scan_emit = std::time::Instant::now();
        let mut scan_counter: usize = 0;

        while let Some(current_dir) = dirs_to_scan.pop() {
            if ftp_manager.change_dir(&current_dir).await.is_err() {
                continue;
            }

            let files = match ftp_manager.list_files().await {
                Ok(f) => f,
                Err(_) => continue,
            };

            for file in files {
                let file_path = format!("{}/{}", current_dir, file.name);
                scan_counter += 1;

                if file.is_dir {
                    dirs_to_scan.push(file_path.clone());
                } else {
                    files_to_delete.push(DeleteItem {
                        path: file_path,
                        name: file.name,
                    });
                }
            }

            // Add directory to delete list (will be deleted after its contents)
            dirs_to_delete.push(current_dir);

            // Emit scan progress every 500ms or every 100 entries
            if last_scan_emit.elapsed().as_millis() > 500 || scan_counter % 100 == 0 {
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "scanning".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: file_name.clone(),
                        direction: "remote".to_string(),
                        message: Some(format!(
                            "Scanning... {} files, {} folders found",
                            files_to_delete.len(),
                            dirs_to_delete.len()
                        )),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                last_scan_emit = std::time::Instant::now();
            }
        }

        let total_files = files_to_delete.len();
        let total_dirs = dirs_to_delete.len();

        info!(
            "Found {} files and {} directories to delete in {}",
            total_files, total_dirs, file_name
        );

        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "scanning".to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "remote".to_string(),
                message: Some(format!(
                    "Scan complete: {} files in {} folders to delete",
                    total_files, total_dirs
                )),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );

        // Phase 2: Delete all files with events (cancellable)
        let mut deleted_files = 0u64;
        let mut errors = 0u64;
        let mut cancelled = false;

        for item in &files_to_delete {
            // Check cancel flag before each file
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                info!(
                    "Folder deletion cancelled by user after {} files",
                    deleted_files
                );
                break;
            }
            let file_delete_id = format!("{}-file-{}", delete_id, deleted_files);

            let _ = app.emit(
                "transfer_event",
                TransferEvent {
                    event_type: "delete_file_start".to_string(),
                    transfer_id: file_delete_id.clone(),
                    filename: item.name.clone(),
                    direction: "remote".to_string(),
                    message: Some(format!("Deleting: {}", item.path)),
                    progress: None,
                    path: Some(item.path.clone()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );

            match ftp_manager.remove(&item.path).await {
                Ok(_) => {
                    deleted_files += 1;
                    let _ = app.emit(
                        "transfer_event",
                        TransferEvent {
                            event_type: "delete_file_complete".to_string(),
                            transfer_id: file_delete_id,
                            filename: item.name.clone(),
                            direction: "remote".to_string(),
                            message: Some(format!("Deleted: {}", item.name)),
                            progress: None,
                            path: Some(item.path.clone()),
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                }
                Err(e) => {
                    errors += 1;
                    warn!("Failed to delete {}: {}", item.path, e);
                    let _ = app.emit(
                        "transfer_event",
                        TransferEvent {
                            event_type: "delete_file_error".to_string(),
                            transfer_id: file_delete_id,
                            filename: item.name.clone(),
                            direction: "remote".to_string(),
                            message: Some(format!("Failed: {} - {}", item.name, e)),
                            progress: None,
                            path: Some(item.path.clone()),
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                }
            }
        }

        // Phase 3: Delete directories (deepest first - reverse the order!)
        // Directories were added in scan order (parent first), so we need to reverse
        // Skip if cancelled - partial content may remain
        let dirs_reversed: Vec<_> = dirs_to_delete.iter().rev().collect();
        for dir_path in dirs_reversed {
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let dir_name = dir_path.split('/').next_back().unwrap_or(dir_path);
            match ftp_manager.remove_dir(dir_path).await {
                Ok(_) => {
                    let _ = app.emit(
                        "transfer_event",
                        TransferEvent {
                            event_type: "delete_dir_complete".to_string(),
                            transfer_id: delete_id.clone(),
                            filename: dir_name.to_string(),
                            direction: "remote".to_string(),
                            message: Some(format!("Removed folder: {}", dir_name)),
                            progress: None,
                            path: Some(dir_path.to_string()),
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                }
                Err(e) => {
                    warn!("Failed to remove remote directory {}: {}", dir_path, e);
                }
            }
        }

        // Return to original directory
        let _ = ftp_manager.change_dir(&original_path).await;

        // Emit completion
        let result_message = if cancelled {
            format!(
                "Deletion cancelled: {} of {} files deleted",
                deleted_files, total_files
            )
        } else if errors > 0 {
            format!(
                "Deleted {} files ({} errors), {} folders",
                deleted_files, errors, total_dirs
            )
        } else {
            format!("Deleted {} files, {} folders", deleted_files, total_dirs)
        };

        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: if cancelled {
                    "delete_cancelled"
                } else {
                    "delete_complete"
                }
                .to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "remote".to_string(),
                message: Some(result_message.clone()),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        Ok(result_message)
    }
}

/// Delete a local file or folder with detailed event emission for each deleted item.
#[tauri::command]
async fn delete_local_file(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    validate_path(&path)?;
    let path_buf = std::path::PathBuf::from(&path);
    let file_name = path_buf
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    let delete_id = format!("del-local-{}", chrono::Utc::now().timestamp_millis());
    let is_dir = path_buf.is_dir();

    if !is_dir {
        // Single file delete
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "delete_start".to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "local".to_string(),
                message: Some(format!("Deleting local file: {}", file_name)),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        match tokio::fs::remove_file(&path).await {
            Ok(_) => {
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "delete_complete".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: file_name.clone(),
                        direction: "local".to_string(),
                        message: Some(format!("Deleted local file: {}", file_name)),
                        progress: None,
                        path: Some(path.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                Ok(format!("Deleted: {}", file_name))
            }
            Err(e) => {
                let _ = app.emit(
                    "transfer_event",
                    TransferEvent {
                        event_type: "delete_error".to_string(),
                        transfer_id: delete_id.clone(),
                        filename: file_name.clone(),
                        direction: "local".to_string(),
                        message: Some(format!("Failed to delete: {}", e)),
                        progress: None,
                        path: Some(path.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                Err(format!("Failed to delete file: {}", e))
            }
        }
    } else {
        // Folder delete - scan first, then delete with events
        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "delete_start".to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "local".to_string(),
                message: Some(format!("Scanning local folder: {}", file_name)),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        // Phase 1: Collect all files and directories
        struct DeleteItem {
            path: std::path::PathBuf,
            name: String,
        }

        let mut files_to_delete: Vec<DeleteItem> = Vec::new();
        let mut dirs_to_delete: Vec<std::path::PathBuf> = Vec::new();
        let mut dirs_to_scan: Vec<std::path::PathBuf> = vec![path_buf.clone()];
        let mut entry_count: u64 = 0;

        while let Some(current_dir) = dirs_to_scan.pop() {
            let mut read_dir = match tokio::fs::read_dir(&current_dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };

            while let Ok(Some(entry)) = read_dir.next_entry().await {
                entry_count += 1;
                if entry_count > 1_000_000 {
                    return Err("Directory contains too many entries (max 1,000,000). Use terminal for large deletions.".to_string());
                }

                let entry_path = entry.path();
                let entry_name = entry.file_name().to_string_lossy().to_string();

                // Use symlink_metadata to avoid following symlinks
                let metadata = tokio::fs::symlink_metadata(&entry_path)
                    .await
                    .map_err(|e| format!("Failed to read metadata: {}", e))?;
                if metadata.is_symlink() {
                    // For symlinks, delete the link itself, not the target
                    files_to_delete.push(DeleteItem {
                        path: entry_path,
                        name: entry_name,
                    });
                } else if metadata.is_dir() {
                    dirs_to_scan.push(entry_path.clone());
                } else {
                    files_to_delete.push(DeleteItem {
                        path: entry_path,
                        name: entry_name,
                    });
                }
            }

            dirs_to_delete.push(current_dir);
        }

        let total_files = files_to_delete.len();
        let total_dirs = dirs_to_delete.len();

        info!(
            "Found {} files and {} directories to delete in {}",
            total_files, total_dirs, file_name
        );

        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "local".to_string(),
                message: Some(format!(
                    "Found {} files in {} folders to delete",
                    total_files, total_dirs
                )),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );

        // Phase 2: Delete all files with events (cancellable)
        let mut deleted_files = 0u64;
        let mut errors = 0u64;
        let mut cancelled = false;
        let mut last_emit = std::time::Instant::now();

        for item in &files_to_delete {
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                info!(
                    "Local folder deletion cancelled by user after {} files",
                    deleted_files
                );
                break;
            }
            match tokio::fs::remove_file(&item.path).await {
                Ok(_) => {
                    deleted_files += 1;

                    // Emit progress every 100ms or every 50 files to avoid flooding
                    if last_emit.elapsed().as_millis() > 100
                        || deleted_files % 50 == 0
                        || deleted_files == total_files as u64
                    {
                        let _ = app.emit(
                            "transfer_event",
                            TransferEvent {
                                event_type: "delete_file_complete".to_string(),
                                transfer_id: delete_id.clone(),
                                filename: item.name.clone(),
                                direction: "local".to_string(),
                                message: Some(format!(
                                    "Deleted [{}/{}]: {}",
                                    deleted_files, total_files, item.name
                                )),
                                progress: None,
                                path: Some(item.path.display().to_string()),
                                delta_stats: None,
                                fallback_reason: None,
                            },
                        );
                        last_emit = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    errors += 1;
                    let _ = app.emit(
                        "transfer_event",
                        TransferEvent {
                            event_type: "delete_file_error".to_string(),
                            transfer_id: delete_id.clone(),
                            filename: item.name.clone(),
                            direction: "local".to_string(),
                            message: Some(format!("Failed: {} - {}", item.name, e)),
                            progress: None,
                            path: Some(item.path.display().to_string()),
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                }
            }
        }

        // Phase 3: Delete directories (deepest first - reverse the order!)
        // Directories were added in scan order (parent first), so we need to reverse
        // to delete children before parents
        let dirs_reversed: Vec<_> = dirs_to_delete.iter().rev().collect();
        for dir_path in dirs_reversed {
            if state.cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let dir_name = dir_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "folder".to_string());

            match tokio::fs::remove_dir(dir_path).await {
                Ok(_) => {
                    let _ = app.emit(
                        "transfer_event",
                        TransferEvent {
                            event_type: "delete_dir_complete".to_string(),
                            transfer_id: delete_id.clone(),
                            filename: dir_name,
                            direction: "local".to_string(),
                            message: Some(format!("Removed folder: {}", dir_path.display())),
                            progress: None,
                            path: Some(dir_path.display().to_string()),
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                }
                Err(e) => {
                    warn!("Failed to remove directory {:?}: {}", dir_path, e);
                }
            }
        }

        // Emit completion
        let result_message = if cancelled {
            format!(
                "Deletion cancelled: {} of {} files deleted",
                deleted_files, total_files
            )
        } else if errors > 0 {
            format!(
                "Deleted {} files ({} errors), {} folders",
                deleted_files, errors, total_dirs
            )
        } else {
            format!("Deleted {} files, {} folders", deleted_files, total_dirs)
        };

        let _ = app.emit(
            "transfer_event",
            TransferEvent {
                event_type: if cancelled {
                    "delete_cancelled"
                } else {
                    "delete_complete"
                }
                .to_string(),
                transfer_id: delete_id.clone(),
                filename: file_name.clone(),
                direction: "local".to_string(),
                message: Some(result_message.clone()),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );

        Ok(result_message)
    }
}

#[tauri::command]
async fn rename_remote_file(
    state: State<'_, AppState>,
    from: String,
    to: String,
) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;

    ftp_manager
        .rename(&from, &to)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn create_remote_folder(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;

    ftp_manager
        .mkdir(&path)
        .await
        .map_err(|e| format!("Failed to create folder: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn chmod_remote_file(
    state: State<'_, AppState>,
    path: String,
    mode: String,
) -> Result<(), String> {
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager
        .chmod(&path, &mode)
        .await
        .map_err(|e| e.to_string())
}

/// Sentinel prefix returned when a local rename/copy would clobber an existing
/// destination and the caller passed `overwrite: Some(false)`. The frontend
/// matches this prefix to raise its overwrite confirmation instead of silently
/// destroying the target. (CLAUDE-AV-B1-10)
pub(crate) const DEST_EXISTS_MARKER: &str = "DEST_EXISTS";

#[tauri::command]
async fn rename_local_file(
    from: String,
    to: String,
    overwrite: Option<bool>,
) -> Result<(), String> {
    validate_path(&from)?;
    validate_path(&to)?;
    // Check for Windows reserved filenames
    #[cfg(windows)]
    {
        let dest_name = std::path::Path::new(&to)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(reserved) = windows_acl::check_windows_reserved(&dest_name) {
            return Err(format!(
                "'{}' is a reserved Windows filename and cannot be used",
                reserved
            ));
        }
    }

    // `tokio::fs::rename` overwrites an existing destination atomically and
    // irreversibly (no trash). When the caller explicitly opts out of clobbering
    // (cross-panel move, inline/batch rename), refuse if `to` already exists so
    // the frontend can prompt. Existing callers omit the flag -> None -> preserve
    // the historical overwrite behavior. (CLAUDE-AV-B1-10)
    if overwrite == Some(false) && std::path::Path::new(&to).exists() {
        return Err(format!("{DEST_EXISTS_MARKER}: {to}"));
    }

    tokio::fs::rename(&from, &to)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn copy_local_file(from: String, to: String, overwrite: Option<bool>) -> Result<(), String> {
    validate_path(&from)?;
    validate_path(&to)?;
    let from_path = std::path::Path::new(&from);
    if !from_path.exists() {
        return Err(format!("Source does not exist: {}", from));
    }
    // See rename_local_file: refuse a silent overwrite when the caller opts out.
    // (CLAUDE-AV-B1-10)
    if overwrite == Some(false) && std::path::Path::new(&to).exists() {
        return Err(format!("{DEST_EXISTS_MARKER}: {to}"));
    }
    if from_path.is_dir() {
        // Recursive directory copy
        copy_dir_recursive(from_path, std::path::Path::new(&to), 0).await?;
    } else {
        tokio::fs::copy(&from, &to)
            .await
            .map_err(|e| format!("Failed to copy file: {}", e))?;
    }
    Ok(())
}

async fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
    depth: u32,
) -> Result<(), String> {
    if depth > 50 {
        return Err("Directory nesting too deep (max 50 levels)".to_string());
    }
    tokio::fs::create_dir_all(dst)
        .await
        .map_err(|e| format!("Failed to create directory: {}", e))?;
    let mut entries = tokio::fs::read_dir(src)
        .await
        .map_err(|e| format!("Failed to read directory: {}", e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| format!("Failed to read entry: {}", e))?
    {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        // Use symlink_metadata to avoid following symlinks
        let metadata = tokio::fs::symlink_metadata(&src_path)
            .await
            .map_err(|e| format!("Failed to read metadata: {}", e))?;
        if metadata.is_symlink() {
            // Skip symlinks for security
            continue;
        }
        if metadata.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path, depth + 1)).await?;
        } else {
            tokio::fs::copy(&src_path, &dst_path)
                .await
                .map_err(|e| format!("Failed to copy file: {}", e))?;
        }
    }
    Ok(())
}

#[tauri::command]
async fn create_local_folder(path: String) -> Result<(), String> {
    validate_path(&path)?;
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(|e| format!("Failed to create folder: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn read_file_base64(path: String, max_size_mb: Option<u32>) -> Result<String, String> {
    validate_path(&path)?;
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    // Size cap to prevent OOM on large files (default 50MB)
    let max_size: u64 = (max_size_mb.unwrap_or(50) as u64) * 1024 * 1024;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| "Failed to read file metadata".to_string())?;
    if metadata.len() > max_size {
        return Err(format!(
            "File too large for preview ({:.1} MB). Max: {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            max_size / (1024 * 1024)
        ));
    }

    let data = tokio::fs::read(&path)
        .await
        .map_err(|_| "Failed to read file".to_string())?;

    Ok(STANDARD.encode(data))
}

/// Calculate checksum (MD5, SHA-1, SHA-256, or SHA-512) for a local file
#[tauri::command]
async fn calculate_checksum(path: String, algorithm: String) -> Result<String, String> {
    validate_path(&path)?;
    use md5::Md5;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("Failed to open file: {}", e))?;

    match algorithm.to_lowercase().as_str() {
        "md5" => {
            let mut hasher = Md5::new();
            let mut buffer = vec![0u8; 64 * 1024]; // 64KB buffer

            loop {
                let bytes_read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        "sha256" => {
            let mut hasher = Sha256::new();
            let mut buffer = vec![0u8; 64 * 1024]; // 64KB buffer

            loop {
                let bytes_read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        "sha1" => {
            use sha1::Digest;
            let mut hasher = sha1::Sha1::new();
            let mut buffer = vec![0u8; 64 * 1024];

            loop {
                let bytes_read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        "sha512" => {
            use sha2::{Digest, Sha512};
            let mut hasher = Sha512::new();
            let mut buffer = vec![0u8; 64 * 1024];

            loop {
                let bytes_read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
            }

            let result = hasher.finalize();
            Ok(hex::encode(result))
        }
        "blake3" | "b3" => {
            let mut hasher = blake3::Hasher::new();
            let mut buffer = vec![0u8; 64 * 1024];

            loop {
                let bytes_read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Failed to read file: {}", e))?;
                if bytes_read == 0 {
                    break;
                }
                hasher.update(&buffer[..bytes_read]);
            }

            Ok(hasher.finalize().to_hex().to_string())
        }
        _ => Err(format!(
            "Unsupported algorithm: {}. Use 'md5', 'sha1', 'sha256', 'sha512', or 'blake3'",
            algorithm
        )),
    }
}

/// RAII guard for atomic archive writes. The archive is written to a sibling
/// `<output>.aerotmp` and renamed into place only on success; on any early
/// error (a `?` return) `Drop` removes the temp file, so the compress path can
/// never leave a partial or 0-byte sibling next to the source (discussion
/// #270). Mirrors the atomic pattern already used by extraction.
struct ArchiveTempFile {
    tmp: std::path::PathBuf,
    committed: bool,
}

impl ArchiveTempFile {
    fn new(final_path: &str) -> Self {
        Self {
            tmp: std::path::PathBuf::from(format!("{}.aerotmp", final_path)),
            committed: false,
        }
    }

    fn path(&self) -> &std::path::Path {
        &self.tmp
    }

    /// Close-then-rename. The caller must have dropped the file handle first
    /// (Windows refuses to rename an open file).
    fn commit(mut self, final_path: &str) -> Result<(), String> {
        std::fs::rename(&self.tmp, final_path)
            .map_err(|e| format!("Failed to finalize archive: {}", e))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for ArchiveTempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// Map a user-facing DEFLATE level to the backend level actually encoded.
///
/// flate2 resolves to the zlib-rs backend in this build (pulled in by the zip
/// crate's `deflate` feature), and zlib-rs maps level 1 to its `deflate_quick`
/// algorithm: a speed-first preset whose output is ~40% larger than `gzip -1`
/// on text, while backend level 2 is already both smaller and faster than
/// `gzip -1` (#406). Pin user level 1 to backend level 2 so every user level
/// stays in the native tools' size band; other levels pass through unchanged.
fn deflate_effective_level(level: i64) -> i64 {
    if level == 1 {
        2
    } else {
        level
    }
}

/// Decide whether a ZIP entry should be stored rather than deflated, so that
/// incompressible data never gets larger from compression (#276, store-if-larger).
/// Returns true when a deflate pass at `level` would not shrink `data`.
fn zip_entry_should_store(data: &[u8], level: i64) -> bool {
    use std::io::Write;
    if level <= 0 || data.is_empty() {
        return level <= 0; // level 0 = store everything; empty data is a no-op either way
    }
    let lvl = level.clamp(1, 9) as u32;
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(lvl));
    if enc.write_all(data).is_err() {
        return false;
    }
    match enc.finish() {
        // Store only when deflate fails to make the payload smaller.
        Ok(compressed) => compressed.len() >= data.len(),
        Err(_) => false,
    }
}

/// Files at or above this size skip the full-buffer store-if-larger trial: the trial
/// deflates the whole buffer once and `write_all` deflates it again, so for large
/// payloads that doubles the CPU and stalls the progress bar at 0% during the
/// (unobservable) trial pass. Above the cap we decide store-vs-deflate from a head
/// sample and stream the file, giving a real byte-by-byte bar with a single pass.
const ZIP_STORE_TRIAL_CAP: u64 = 8 * 1024 * 1024;
/// Head sample used to decide store-vs-deflate for files above `ZIP_STORE_TRIAL_CAP`.
const ZIP_STORE_SAMPLE_BYTES: usize = 256 * 1024;

/// Build the base ZIP entry options (compression method + level), without encryption.
fn zip_base_options(level: i64, store: bool) -> zip::write::SimpleFileOptions {
    use zip::write::SimpleFileOptions;
    if store {
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
    } else {
        SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(level))
    }
}

/// Sample-based store decision for a large file: deflate only the head and keep the
/// payload Stored if even that does not shrink (incompressible). Far cheaper than a
/// full-buffer trial on a multi-gigabyte file.
fn zip_large_should_store(path: &std::path::Path, level: i64) -> Result<bool, String> {
    use std::io::Read;
    if level <= 0 {
        return Ok(true);
    }
    let mut f = std::fs::File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let mut sample = vec![0u8; ZIP_STORE_SAMPLE_BYTES];
    let n = f
        .read(&mut sample)
        .map_err(|e| format!("Failed to read file: {}", e))?;
    sample.truncate(n);
    Ok(zip_entry_should_store(&sample, level))
}

/// Add one regular file as a ZIP entry, emitting byte-true progress.
///
/// Small files (< `ZIP_STORE_TRIAL_CAP`) keep the exact store-if-larger trial (#276)
/// and report their size on completion; large files stream through a `ProgressReader`
/// so a single big file fills the bar 0->100 within itself.
fn add_zip_file_entry(
    zip: &mut zip::ZipWriter<std::fs::File>,
    entry_name: String,
    file_path: &std::path::Path,
    level: i64,
    secret_password: &Option<SecretString>,
    progress: &mut crate::archive_progress::ArchiveProgress,
) -> Result<(), String> {
    use std::io::{Read, Write};

    let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);

    if file_size < ZIP_STORE_TRIAL_CAP {
        let mut f =
            std::fs::File::open(file_path).map_err(|e| format!("Failed to open file: {}", e))?;
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        let base = zip_base_options(level, zip_entry_should_store(&buffer, level));
        if let Some(pwd) = secret_password {
            zip.start_file(
                entry_name,
                base.with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret()),
            )
            .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
        } else {
            zip.start_file(entry_name, base)
                .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
        }
        zip.write_all(&buffer)
            .map_err(|e| format!("Failed to write to ZIP: {}", e))?;
        progress.add(buffer.len() as u64);
    } else {
        let store = level == 0 || zip_large_should_store(file_path, level)?;
        let base = zip_base_options(level, store);
        if let Some(pwd) = secret_password {
            zip.start_file(
                entry_name,
                base.with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret()),
            )
            .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
        } else {
            zip.start_file(entry_name, base)
                .map_err(|e| format!("Failed to add file to ZIP: {}", e))?;
        }
        let f =
            std::fs::File::open(file_path).map_err(|e| format!("Failed to open file: {}", e))?;
        let mut reader = crate::archive_progress::ProgressReader::new(f, progress);
        std::io::copy(&mut reader, zip).map_err(|e| format!("Failed to write to ZIP: {}", e))?;
    }
    Ok(())
}

/// Sum the bytes of every regular file that a compress over `paths` will read,
/// mirroring the write-side traversal (recurse directories, skip symlinks). Used as
/// the progress denominator so the bar closes exactly at the real input total.
///
/// This is a scan-time snapshot: if a file is modified between this sum and the write
/// pass, the per-chunk counter is clamped to the total (`ArchiveProgress::add`) and the
/// final 100% frame is forced (`finish`), so the bar never overshoots and always closes
/// at 100% - the only visible effect of concurrent modification is a mid-run jump.
fn sum_compress_input_bytes(paths: &[String]) -> u64 {
    use walkdir::WalkDir;
    let mut total = 0u64;
    for path in paths {
        let p = std::path::Path::new(path);
        if p.is_file() {
            total = total.saturating_add(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
        } else if p.is_dir() {
            for entry in WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
                if let Ok(md) = std::fs::symlink_metadata(entry.path()) {
                    if md.file_type().is_symlink() {
                        continue;
                    }
                    if md.is_file() {
                        total = total.saturating_add(md.len());
                    }
                }
            }
        }
    }
    total
}

/// Real (canary) compression-size estimate.
///
/// Rather than a fixed per-format ratio table, this compresses a bounded
/// SAMPLE of the actual input with the real codec at the chosen level and
/// extrapolates the measured ratio to the full input. When the whole input
/// fits under the sample cap it is compressed in full, so the result is exact
/// (`exact = true`). The sample is capped so the call stays near-instant even
/// for very large inputs.
#[derive(serde::Serialize)]
struct CompressEstimate {
    /// Total uncompressed size of the input (recursive, symlinks skipped).
    input_bytes: u64,
    /// Estimated size of the compressed output.
    estimated_bytes: u64,
    /// estimated_bytes / input_bytes * 100.
    ratio_pct: f64,
    /// How many bytes were actually read+compressed to derive the ratio.
    sampled_bytes: u64,
    /// True when the entire input was compressed (no extrapolation).
    exact: bool,
}

/// Maximum bytes read+compressed for the canary sample (keeps the call fast).
const CANARY_SAMPLE_CAP: u64 = 4 * 1024 * 1024;

/// Collect every regular file (recursive) under the given paths, with sizes.
/// Zero-byte files and symlinks are skipped.
fn collect_input_files(paths: &[String]) -> Vec<(std::path::PathBuf, u64)> {
    use walkdir::WalkDir;
    let mut out = Vec::new();
    for path in paths {
        let p = std::path::Path::new(path);
        if p.is_file() {
            if let Ok(md) = std::fs::metadata(p) {
                if md.len() > 0 {
                    out.push((p.to_path_buf(), md.len()));
                }
            }
        } else if p.is_dir() {
            for entry in WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
                if let Ok(md) = std::fs::symlink_metadata(entry.path()) {
                    if md.file_type().is_symlink() || !md.is_file() || md.len() == 0 {
                        continue;
                    }
                    out.push((entry.path().to_path_buf(), md.len()));
                }
            }
        }
    }
    out
}

/// Append up to `len` bytes from `path` starting at `offset` into `buf`.
fn read_region_append(path: &std::path::Path, offset: u64, len: usize, buf: &mut Vec<u8>) {
    use std::io::{Read, Seek, SeekFrom};
    if len == 0 {
        return;
    }
    if let Ok(mut f) = std::fs::File::open(path) {
        if f.seek(SeekFrom::Start(offset)).is_ok() {
            let _ = f.take(len as u64).read_to_end(buf);
        }
    }
}

/// Build the canary sample. If the whole input fits the cap, read it all
/// (exact); otherwise read three spread regions of the largest file plus the
/// heads of the remaining files, bounded by the cap, for representativeness.
fn build_canary_sample(files: &[(std::path::PathBuf, u64)], total: u64, cap: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    if total <= cap {
        for (p, _) in files {
            read_region_append(p, 0, usize::MAX, &mut buf);
        }
        return buf;
    }
    let largest = files
        .iter()
        .enumerate()
        .max_by_key(|(_, (_, s))| *s)
        .map(|(i, _)| i);
    let mut remaining = cap as usize;
    if let Some(i) = largest {
        let (p, size) = &files[i];
        let want = (cap / 2).min(*size) as usize;
        let region = (want / 3).max(1);
        let positions = [
            0u64,
            size.saturating_sub(region as u64) / 2,
            size.saturating_sub(region as u64),
        ];
        for off in positions {
            if remaining == 0 {
                break;
            }
            let take = region.min(remaining);
            read_region_append(p, off, take, &mut buf);
            remaining = remaining.saturating_sub(take);
        }
    }
    for (idx, (p, size)) in files.iter().enumerate() {
        if Some(idx) == largest || remaining == 0 {
            continue;
        }
        let take = (*size as usize).min(remaining).min(256 * 1024);
        read_region_append(p, 0, take, &mut buf);
        remaining = remaining.saturating_sub(take);
    }
    buf
}

/// Compress `buf` in memory with the codec the frontend maps each format to.
fn compress_sample(buf: &[u8], codec: &str, level: i32) -> Result<usize, String> {
    use std::io::Write;
    match codec {
        "store" => Ok(buf.len()),
        // zip and tar.gz both deflate; the few-byte gzip header is immaterial here.
        "deflate" | "gzip" => {
            let lvl = deflate_effective_level(level.clamp(0, 9) as i64) as u32;
            let mut e =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(lvl));
            e.write_all(buf).map_err(|e| e.to_string())?;
            Ok(e.finish().map_err(|e| e.to_string())?.len())
        }
        // 7z (LZMA2) is estimated with xz, which is also LZMA2-based.
        "xz" => {
            let lvl = level.clamp(0, 9) as u32;
            let mut e = xz2::write::XzEncoder::new(Vec::new(), lvl);
            e.write_all(buf).map_err(|e| e.to_string())?;
            Ok(e.finish().map_err(|e| e.to_string())?.len())
        }
        "bzip2" => {
            let lvl = level.clamp(1, 9) as u32;
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(lvl));
            e.write_all(buf).map_err(|e| e.to_string())?;
            Ok(e.finish().map_err(|e| e.to_string())?.len())
        }
        "zstd" => {
            let out = zstd::stream::encode_all(buf, level).map_err(|e| e.to_string())?;
            Ok(out.len())
        }
        other => Err(format!("Unknown codec: {other}")),
    }
}

/// Estimate the compressed size of `paths` for a given codec/level via a
/// bounded canary sample. Runs on a blocking thread (I/O + CPU bound).
#[tauri::command]
async fn estimate_compressed_size(
    paths: Vec<String>,
    codec: String,
    level: i64,
) -> Result<CompressEstimate, String> {
    for p in &paths {
        validate_path(p)?;
    }
    tokio::task::spawn_blocking(move || {
        let files = collect_input_files(&paths);
        let total: u64 = files.iter().map(|(_, s)| *s).sum();
        if total == 0 {
            return Ok(CompressEstimate {
                input_bytes: 0,
                estimated_bytes: 0,
                ratio_pct: 0.0,
                sampled_bytes: 0,
                exact: true,
            });
        }
        let sample = build_canary_sample(&files, total, CANARY_SAMPLE_CAP);
        if sample.is_empty() {
            return Ok(CompressEstimate {
                input_bytes: total,
                estimated_bytes: total,
                ratio_pct: 100.0,
                sampled_bytes: 0,
                exact: false,
            });
        }
        let out = compress_sample(&sample, &codec, level as i32)?;
        let ratio = out as f64 / sample.len() as f64;
        let estimated = (total as f64 * ratio).round() as u64;
        Ok(CompressEstimate {
            input_bytes: total,
            estimated_bytes: estimated,
            ratio_pct: ratio * 100.0,
            sampled_bytes: sample.len() as u64,
            // F-08: only "exact" if the whole input was actually sampled; a
            // silently-skipped unreadable file leaves sample.len() < total.
            exact: total <= CANARY_SAMPLE_CAP && sample.len() as u64 == total,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Compress files/folders into a ZIP archive
#[tauri::command]
async fn compress_files(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,
    compression_level: Option<i64>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    compress_files_impl(paths, output_path, password, compression_level, Some(app)).await
}

/// Implementation shared by the GUI command (passes `Some(app)` for live progress)
/// and the headless `compress_files_core` wrapper used by the CLI / AI tools (`None`).
async fn compress_files_impl(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,
    compression_level: Option<i64>,
    app: Option<tauri::AppHandle>,
) -> Result<String, String> {
    validate_path(&output_path)?;
    for p in &paths {
        validate_path(p)?;
    }

    use std::fs::File;
    use walkdir::WalkDir;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    // Byte-true progress denominator: the real input total, mirroring the write-side
    // traversal so the bar closes exactly at the total (HANDOFF section 3.3.A).
    let total_bytes = sum_compress_input_bytes(&paths);
    let mut progress = crate::archive_progress::ArchiveProgress::for_optional_app(
        app,
        crate::archive_progress::phase::COMPRESSING,
        total_bytes,
    );

    // Atomic write: build into <output>.aerotmp, rename on success.
    let temp = ArchiveTempFile::new(&output_path);
    let file =
        File::create(temp.path()).map_err(|e| format!("Failed to create ZIP file: {}", e))?;

    let mut zip = ZipWriter::new(file);
    let level = deflate_effective_level(compression_level.unwrap_or(5));
    // Level 0 means "store" (no deflate). The zip crate rejects a numeric
    // compression_level on the Stored method, so only attach the level when
    // we are actually deflating.
    let base_options = if level == 0 {
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)
    } else {
        SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(level))
    };

    for path in &paths {
        let path = std::path::Path::new(path);

        if path.is_file() {
            let file_name = path
                .file_name()
                .ok_or("Invalid file name")?
                .to_string_lossy()
                .to_string();
            add_zip_file_entry(
                &mut zip,
                file_name,
                path,
                level,
                &secret_password,
                &mut progress,
            )?;
        } else if path.is_dir() {
            let _base_name = path
                .file_name()
                .ok_or("Invalid directory name")?
                .to_string_lossy();

            for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                let entry_path = entry.path();
                let relative_path = entry_path
                    .strip_prefix(path.parent().unwrap_or(path))
                    .map_err(|e| format!("Path error: {}", e))?;

                // Use symlink_metadata to avoid following symlinks (A7-06)
                let metadata = std::fs::symlink_metadata(entry_path)
                    .map_err(|e| format!("Cannot read metadata: {}", e))?;
                if metadata.file_type().is_symlink() {
                    continue; // Skip symlinks
                }

                if metadata.is_file() {
                    add_zip_file_entry(
                        &mut zip,
                        relative_path.to_string_lossy().to_string(),
                        entry_path,
                        level,
                        &secret_password,
                        &mut progress,
                    )?;
                } else if metadata.is_dir() && entry_path != path {
                    let dir_path = format!("{}/", relative_path.to_string_lossy());
                    if let Some(ref pwd) = secret_password {
                        zip.add_directory(
                            &dir_path,
                            base_options
                                .with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret()),
                        )
                        .map_err(|e| format!("Failed to add directory to ZIP: {}", e))?;
                    } else {
                        zip.add_directory(&dir_path, base_options)
                            .map_err(|e| format!("Failed to add directory to ZIP: {}", e))?;
                    }
                }
            }
        }
    }

    // finish() returns the inner File; dropping it closes the handle so the
    // rename in commit() succeeds on Windows too.
    drop(
        zip.finish()
            .map_err(|e| format!("Failed to finalize ZIP: {}", e))?,
    );
    temp.commit(&output_path)?;
    progress.finish();

    Ok(output_path)
}

/// Extract a ZIP archive
#[tauri::command]
async fn extract_archive(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
    password: Option<String>,
) -> Result<String, String> {
    validate_path(&archive_path)?;
    validate_path(&output_dir)?;

    use std::fs::{self, File};
    use zip::ZipArchive;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let file = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;

    let mut archive =
        ZipArchive::new(file).map_err(|e| format!("Failed to read archive: {}", e))?;

    // Determine actual output directory
    let actual_output = if create_subfolder {
        let archive_stem = std::path::Path::new(&archive_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let subfolder = std::path::Path::new(&output_dir).join(&archive_stem);
        subfolder.to_string_lossy().to_string()
    } else {
        output_dir.clone()
    };

    // Create output directory if needed
    fs::create_dir_all(&actual_output)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    for i in 0..archive.len() {
        let mut file = if let Some(ref pwd) = secret_password {
            archive
                .by_index_decrypt(i, pwd.expose_secret().as_bytes())
                .map_err(|e| format!("Failed to decrypt file from archive: {}", e))?
        } else {
            archive
                .by_index(i)
                .map_err(|e| format!("Failed to read file from archive: {}", e))?
        };

        // ZIP Slip protection: use the single shared guard (rejects traversal,
        // absolute paths, drive prefixes, empty names and null bytes) instead of a
        // divergent inline copy. (CLAUDE-AV-B1-03)
        let entry_name = file.name().to_string();
        if !is_safe_archive_entry(&entry_name) {
            continue;
        }
        let outpath = std::path::Path::new(&actual_output).join(&entry_name);

        if entry_name.ends_with('/') {
            // Directory
            fs::create_dir_all(&outpath)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        } else {
            // File
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }

            let mut outfile =
                File::create(&outpath).map_err(|e| format!("Failed to create file: {}", e))?;

            let declared = file.size();
            copy_entry_bounded(&mut file, &mut outfile, declared)
                .map_err(|e| format!("Failed to extract file: {}", e))?;
        }
    }

    Ok(actual_output)
}

/// Advanced 7z encoder knobs, all optional (unset keeps the current LZMA2
/// defaults). Surfaced by the CompressDialog "Advanced" section and the CLI
/// `compress` flags. `dictionary_size` and `threads` apply to LZMA2 only; the
/// other methods are driven by the level alone.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SevenZAdvanced {
    /// Content method: "lzma2" (default) | "lzma" | "ppmd" | "bzip2".
    pub method: Option<String>,
    /// LZMA2 dictionary size in bytes (the encoder clamps it to 4096..=4 GiB).
    pub dictionary_size: Option<u64>,
    /// Pack every file into one solid block: better ratio on many small files,
    /// slower random extraction. Off by default.
    pub solid: Option<bool>,
    /// LZMA2 multi-thread compression thread count (1 = single-threaded).
    pub threads: Option<u32>,
}

/// Compress files/folders into a 7z archive (LZMA2 by default; the method and
/// other codec knobs come from the optional `advanced` options).
#[tauri::command]
async fn compress_7z(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,
    compression_level: Option<i64>,
    encrypt_header: Option<bool>,
    advanced: Option<SevenZAdvanced>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    compress_7z_impl(
        paths,
        output_path,
        password,
        compression_level,
        encrypt_header,
        advanced,
        Some(app),
    )
    .await
}

async fn compress_7z_impl(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,
    compression_level: Option<i64>,
    encrypt_header: Option<bool>,
    advanced: Option<SevenZAdvanced>,
    app: Option<tauri::AppHandle>,
) -> Result<String, String> {
    use sevenz_rust2::*;
    use std::fs::File;
    use std::path::Path;
    use walkdir::WalkDir;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    // Collect all files to compress
    let mut entries: Vec<(String, String)> = Vec::new(); // (archive_name, full_path)

    for path_str in &paths {
        let path = Path::new(path_str);

        if path.is_file() {
            let file_name = path
                .file_name()
                .ok_or("Invalid file name")?
                .to_string_lossy()
                .to_string();
            entries.push((file_name, path_str.clone()));
        } else if path.is_dir() {
            // Add directory contents recursively
            for entry in WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let entry_path = entry.path();
                if entry_path.is_file() {
                    let relative_path = entry_path
                        .strip_prefix(path.parent().unwrap_or(path))
                        .map_err(|e| format!("Path error: {}", e))?;
                    entries.push((
                        relative_path.to_string_lossy().to_string(),
                        entry_path.to_string_lossy().to_string(),
                    ));
                }
            }
        }
    }

    if entries.is_empty() {
        return Err("No files to compress".to_string());
    }

    // Byte-true progress denominator: sum of the entry sizes actually read.
    let total_bytes: u64 = entries
        .iter()
        .map(|(_, p)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();
    let mut progress = crate::archive_progress::ArchiveProgress::for_optional_app(
        app,
        crate::archive_progress::phase::COMPRESSING,
        total_bytes,
    );

    // Create the 7z archive (atomic temp; renamed into place on success).
    let temp = ArchiveTempFile::new(&output_path);
    let output_file =
        File::create(temp.path()).map_err(|e| format!("Failed to create 7z file: {}", e))?;

    let mut sz = ArchiveWriter::new(output_file)
        .map_err(|e| format!("Failed to create 7z writer: {}", e))?;

    // Map the caller's preset (0-9, unset = 5 = 7-Zip "Normal") onto the chosen
    // content method. The dialog's preset buttons send the canonical 7-Zip
    // levels (Fastest=1, Fast=3, Normal=5, Maximum=7, Ultra=9) and the CLI
    // passes 0-9 directly; every from_level clamps to its valid range.
    let level = compression_level.unwrap_or(5).clamp(0, 9) as u32;
    let adv = advanced.unwrap_or_default();

    // Build the content method. LZMA2 (default) additionally honors the
    // dictionary-size and thread knobs; LZMA, PPMd and BZip2 are driven by the
    // level alone (the crate maps it to their internal parameters). All four
    // are decodable by our extract path (verified against sevenz-rust2 0.21.1
    // add_decoder: ID_LZMA, ID_LZMA2, ID_PPMD, ID_BZIP2), so we never create an
    // archive we cannot reopen. Word size is not exposed by the crate's public
    // API in 0.21.1, so it is intentionally not offered.
    let content: EncoderConfiguration = match adv.method.as_deref() {
        None | Some("lzma2") => {
            let mut o = match adv.threads.filter(|t| *t > 1) {
                Some(t) => encoder_options::Lzma2Options::from_level_mt(level, t, 0),
                None => encoder_options::Lzma2Options::from_level(level),
            };
            if let Some(d) = adv.dictionary_size {
                o.set_dictionary_size(d.min(u32::MAX as u64) as u32);
            }
            o.into()
        }
        Some("lzma") => EncoderConfiguration::new(EncoderMethod::LZMA).with_options(
            encoder_options::EncoderOptions::Lzma(encoder_options::LzmaOptions::from_level(level)),
        ),
        Some("ppmd") => encoder_options::PpmdOptions::from_level(level).into(),
        Some("bzip2") => encoder_options::Bzip2Options::from_level(level).into(),
        Some(other) => {
            return Err(format!(
                "unknown 7z method '{}': use lzma2, lzma, ppmd or bzip2",
                other
            ));
        }
    };

    // Set compression and optional AES-256 encryption.
    if let Some(ref pwd) = secret_password {
        let aes_options =
            encoder_options::AesEncoderOptions::new(Password::from(pwd.expose_secret()));
        sz.set_content_methods(vec![aes_options.into(), content]);
        // -mhe: header (filename) encryption is opt-in, exactly like 7-Zip's
        // "Encrypt file names" checkbox that sits under the password. Off keeps
        // the classic behaviour (content encrypted, names readable); on hides
        // the names too, at the cost of needing the password even to list the
        // archive. sevenz-rust2 defaults this to true once a password is set, so
        // we set it explicitly from the caller's choice in both directions.
        sz.set_encrypt_header(encrypt_header.unwrap_or(false));
    } else {
        sz.set_content_methods(vec![content]);
        // No password: nothing to encrypt, so leave the header in the clear (an
        // encrypted header without a key is meaningless and would only break
        // plain listing).
        sz.set_encrypt_header(false);
    }

    // Add files to the archive. Solid packs every file into a single stream
    // (better ratio, slower random extract); non-solid keeps one pack per file.
    if adv.solid.unwrap_or(false) {
        // push_archive_entries takes every entry reader at once and reads them
        // sequentially on this thread, so the readers can share one progress
        // counter through Rc<RefCell> (only one borrow is ever live) and the bar
        // stays byte-true without a background thread.
        let progress_rc = std::rc::Rc::new(std::cell::RefCell::new(progress));
        let mut zentries = Vec::with_capacity(entries.len());
        let mut readers = Vec::with_capacity(entries.len());
        for (archive_name, full_path) in &entries {
            let source_path = Path::new(full_path);
            let file = File::open(source_path)
                .map_err(|e| format!("Failed to open file '{}': {}", archive_name, e))?;
            zentries.push(ArchiveEntry::from_path(source_path, archive_name.clone()));
            readers.push(SourceReader::new(
                crate::archive_progress::RcProgressReader::new(file, progress_rc.clone()),
            ));
        }
        sz.push_archive_entries(zentries, readers)
            .map_err(|e| format!("Failed to add files (solid): {}", e))?;
        // Every reader was consumed by push_archive_entries, so this Rc is the
        // last reference: recover the progress to finish it below.
        progress = std::rc::Rc::try_unwrap(progress_rc)
            .map_err(|_| "internal: solid progress still referenced".to_string())?
            .into_inner();
    } else {
        for (archive_name, full_path) in &entries {
            let source_path = Path::new(full_path);
            let entry = ArchiveEntry::from_path(source_path, archive_name.clone());

            // Open the source and count bytes as the 7z writer pulls them through.
            let file = File::open(source_path)
                .map_err(|e| format!("Failed to open file '{}': {}", archive_name, e))?;
            let reader = crate::archive_progress::ProgressReader::new(file, &mut progress);

            sz.push_archive_entry(entry, Some(reader))
                .map_err(|e| format!("Failed to add file '{}': {}", archive_name, e))?;
        }
    }

    // finish() consumes the writer and drops the inner file handle, so the
    // rename in commit() succeeds (Windows included).
    sz.finish()
        .map_err(|e| format!("Failed to finalize 7z archive: {}", e))?;
    temp.commit(&output_path)?;
    progress.finish();

    Ok(output_path)
}

/// Validate that an archive entry name is safe for extraction.
/// Rejects absolute paths, Windows drive letters, path traversal (`..`),
/// and entries that would escape the destination directory.
pub(crate) fn is_safe_archive_entry(entry_name: &str) -> bool {
    // Reject empty names
    if entry_name.is_empty() {
        return false;
    }
    // Reject absolute paths (Unix and Windows)
    if entry_name.starts_with('/') || entry_name.starts_with('\\') {
        return false;
    }
    // Reject Windows drive letters (e.g. "C:")
    if entry_name.len() >= 2 && entry_name.as_bytes()[1] == b':' {
        return false;
    }
    // Reject path traversal via ".." in any component (handles both / and \ separators)
    if entry_name
        .split('/')
        .chain(entry_name.split('\\'))
        .any(|c| c == "..")
    {
        return false;
    }
    // Reject null bytes
    if entry_name.contains('\0') {
        return false;
    }
    true
}

/// Copy an archive entry into `writer` but never write more than the entry's
/// declared uncompressed size: a stream that expands past what its header claims
/// is a decompression bomb (or a corrupt archive) and is rejected. Mirrors the
/// single-entry browse path (archive_browse.rs, CLAUDE-AV-015) so the
/// whole-archive extractors get the same defense the preview path already had.
/// (CLAUDE-AV-B1-04)
fn copy_entry_bounded<R: std::io::Read + ?Sized, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    declared: u64,
) -> std::io::Result<u64> {
    let mut limited = std::io::Read::take(&mut *reader, declared.saturating_add(1));
    let written = std::io::copy(&mut limited, writer)?;
    if written > declared {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "archive entry expands past its declared size (compression bomb?)",
        ));
    }
    Ok(written)
}

/// Absolute floor for a single-stream (gz/xz/bz2) decompression cap: a raw codec
/// stream carries no reliable declared size, so we cap the plaintext at
/// `max(compressed_len * ratio, this)`. A tiny bomb still cannot exceed this;
/// a genuinely large file scales with its own compressed size. (CLAUDE-AV-B1-05)
const SINGLE_STREAM_ABS_FLOOR: u64 = 1024 * 1024 * 1024; // 1 GiB
const SINGLE_STREAM_MAX_RATIO: u64 = 1000;

/// Extract a 7z archive with optional password (AES-256 decryption)
#[tauri::command]
async fn extract_7z(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<String, String> {
    use sevenz_rust2::*;
    use std::fs::{self, File};
    use std::io::BufReader;
    use std::path::Path;

    // Determine output directory
    let final_output_dir = if create_subfolder {
        let archive_name = Path::new(&archive_path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "extracted".to_string());
        Path::new(&output_dir)
            .join(&archive_name)
            .to_string_lossy()
            .to_string()
    } else {
        output_dir.clone()
    };

    // Create output directory
    fs::create_dir_all(&final_output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let file =
        File::open(&archive_path).map_err(|e| format!("Failed to open 7z archive: {}", e))?;
    let reader = BufReader::new(file);

    let pwd = secret_password
        .as_ref()
        .map(|p| Password::from(p.expose_secret()))
        .unwrap_or_else(Password::empty);

    let mut archive =
        ArchiveReader::new(reader, pwd).map_err(|e| format!("Failed to read 7z archive: {}", e))?;

    let dest = Path::new(&final_output_dir);

    // C5: Extract entries with per-entry path traversal validation
    // instead of using decompress_file() which extracts blindly
    archive
        .for_each_entries(|entry, reader| {
            let name = entry.name();

            // Skip entries with unsafe paths (traversal, absolute, drive letters)
            if !is_safe_archive_entry(name) {
                return Ok(true); // skip this entry, continue to next
            }

            let out_path = dest.join(name);

            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }

            if entry.is_directory() {
                fs::create_dir_all(&out_path)?;
            } else {
                let mut outfile = File::create(&out_path)?;
                let declared = entry.size();
                copy_entry_bounded(reader, &mut outfile, declared)?;
            }

            Ok(true) // continue
        })
        .map_err(|e| format!("Failed to extract 7z archive: {}", e))?;

    Ok(final_output_dir)
}

/// Check if a 7z archive is password protected
#[tauri::command]
async fn is_7z_encrypted(archive_path: String) -> Result<bool, String> {
    use sevenz_rust2::*;
    use std::fs::File;
    use std::io::BufReader;

    let file = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;

    let reader = BufReader::new(file);

    // Try to open without password: with content-only encryption the 7z header
    // lists names in the clear, so this succeeds and we probe the content below.
    // With -mhe (encrypted header) the open itself fails without the password,
    // which the error branch maps to "encrypted".
    let mut archive = match ArchiveReader::new(reader, Password::empty()) {
        Ok(a) => a,
        Err(e) => {
            let err_str = format!("{:?}", e);
            if err_str.contains("password")
                || err_str.contains("Password")
                || err_str.contains("encrypted")
                || err_str.contains("Encrypted")
            {
                return Ok(true);
            }
            return Ok(false);
        }
    };

    // Metadata opened fine, but content may still be encrypted.
    // Try to decompress the first file: if it fails, content is encrypted.
    let has_files = archive.archive().files.iter().any(|f| f.has_stream());
    if !has_files {
        return Ok(false);
    }

    let mut encrypted = false;
    let result = archive.for_each_entries(|_entry, reader| {
        let mut buf = [0u8; 1];
        match reader.read(&mut buf) {
            Ok(_) => {}
            Err(_) => {
                encrypted = true;
            }
        }
        // Stop after first entry
        Ok(false)
    });

    if result.is_err() {
        encrypted = true;
    }

    Ok(encrypted)
}

/// Check if a ZIP archive is password protected (AES or ZipCrypto)
#[tauri::command]
async fn is_zip_encrypted(archive_path: String) -> Result<bool, String> {
    use std::fs::File;
    use zip::ZipArchive;

    let file = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;

    let mut archive =
        ZipArchive::new(file).map_err(|e| format!("Failed to read archive: {}", e))?;

    // Check if any file in the archive is encrypted
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index_raw(i) {
            if entry.encrypted() {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Detect the real encryption cipher of an already-known-encrypted general
/// archive, for the unlock dialog badge. Returns a short canonical token the
/// frontend formats and colors: `AES-256` / `AES-192` / `AES-128` (strong) or
/// `ZipCrypto` (legacy, weak). Best-effort: the caller shows only the format
/// badge when this errors, so a detection miss never blocks the unlock.
#[tauri::command]
async fn detect_archive_cipher(archive_path: String, kind: String) -> Result<String, String> {
    match kind.as_str() {
        // 7z encryption is AES-256 (AES-256-CBC) only, by format.
        "sevenz" => Ok("AES-256".to_string()),
        "rar" => detect_rar_cipher(&archive_path),
        "zip" => detect_zip_cipher(&archive_path),
        other => Err(format!(
            "unsupported archive kind for cipher detection: {other}"
        )),
    }
}

/// Structured archive metadata for the proactive list-view badges (the Type
/// column padlock and the optional Encryption column). Unlike
/// `detect_archive_cipher`, which the unlock dialog calls for an archive already
/// known to be encrypted, this determines *whether* an archive is encrypted,
/// reading only the bytes it needs (the ZIP tail plus central directory, the 7z
/// next-header) so it stays cheap enough to run lazily per visible row.
/// Best-effort: on any parse error the caller shows a neutral state, never a
/// wrong badge.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArchiveMeta {
    /// True when at least one entry (or the header) is encrypted.
    pub(crate) encrypted: bool,
    /// Canonical cipher token when encrypted: `AES-256` / `AES-192` / `AES-128`
    /// / `AES` / `ZipCrypto`. `None` when not encrypted or undetermined.
    pub(crate) cipher: Option<String>,
    /// Representative compression method token for the Compression column, e.g.
    /// `Deflate` / `LZMA2` / `BZip2` / `Zstd` / `PPMd` / `RAR`, or `Store` for an
    /// uncompressed archive. `None` only when the method cannot be read (an
    /// unknown method, or a 7z / RAR with an encrypted header).
    pub(crate) compression: Option<String>,
}

/// Proactively detect whether an archive is encrypted, with which cipher, and
/// its representative compression method for the list-view badges. ZIP, 7z and
/// RAR are supported. Reads only the header regions (ZIP/7z) or lists the RAR
/// directory, never the whole payload.
#[tauri::command]
async fn detect_archive_meta(archive_path: String, kind: String) -> Result<ArchiveMeta, String> {
    match kind.as_str() {
        "sevenz" => detect_7z_meta(&archive_path),
        "zip" => detect_zip_meta(&archive_path),
        "rar" => detect_rar_meta(&archive_path),
        other => Err(format!(
            "unsupported archive kind for meta detection: {other}"
        )),
    }
}

/// ZIP metadata by parsing the central directory: WinZip AES (compression
/// method 99) carries a 0x9901 extra field whose strength byte gives
/// 128/192/256; a plain encrypted entry (general-purpose flag bit 0) is legacy
/// ZipCrypto; no encrypted entry means not encrypted. Reads only the tail (EOCD
/// lives within the last 64 KiB + 22) and the central directory for archives
/// larger than 8 MiB, falling back to a whole-file read for small or ZIP64
/// archives where the seek offsets are cheap or use 32-bit sentinels.
fn detect_zip_meta(path: &str) -> Result<ArchiveMeta, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let flen = f.metadata().map_err(|e| format!("stat: {e}"))?.len();
    if flen < 22 {
        return Err("too small for ZIP".to_string());
    }

    // Small or ZIP64 archives: read whole file (cheap, and it sidesteps the
    // 0xFFFFFFFF central-directory sentinel that ZIP64 stores in the EOCD).
    if flen <= 8 * 1024 * 1024 {
        let data = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
        let eocd = zip_find_eocd(&data).ok_or("no end-of-central-directory record")?;
        if eocd + 20 > data.len() {
            return Err("truncated EOCD".to_string());
        }
        let entries = u16::from_le_bytes([data[eocd + 10], data[eocd + 11]]) as usize;
        let cd_off = u32::from_le_bytes([
            data[eocd + 16],
            data[eocd + 17],
            data[eocd + 18],
            data[eocd + 19],
        ]) as usize;
        let cd = data.get(cd_off..).unwrap_or(&[]);
        return Ok(parse_zip_central_dir(cd, entries));
    }

    // Large archive: read only the tail to find the EOCD, then the central
    // directory it points at.
    let tail_len = flen.min(65_557) as usize;
    let tail_start = flen - tail_len as u64;
    f.seek(SeekFrom::Start(tail_start))
        .map_err(|e| format!("seek tail: {e}"))?;
    let mut tail = vec![0u8; tail_len];
    f.read_exact(&mut tail)
        .map_err(|e| format!("read tail: {e}"))?;
    let eocd = zip_find_eocd(&tail).ok_or("no end-of-central-directory record")?;
    if eocd + 20 > tail.len() {
        return Err("truncated EOCD".to_string());
    }
    let entries = u16::from_le_bytes([tail[eocd + 10], tail[eocd + 11]]) as usize;
    let cd_size = u32::from_le_bytes([
        tail[eocd + 12],
        tail[eocd + 13],
        tail[eocd + 14],
        tail[eocd + 15],
    ]) as usize;
    let cd_off = u32::from_le_bytes([
        tail[eocd + 16],
        tail[eocd + 17],
        tail[eocd + 18],
        tail[eocd + 19],
    ]);
    // ZIP64 sentinel: fall back to the whole-file read.
    if cd_off == 0xFFFF_FFFF || cd_size == 0 {
        let data = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
        let eocd = zip_find_eocd(&data).ok_or("no end-of-central-directory record")?;
        let entries = u16::from_le_bytes([data[eocd + 10], data[eocd + 11]]) as usize;
        let cd_off = u32::from_le_bytes([
            data[eocd + 16],
            data[eocd + 17],
            data[eocd + 18],
            data[eocd + 19],
        ]) as usize;
        let cd = data.get(cd_off..).unwrap_or(&[]);
        return Ok(parse_zip_central_dir(cd, entries));
    }
    // Clamp the attacker-controlled central-directory size to what the file can
    // actually hold before allocating: a crafted EOCD can declare cd_size up to
    // ~4 GiB, and `vec![0u8; cd_size]` would reserve that much RAM (a passive OOM
    // spike triggered merely by listing a directory containing the malicious zip)
    // even though `read_exact` then fails on the short file. (CLAUDE-AV-B1-01)
    if cd_off as u64 >= flen {
        return Err("central directory offset past end of file".to_string());
    }
    let remaining = (flen - cd_off as u64) as usize;
    let cd_size = cd_size.min(remaining);
    f.seek(SeekFrom::Start(cd_off as u64))
        .map_err(|e| format!("seek cd: {e}"))?;
    let mut cd = vec![0u8; cd_size];
    f.read_exact(&mut cd).map_err(|e| format!("read cd: {e}"))?;
    Ok(parse_zip_central_dir(&cd, entries))
}

/// Parse a ZIP central directory (a slice starting at the first central-directory
/// file header) and return the encryption metadata of the first encrypted entry.
pub(crate) fn parse_zip_central_dir(cd: &[u8], entries: usize) -> ArchiveMeta {
    let mut p = 0usize;
    let mut encrypted = false;
    let mut cipher: Option<String> = None;
    // Compression: prefer the first real compressed method; only fall back to
    // "Store" when every entry is stored (so a leading directory entry does not
    // mask a compressed file). An unreadable/unknown method leaves both unset.
    let mut method_label: Option<String> = None;
    let mut saw_store = false;
    for _ in 0..entries {
        if p + 46 > cd.len() || cd[p..p + 4] != [0x50, 0x4B, 0x01, 0x02] {
            break;
        }
        let flags = u16::from_le_bytes([cd[p + 8], cd[p + 9]]);
        let method = u16::from_le_bytes([cd[p + 10], cd[p + 11]]);
        let fnlen = u16::from_le_bytes([cd[p + 28], cd[p + 29]]) as usize;
        let extralen = u16::from_le_bytes([cd[p + 30], cd[p + 31]]) as usize;
        let commentlen = u16::from_le_bytes([cd[p + 32], cd[p + 33]]) as usize;
        let extra_start = p + 46 + fnlen;
        if method_label.is_none() {
            // A WinZip AES entry (method 99) carries its real method in the
            // 0x9901 extra field.
            let eff_method = if method == 99 {
                zip_aes_inner_method(cd, extra_start, extralen).unwrap_or(99)
            } else {
                method
            };
            match zip_method_label(eff_method).as_deref() {
                Some("Store") => saw_store = true,
                Some(m) => method_label = Some(m.to_string()),
                None => {}
            }
        }
        // Encryption: first encrypted entry wins the cipher token.
        if !encrypted && flags & 0x0001 != 0 {
            encrypted = true;
            cipher = Some(if method == 99 {
                match zip_aes_strength(cd, extra_start, extralen) {
                    Some(bits) => format!("AES-{bits}"),
                    None => "AES".to_string(),
                }
            } else {
                "ZipCrypto".to_string()
            });
        }
        p = extra_start + extralen + commentlen;
    }
    ArchiveMeta {
        encrypted,
        cipher,
        compression: method_label.or_else(|| saw_store.then(|| "Store".to_string())),
    }
}

/// Map a ZIP compression method id to a display token: `Store` for an
/// uncompressed (method 0) entry, the named method for a compressed one, or
/// `None` for a method we do not recognize (so we never mislabel it). The WinZip
/// AES wrapper (method 99) is resolved to its inner method by the caller first.
fn zip_method_label(method: u16) -> Option<String> {
    let label = match method {
        0 => "Store",
        8 => "Deflate",
        9 => "Deflate64",
        12 => "BZip2",
        14 => "LZMA",
        93 => "Zstd",
        95 => "XZ",
        96 => "Jpeg",
        98 => "PPMd",
        _ => return None, // unknown method -> undetermined, no badge
    };
    Some(label.to_string())
}

/// Read the WinZip AES (0x9901) extra field's inner compression-method id (the
/// two bytes after the 2-byte version, 2-byte vendor, 1-byte strength). Mirrors
/// `zip_aes_strength`, which reads the strength byte from the same field.
fn zip_aes_inner_method(data: &[u8], start: usize, len: usize) -> Option<u16> {
    let end = (start + len).min(data.len());
    let mut off = start;
    while off + 4 <= end {
        let id = u16::from_le_bytes([data[off], data[off + 1]]);
        let sz = u16::from_le_bytes([data[off + 2], data[off + 3]]) as usize;
        let field = off + 4;
        if id == 0x9901 && field + 7 <= end {
            return Some(u16::from_le_bytes([data[field + 5], data[field + 6]]));
        }
        off = field + sz;
    }
    None
}

/// 7z proactive encryption detection. The 32-byte start header points at the
/// "next header" (offset and size, both u64 LE, relative to the end of the start
/// header). AES-256 encryption appears as the coder id 06F10701 in that header
/// stream, for both encrypted content and an encrypted header (whose outer
/// kEncodedHeader coder is AES too, and whose id is still readable in the
/// referenced next-header region). We read only that region and scan for the id.
/// 7z encryption is AES-256 by format.
fn detect_7z_meta(path: &str) -> Result<ArchiveMeta, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mut start = [0u8; 32];
    f.read_exact(&mut start).map_err(|e| format!("read: {e}"))?;
    const SIG: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
    if start[..6] != SIG {
        return Err("not a 7z archive".to_string());
    }
    let nh_off = u64::from_le_bytes(start[12..20].try_into().unwrap());
    let nh_size = u64::from_le_bytes(start[20..28].try_into().unwrap());
    if nh_size == 0 || nh_size > 16 * 1024 * 1024 {
        return Ok(ArchiveMeta {
            encrypted: false,
            cipher: None,
            compression: None,
        });
    }
    // `nh_off` is an attacker-controlled u64 from the 32-byte start header. `32 +
    // nh_off` overflows (debug panic, release wrap) and is never bounded to the
    // file length; validate both before seeking. (CLAUDE-AV-B1-02)
    let flen = f.metadata().map(|m| m.len()).unwrap_or(0);
    let abs_off = match 32u64.checked_add(nh_off) {
        Some(o) if o <= flen => o,
        _ => {
            return Ok(ArchiveMeta {
                encrypted: false,
                cipher: None,
                compression: None,
            })
        }
    };
    f.seek(SeekFrom::Start(abs_off))
        .map_err(|e| format!("seek: {e}"))?;
    let mut hdr = vec![0u8; nh_size as usize];
    f.read_exact(&mut hdr)
        .map_err(|e| format!("read hdr: {e}"))?;
    const AES_CODER_ID: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];
    let encrypted = hdr.windows(4).any(|w| w == AES_CODER_ID);
    Ok(ArchiveMeta {
        encrypted,
        cipher: encrypted.then(|| "AES-256".to_string()),
        compression: detect_7z_compression(path),
    })
}

/// Best-effort 7z compression method for the Compression column. Parses the
/// archive header via `sevenz-rust2` (metadata only, no extraction), preferring
/// a real compression coder and falling back to "Store" only when every coder is
/// COPY (filters like BCJ/Delta and the AES coder are skipped). Reading the
/// header with an empty password succeeds only for a plaintext header; an `-mhe`
/// encrypted-header archive errors out here, so we return `None` (unknown, no
/// badge) rather than guessing.
fn detect_7z_compression(path: &str) -> Option<String> {
    use sevenz_rust2::{Archive, Password};
    let mut f = std::fs::File::open(path).ok()?;
    let archive = Archive::read(&mut f, &Password::empty()).ok()?;
    let mut saw_store = false;
    for block in &archive.blocks {
        for coder in &block.coders {
            match sevenz_rust2::EncoderMethod::by_id(coder.encoder_method_id())
                .and_then(sevenz_method_label)
                .as_deref()
            {
                Some("Store") => saw_store = true,
                Some(m) => return Some(m.to_string()),
                None => {}
            }
        }
    }
    saw_store.then(|| "Store".to_string())
}

/// Map a `sevenz-rust2` coder to a display token: the named compression method,
/// "Store" for a COPY (uncompressed) stream, or `None` for a chained filter
/// (BCJ/Delta) or AES encryption, so only a compression status surfaces in the
/// Compression column.
fn sevenz_method_label(m: sevenz_rust2::EncoderMethod) -> Option<String> {
    let label = match m.name() {
        "COPY" => "Store",
        "LZMA2" => "LZMA2",
        "LZMA" => "LZMA",
        "PPMD" => "PPMd",
        "BZIP2" => "BZip2",
        "ZSTD" => "Zstd",
        "BROTLI" => "Brotli",
        "LZ4" => "LZ4",
        "LZS" => "LZS",
        "LIZARD" => "Lizard",
        "DEFLATE" => "Deflate",
        "DEFLATE64" => "Deflate64",
        _ => return None, // filters (BCJ/Delta), AES -> not a compression badge
    };
    Some(label.to_string())
}

/// RAR cipher by archive-signature version: RAR5 uses AES-256, legacy RAR4 uses
/// AES-128. The caller only opens this for an already-encrypted archive.
fn detect_rar_cipher(path: &str) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mut sig = [0u8; 8];
    f.read_exact(&mut sig).map_err(|e| format!("read: {e}"))?;
    const RAR5: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01];
    const RAR4: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];
    if sig[..7] == RAR5 {
        Ok("AES-256".to_string())
    } else if sig[..7] == RAR4 {
        Ok("AES-128".to_string())
    } else {
        // Unknown RAR variant: still AES, do not claim a strength we did not read.
        Ok("AES".to_string())
    }
}

/// Map the unrar `FileHeader::method` (RARHeaderDataEx.Method: 0x30 = Store,
/// 0x31..0x35 = the RAR compression levels) to a Compression-column token. RAR
/// uses one proprietary algorithm, so a compressed entry reads simply "RAR"; a
/// stored entry reads "Store".
fn rar_method_label(method: u32) -> String {
    if method == 0x30 || method == 0 {
        "Store".to_string()
    } else {
        "RAR".to_string()
    }
}

/// RAR cipher token from the archive signature, or `None` when the file is not a
/// real RAR (wrong/short magic). Unlike `detect_rar_cipher` (which assumes an
/// already-encrypted RAR and falls back to a bare "AES"), this returns `None` so
/// a corrupt or misnamed `.rar` gets no badge instead of a false padlock. RAR5 =
/// AES-256, RAR4 = AES-128.
fn rar_signature_cipher(path: &str) -> Result<Option<String>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let mut sig = [0u8; 8];
    if f.read_exact(&mut sig).is_err() {
        return Ok(None);
    }
    const RAR5: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01];
    const RAR4: [u8; 7] = [0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];
    Ok(if sig[..7] == RAR5 {
        Some("AES-256".to_string())
    } else if sig[..7] == RAR4 {
        Some("AES-128".to_string())
    } else {
        None
    })
}

/// Proactive RAR metadata: encryption, cipher and compression method, without a
/// password. Confirms a real RAR signature first (a corrupt or misnamed `.rar`
/// reads as undetectable, never a false padlock), then lists the RAR directory
/// (never extracts): an entry flagged encrypted, or a listing that fails part-
/// way, means encrypted; a header-encrypted (`-hp`) archive cannot be listed at
/// all without the password, so it reads as encrypted with an unknown method.
/// Cipher follows the version (RAR5 = AES-256, RAR4 = AES-128). Compression
/// prefers the first real compressed entry ("RAR"), falling back to "Store" only
/// when every entry is stored.
fn detect_rar_meta(path: &str) -> Result<ArchiveMeta, String> {
    let cipher_token = match rar_signature_cipher(path)? {
        Some(c) => c,
        None => return Err("not a RAR archive".to_string()),
    };
    let listing = match unrar::Archive::new(path).open_for_listing() {
        Ok(list) => list,
        // Valid RAR magic but the header stream will not open without a
        // password: a header-encrypted (-hp) archive. Encrypted, method unknown.
        Err(_) => {
            return Ok(ArchiveMeta {
                encrypted: true,
                cipher: Some(cipher_token),
                compression: None,
            });
        }
    };
    let mut encrypted = false;
    let mut method_label: Option<String> = None;
    let mut saw_store = false;
    for entry in listing {
        match entry {
            Ok(e) => {
                if e.is_encrypted() {
                    encrypted = true;
                }
                if method_label.is_none() {
                    match rar_method_label(e.method).as_str() {
                        "Store" => saw_store = true,
                        other => method_label = Some(other.to_string()),
                    }
                }
            }
            // A header read failing part-way through listing indicates header
            // encryption; stop and report encrypted.
            Err(_) => {
                encrypted = true;
                break;
            }
        }
    }
    Ok(ArchiveMeta {
        encrypted,
        cipher: encrypted.then(|| cipher_token.clone()),
        compression: method_label.or_else(|| saw_store.then(|| "Store".to_string())),
    })
}

/// ZIP cipher for the unlock dialog badge of an archive already known to be
/// encrypted. Delegates to the proactive `detect_zip_meta` and maps the
/// not-encrypted case back to an error so the dialog keeps its prior contract.
fn detect_zip_cipher(path: &str) -> Result<String, String> {
    match detect_zip_meta(path)?.cipher {
        Some(cipher) => Ok(cipher),
        None => Err("no encrypted entry in ZIP".to_string()),
    }
}

/// Scan backward (max 64 KiB + 22) for the End Of Central Directory signature.
pub(crate) fn zip_find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let last = data.len() - 22;
    let start = last.saturating_sub(65536);
    (start..=last)
        .rev()
        .find(|&i| data[i..i + 4] == [0x50, 0x4B, 0x05, 0x06])
}

/// Read the WinZip AES (0x9901) extra field strength byte (1=128, 2=192, 3=256).
fn zip_aes_strength(data: &[u8], start: usize, len: usize) -> Option<u16> {
    let end = (start + len).min(data.len());
    let mut off = start;
    while off + 4 <= end {
        let id = u16::from_le_bytes([data[off], data[off + 1]]);
        let sz = u16::from_le_bytes([data[off + 2], data[off + 3]]) as usize;
        let field = off + 4;
        if id == 0x9901 && field + 5 <= end {
            return Some(match data[field + 4] {
                1 => 128,
                2 => 192,
                _ => 256,
            });
        }
        off = field + sz;
    }
    None
}

/// Append one regular file to a tar `Builder`, emitting byte-true progress as the
/// data is read. Replaces `append_path_with_name` (which opens and reads the file
/// itself, giving no progress hook) with an explicit header + `append_data` over a
/// `ProgressReader`. `set_metadata` carries size/mode/mtime; `append_data` sets the
/// checksum, so the resulting entry matches the prior behavior for a stable file.
/// `append_data` writes exactly `header.size()` bytes: as with the old helper, a file
/// that grows mid-archive is truncated to its scan-time size and one that shrinks
/// surfaces a short-read error (the whole compress then fails and the temp is dropped).
fn tar_append_file<W: std::io::Write>(
    archive: &mut tar::Builder<W>,
    abs_path: &std::path::Path,
    rel_path: &str,
    progress: &mut crate::archive_progress::ArchiveProgress,
) -> Result<(), String> {
    let file =
        std::fs::File::open(abs_path).map_err(|e| format!("Failed to add {}: {}", rel_path, e))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("Failed to add {}: {}", rel_path, e))?;
    let mut header = tar::Header::new_gnu();
    header.set_metadata(&meta);
    let reader = crate::archive_progress::ProgressReader::new(file, progress);
    archive
        .append_data(&mut header, rel_path, reader)
        .map_err(|e| format!("Failed to add {}: {}", rel_path, e))
}

/// Compress files/folders into a TAR-based archive.
/// Supports formats: "tar", "tar.gz", "tar.xz", "tar.bz2"
#[tauri::command]
async fn compress_tar(
    paths: Vec<String>,
    output_path: String,
    format: String,
    compression_level: Option<i64>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    compress_tar_impl(paths, output_path, format, compression_level, Some(app)).await
}

async fn compress_tar_impl(
    paths: Vec<String>,
    output_path: String,
    format: String,
    compression_level: Option<i64>,
    app: Option<tauri::AppHandle>,
) -> Result<String, String> {
    use std::fs::File;
    use std::path::Path;
    use walkdir::WalkDir;

    let output = Path::new(&output_path);

    // Collect all files (expanding directories recursively)
    let mut entries: Vec<(std::path::PathBuf, String)> = Vec::new();
    for p in &paths {
        let path = Path::new(p);
        if path.is_dir() {
            for entry in WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if entry.file_type().is_file() {
                    let rel = entry
                        .path()
                        .strip_prefix(path.parent().unwrap_or(path))
                        .unwrap_or(entry.path());
                    entries.push((
                        entry.path().to_path_buf(),
                        rel.to_string_lossy().to_string(),
                    ));
                }
            }
            // Directory entries are created automatically by tar when adding files
        } else if path.is_file() {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            entries.push((path.to_path_buf(), name));
        }
    }

    if entries.is_empty() {
        return Err("No files to compress".to_string());
    }

    // Byte-true progress denominator: sum of the entry sizes actually read.
    let total_bytes: u64 = entries
        .iter()
        .map(|(p, _)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum();
    let mut progress = crate::archive_progress::ArchiveProgress::for_optional_app(
        app,
        crate::archive_progress::phase::COMPRESSING,
        total_bytes,
    );

    // Create the archive based on format (atomic temp; renamed on success).
    let temp = ArchiveTempFile::new(&output_path);
    let file = File::create(temp.path()).map_err(|e| format!("Failed to create archive: {}", e))?;

    match format.as_str() {
        "tar" => {
            let mut archive = tar::Builder::new(file);
            for (abs_path, rel_path) in &entries {
                tar_append_file(&mut archive, abs_path, rel_path, &mut progress)?;
            }
            archive
                .finish()
                .map_err(|e| format!("Failed to finalize tar: {}", e))?;
        }
        "tar.gz" => {
            let gz = flate2::write::GzEncoder::new(
                file,
                flate2::Compression::new(
                    deflate_effective_level(compression_level.unwrap_or(5)) as u32
                ),
            );
            let mut archive = tar::Builder::new(gz);
            for (abs_path, rel_path) in &entries {
                tar_append_file(&mut archive, abs_path, rel_path, &mut progress)?;
            }
            archive
                .into_inner()
                .map_err(|e| format!("Failed to finalize gz: {}", e))?
                .finish()
                .map_err(|e| format!("Failed to finish gz: {}", e))?;
        }
        "tar.xz" => {
            let xz = xz2::write::XzEncoder::new(file, compression_level.unwrap_or(5) as u32);
            let mut archive = tar::Builder::new(xz);
            for (abs_path, rel_path) in &entries {
                tar_append_file(&mut archive, abs_path, rel_path, &mut progress)?;
            }
            archive
                .into_inner()
                .map_err(|e| format!("Failed to finalize xz: {}", e))?
                .finish()
                .map_err(|e| format!("Failed to finish xz: {}", e))?;
        }
        "tar.bz2" => {
            // bzip2 only accepts block sizes 1-9 (there is no level 0 / store mode);
            // clamp so a caller passing 0 gets the lightest real level instead of a panic.
            let bz = bzip2::write::BzEncoder::new(
                file,
                bzip2::Compression::new((compression_level.unwrap_or(5) as u32).clamp(1, 9)),
            );
            let mut archive = tar::Builder::new(bz);
            for (abs_path, rel_path) in &entries {
                tar_append_file(&mut archive, abs_path, rel_path, &mut progress)?;
            }
            archive
                .into_inner()
                .map_err(|e| format!("Failed to finalize bz2: {}", e))?
                .finish()
                .map_err(|e| format!("Failed to finish bz2: {}", e))?;
        }
        _ => return Err(format!("Unsupported format: {}", format)),
    }

    temp.commit(&output_path)?;
    progress.finish();

    let file_count = entries.len();
    Ok(format!(
        "Compressed {} files into {}",
        file_count,
        output.display()
    ))
}

#[tauri::command]
async fn compress_single(
    input_path: String,
    output_path: String,
    format: String,
    compression_level: Option<i64>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    compress_single_impl(
        input_path,
        output_path,
        format,
        compression_level,
        Some(app),
    )
    .await
}

/// Compress a SINGLE file into a standalone gzip/xz/bzip2 stream (no tar wrapper).
///
/// gz/xz/bz2 are single-stream codecs: the container holds exactly one member and
/// carries no file name or directory structure, so this rejects folders and only
/// ever receives one input. The GUI restricts the picker to a lone file and the
/// CLI rejects multi-path invocations; this guard is the backend backstop.
async fn compress_single_impl(
    input_path: String,
    output_path: String,
    format: String,
    compression_level: Option<i64>,
    app: Option<tauri::AppHandle>,
) -> Result<String, String> {
    use std::fs::File;
    use std::path::Path;

    let src = Path::new(&input_path);
    if !src.is_file() {
        return Err("gzip/xz/bzip2 compress a single file only (not a folder)".to_string());
    }

    // Byte-true progress denominator: the source file size (what we read through).
    let total_bytes = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let mut progress = crate::archive_progress::ArchiveProgress::for_optional_app(
        app,
        crate::archive_progress::phase::COMPRESSING,
        total_bytes,
    );

    // Atomic temp; renamed into place on success (Drop cleans it up on any error).
    let temp = ArchiveTempFile::new(&output_path);
    let out = File::create(temp.path()).map_err(|e| format!("Failed to create archive: {}", e))?;
    let infile = File::open(src).map_err(|e| format!("Failed to open input: {}", e))?;
    // Count bytes as the encoder pulls them through the source (uncompressed payload).
    let mut reader = crate::archive_progress::ProgressReader::new(infile, &mut progress);

    // Preset 0-9, default 5 (7-Zip "Normal"), matching the tar family. gzip/xz honor
    // the whole 0-9 range; bzip2 has no level 0, so it floors at 1.
    let level = compression_level.unwrap_or(5).clamp(0, 9) as u32;

    match format.as_str() {
        "gz" => {
            let mut enc = flate2::write::GzEncoder::new(
                out,
                flate2::Compression::new(deflate_effective_level(level as i64) as u32),
            );
            std::io::copy(&mut reader, &mut enc).map_err(|e| format!("gzip: {}", e))?;
            enc.finish().map_err(|e| format!("gzip finish: {}", e))?;
        }
        "xz" => {
            let mut enc = xz2::write::XzEncoder::new(out, level);
            std::io::copy(&mut reader, &mut enc).map_err(|e| format!("xz: {}", e))?;
            enc.finish().map_err(|e| format!("xz finish: {}", e))?;
        }
        "bz2" => {
            let mut enc = bzip2::write::BzEncoder::new(out, bzip2::Compression::new(level.max(1)));
            std::io::copy(&mut reader, &mut enc).map_err(|e| format!("bzip2: {}", e))?;
            enc.finish().map_err(|e| format!("bzip2 finish: {}", e))?;
        }
        other => return Err(format!("Unsupported standalone format: {}", other)),
    }

    temp.commit(&output_path)?;
    progress.finish();
    Ok(output_path)
}

/// The decompressed member name for a standalone codec file: the archive's own
/// name with only the trailing codec extension removed (`report.txt.gz` ->
/// `report.txt`, `data.xz` -> `data`). A single-stream container carries no file
/// name, so this reconstructs it from the archive name. `file_name`-derived and
/// suffix-stripped, so the result is always a single path component (no traversal).
/// Falls back to a safe name when the archive is literally just the extension.
fn single_stream_member_name(archive_name: &str, codec: &str) -> String {
    let ext = match codec {
        "gz" => ".gz",
        "xz" => ".xz",
        "bz2" => ".bz2",
        _ => "",
    };
    let lower = archive_name.to_ascii_lowercase();
    if !ext.is_empty() && lower.ends_with(ext) {
        let stem = &archive_name[..archive_name.len() - ext.len()];
        if !stem.is_empty() {
            return stem.to_string();
        }
    }
    // Forced codec on a name without that extension, or an empty stem: never emit
    // an empty filename.
    let base = archive_name.trim_end_matches('.');
    if base.is_empty() {
        "extracted".to_string()
    } else {
        format!("{}.out", base)
    }
}

/// Decode a standalone single-stream codec file (gz/xz/bz2 with no tar wrapper)
/// back to its lone member. `kind` forces the codec ("gz" | "xz" | "bz2"); when
/// None it is sniffed from the extension, so the CLI's `--archive-format` stays
/// authoritative on extract exactly like the tar lane. `create_subfolder` nests
/// the output under a per-archive stem folder (matching the other extractors).
/// Returns the destination directory. These files are never encrypted.
async fn extract_single_impl(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
    kind: Option<String>,
) -> Result<String, String> {
    use std::fs::File;
    use std::path::Path;

    let codec = match kind {
        Some(k) => k,
        None => {
            let lower = archive_path.to_ascii_lowercase();
            if lower.ends_with(".gz") {
                "gz".to_string()
            } else if lower.ends_with(".xz") {
                "xz".to_string()
            } else if lower.ends_with(".bz2") {
                "bz2".to_string()
            } else {
                return Err(format!(
                    "Unrecognized single-stream format: {}",
                    archive_path
                ));
            }
        }
    };

    let archive_name = Path::new(&archive_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let member_name = single_stream_member_name(&archive_name, &codec);

    let dest_dir = if create_subfolder {
        Path::new(&output_dir).join(archive_extract_stem(&archive_name))
    } else {
        Path::new(&output_dir).to_path_buf()
    };
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;
    let out_path = dest_dir.join(&member_name);

    let compressed_len = std::fs::metadata(&archive_path)
        .map(|m| m.len())
        .unwrap_or(0);
    // A raw gz/xz/bz2 stream has no reliable declared uncompressed size, so cap the
    // plaintext at max(compressed * ratio, floor): a few-KB bomb cannot exceed the
    // floor, while a legit large file scales with its own compressed size.
    // (CLAUDE-AV-B1-05)
    let cap = compressed_len
        .saturating_mul(SINGLE_STREAM_MAX_RATIO)
        .max(SINGLE_STREAM_ABS_FLOOR);
    let infile = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let mut reader: Box<dyn std::io::Read> = match codec.as_str() {
        "gz" => Box::new(flate2::read::GzDecoder::new(infile)),
        "xz" => Box::new(xz2::read::XzDecoder::new(infile)),
        "bz2" => Box::new(bzip2::read::BzDecoder::new(infile)),
        other => return Err(format!("Unrecognized single-stream format: {}", other)),
    };
    let mut outfile =
        File::create(&out_path).map_err(|e| format!("Failed to create output file: {}", e))?;
    let written = {
        let mut limited = std::io::Read::take(&mut *reader, cap.saturating_add(1));
        std::io::copy(&mut limited, &mut outfile)
            .map_err(|e| format!("Failed to decompress: {}", e))?
    };
    if written > cap {
        let _ = std::fs::remove_file(&out_path);
        return Err("Decompressed stream exceeds the size limit (compression bomb?)".to_string());
    }

    Ok(dest_dir.to_string_lossy().to_string())
}

/// Extract a standalone single-stream codec file (gz/xz/bz2, no tar wrapper) back
/// to its lone member. Codec sniffed from the extension; never encrypted.
#[tauri::command]
async fn extract_single(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
) -> Result<String, String> {
    extract_single_impl(archive_path, output_dir, create_subfolder, None).await
}

/// Build a tar-stream reader for `archive_path`. An explicit `kind`
/// ("tar" | "tar.gz" | "tar.xz" | "tar.bz2") forces the decompression filter;
/// otherwise it is sniffed from the file extension. This lets the CLI honor an
/// explicit `--archive-format` on extract while the GUI keeps extension sniffing.
fn tar_reader_for(
    archive_path: &str,
    kind: Option<&str>,
) -> Result<Box<dyn std::io::Read>, String> {
    let file =
        std::fs::File::open(archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let pick = match kind {
        Some(k) => k.to_string(),
        None => {
            let ext = archive_path.to_lowercase();
            if ext.ends_with(".tar.gz") || ext.ends_with(".tgz") {
                "tar.gz".to_string()
            } else if ext.ends_with(".tar.xz") || ext.ends_with(".txz") {
                "tar.xz".to_string()
            } else if ext.ends_with(".tar.bz2") || ext.ends_with(".tbz2") {
                "tar.bz2".to_string()
            } else if ext.ends_with(".tar") {
                "tar".to_string()
            } else {
                return Err(format!("Unrecognized archive format: {}", ext));
            }
        }
    };
    Ok(match pick.as_str() {
        "tar.gz" => Box::new(flate2::read::GzDecoder::new(file)),
        "tar.xz" => Box::new(xz2::read::XzDecoder::new(file)),
        "tar.bz2" => Box::new(bzip2::read::BzDecoder::new(file)),
        "tar" => Box::new(file),
        other => return Err(format!("Unrecognized archive format: {}", other)),
    })
}

/// Resolve the destination directory for a tar extraction, creating a per-archive
/// subfolder (stripping a trailing `.tar` for `.tar.gz` etc.) when requested.
fn tar_final_output(
    archive_path: &str,
    output_dir: &str,
    create_subfolder: bool,
) -> Result<std::path::PathBuf, String> {
    use std::path::Path;
    let out = Path::new(output_dir);
    if create_subfolder {
        let stem = Path::new(archive_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let folder_name = if stem.ends_with(".tar") {
            stem.trim_end_matches(".tar").to_string()
        } else {
            stem.to_string()
        };
        let subfolder = out.join(&folder_name);
        std::fs::create_dir_all(&subfolder).map_err(|e| format!("Failed to create dir: {}", e))?;
        Ok(subfolder)
    } else {
        Ok(out.to_path_buf())
    }
}

/// Unpack a tar stream into `final_output`, skipping unsafe (traversal / absolute)
/// entries. Shared by extension-sniffing and format-forced extraction so both
/// enforce the identical path-traversal guard.
///
/// Returns the destination path and the skipped-link notes as SEPARATE values:
/// the path must stay a clean directory string (the CLI walks it to size the
/// extraction), so skip notes are never smuggled into it.
fn tar_unpack(
    reader: Box<dyn std::io::Read>,
    final_output: &std::path::Path,
) -> Result<(String, Vec<String>), String> {
    use std::fs::File;
    let mut ar = tar::Archive::new(reader);

    // Skipped-link notes so an unsafe (or unsupported) link is never dropped silently.
    let mut link_reports: Vec<String> = Vec::new();

    // C5: Iterate entries manually with path traversal validation
    // instead of using unpack() which extracts blindly
    for entry_result in ar
        .entries()
        .map_err(|e| format!("Failed to read tar entries: {}", e))?
    {
        let mut entry = entry_result.map_err(|e| format!("Failed to read tar entry: {}", e))?;

        let entry_path = entry
            .path()
            .map_err(|e| format!("Failed to get entry path: {}", e))?
            .to_string_lossy()
            .to_string();

        // Skip entries with unsafe paths (traversal, absolute, drive letters)
        if !is_safe_archive_entry(&entry_path) {
            continue;
        }

        let out_path = final_output.join(&entry_path);

        let et = entry.header().entry_type();
        if et.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("Failed to create directory '{}': {}", entry_path, e))?;
        } else if et.is_symlink() || et.is_hard_link() {
            // Symlink / hardlink entries carry a target path, not file bytes. The
            // pre-existing code blindly ran File::create + io::copy, which silently
            // turned every link into an empty regular file (silent-drop bug). Recreate
            // in-root links faithfully; skip + report any link whose target could escape
            // the extraction root (absolute, drive-letter or any ".." component), and
            // never materialize an unsafe link.
            let link_kind = if et.is_symlink() {
                "symlink"
            } else {
                "hardlink"
            };
            let target = match entry
                .link_name()
                .map_err(|e| format!("Failed to read link target for '{}': {}", entry_path, e))?
            {
                Some(t) => t,
                None => {
                    link_reports.push(format!(
                        "skipped ({} without target): {}",
                        link_kind, entry_path
                    ));
                    continue;
                }
            };
            let tstr = target.to_string_lossy();

            // SECURITY: validate the target with the SAME guard used for entry paths,
            // so an absolute target or any ".." component is rejected. Conservative:
            // a legit in-root relative target that starts with ".." is also skipped.
            if !is_safe_archive_entry(&tstr) {
                link_reports.push(format!(
                    "skipped (unsafe link target): {} -> {}",
                    entry_path, tstr
                ));
                continue;
            }

            // Ensure parent directory exists (mirror the regular-file branch).
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    format!("Failed to create parent dir for '{}': {}", entry_path, e)
                })?;
            }

            #[cfg(unix)]
            {
                // Overwrite semantics on re-extract: File::create truncates an
                // existing regular file, but symlink()/hard_link() fail EEXIST,
                // so extracting the same tar twice died at the first link. Remove
                // a pre-existing file or link at the path (NotFound is fine).
                let _ = std::fs::remove_file(&out_path);
                if et.is_symlink() {
                    std::os::unix::fs::symlink(&*target, &out_path)
                        .map_err(|e| format!("Failed to create symlink '{}': {}", entry_path, e))?;
                } else {
                    // Hardlink target is an in-root path relative to the extraction root.
                    std::fs::hard_link(final_output.join(&*target), &out_path).map_err(|e| {
                        format!("Failed to create hardlink '{}': {}", entry_path, e)
                    })?;
                }
            }
            #[cfg(not(unix))]
            {
                // No portable symlink/hardlink creation guarantee off Unix: skip + report
                // rather than fail the whole extraction.
                link_reports.push(format!(
                    "skipped ({} unsupported on this platform): {} -> {}",
                    link_kind, entry_path, tstr
                ));
            }
        } else {
            // Ensure parent directory exists
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    format!("Failed to create parent dir for '{}': {}", entry_path, e)
                })?;
            }

            let mut outfile = File::create(&out_path)
                .map_err(|e| format!("Failed to create file '{}': {}", entry_path, e))?;
            let declared = entry.header().size().unwrap_or(0);
            copy_entry_bounded(&mut entry, &mut outfile, declared)
                .map_err(|e| format!("Failed to extract '{}': {}", entry_path, e))?;
        }
    }

    for note in &link_reports {
        eprintln!("tar_unpack: {}", note);
    }

    Ok((final_output.to_string_lossy().to_string(), link_reports))
}

/// Extract TAR-based archives (auto-detects tar, tar.gz, tar.xz, tar.bz2 from extension)
#[tauri::command]
async fn extract_tar(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
) -> Result<String, String> {
    let final_output = tar_final_output(&archive_path, &output_dir, create_subfolder)?;
    let reader = tar_reader_for(&archive_path, None)?;
    tar_unpack(reader, &final_output).map(|(dest, _skipped)| dest)
}

/// As `extract_tar_core`, but the tar filter is forced by `kind`
/// ("tar" | "tar.gz" | "tar.xz" | "tar.bz2") instead of sniffed from the
/// extension, so the CLI's `--archive-format` is authoritative on extract.
pub async fn extract_tar_as_core(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
    kind: String,
) -> Result<String, String> {
    let final_output = tar_final_output(&archive_path, &output_dir, create_subfolder)?;
    let reader = tar_reader_for(&archive_path, Some(&kind))?;
    tar_unpack(reader, &final_output).map(|(dest, _skipped)| dest)
}

/// Extract a RAR archive with optional password
#[tauri::command]
async fn extract_rar(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<String, String> {
    use std::path::Path;

    let final_output = if create_subfolder {
        let archive_name = Path::new(&archive_path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "extracted".to_string());
        Path::new(&output_dir).join(&archive_name)
    } else {
        Path::new(&output_dir).to_path_buf()
    };

    std::fs::create_dir_all(&final_output)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    // Wrap password in SecretString for zeroization on drop
    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let archive = if let Some(ref pwd) = secret_password {
        unrar::Archive::with_password(&archive_path, pwd.expose_secret().as_bytes())
    } else {
        unrar::Archive::new(&archive_path)
    };

    let mut archive = archive
        .open_for_processing()
        .map_err(|e| format!("Failed to open RAR archive: {}", e))?;

    while let Some(header) = archive
        .read_header()
        .map_err(|e| format!("Failed to read RAR header: {}", e))?
    {
        let entry_name = header.entry().filename.to_string_lossy().to_string();

        // C5: Skip entries with unsafe paths (traversal, absolute, drive letters)
        if !is_safe_archive_entry(&entry_name) {
            archive = header
                .skip()
                .map_err(|e| format!("Failed to skip RAR entry: {}", e))?;
            continue;
        }

        // Defense in depth: the `unrar` crate does not expose a symlink target
        // before extraction, so we cannot validate it the way `tar_unpack` does.
        // Skip symlink entries entirely (Unix host attributes carry S_IFLNK in the
        // low mode bits) rather than trusting the bundled UnRAR library's own
        // traversal guard as the only barrier. zip/7z likewise never materialize
        // symlinks, so this keeps RAR consistent. (CLAUDE-AV-B1-06)
        const S_IFMT: u32 = 0o170000;
        const S_IFLNK: u32 = 0o120000;
        if header.entry().file_attr & S_IFMT == S_IFLNK {
            archive = header
                .skip()
                .map_err(|e| format!("Failed to skip RAR symlink entry: {}", e))?;
            continue;
        }

        archive = if header.entry().is_file() {
            header
                .extract_with_base(&final_output)
                .map_err(|e| format!("Failed to extract RAR entry: {}", e))?
        } else {
            header
                .skip()
                .map_err(|e| format!("Failed to skip RAR entry: {}", e))?
        };
    }

    Ok(final_output.to_string_lossy().to_string())
}

/// Check if a RAR archive is password protected
#[tauri::command]
async fn is_rar_encrypted(archive_path: String) -> Result<bool, String> {
    let archive = unrar::Archive::new(&archive_path)
        .open_for_listing()
        .map_err(|e| format!("Failed to open RAR archive: {}", e))?;

    for entry in archive {
        match entry {
            Ok(e) => {
                if e.is_encrypted() {
                    return Ok(true);
                }
            }
            Err(_) => return Ok(true), // If listing fails, assume encrypted
        }
    }

    Ok(false)
}

// ─── OS "Extract here / to folder" intent (Deliverable G) ────────────────────
//
// These back the dedicated lightweight `extract` window (Option B): the file
// manager verbs launch `aeroftp --extract-here <path>` / `--extract-to <path>`,
// the argv is parsed by `parse_extract_intent`, and a tiny webview that loads
// `extract.html` is opened WITHOUT booting the main app (no vault unlock, no
// sync). The clear-archive "Extract here" case is handled by the CLI directly
// and never reaches this code. The window calls `extract_probe` to learn the
// container kind + whether a password is needed, then (for "Extract to folder")
// `resolve_unique_extract_dir` to derive a never-clobbering stem subfolder.

/// One-shot probe result handed to the dedicated extract window so it knows
/// which extractor to drive and whether to prompt for a password.
#[derive(Debug, serde::Serialize)]
struct ExtractProbe {
    /// "zip" | "sevenz" | "rar" | "tar" | "single" | "aerozip" | "aerovault_v2" | "aerovault_v3"
    kind: String,
    /// True when extraction needs a password (encrypted general archive, or any
    /// non-plaintext aero container). `.aerozip` plaintext is always false.
    encrypted: bool,
    /// Archive size in bytes, so the window can pass the toast threshold to
    /// `runExtractWithToast` without a second stat.
    archive_bytes: u64,
}

/// Strip the full archive extension from a file name, returning the stem used to
/// name an "Extract to folder" subfolder. Handles the multi-part tar extensions
/// (.tar.gz / .tar.xz / .tar.bz2) and the aero* + general single extensions,
/// falling back to a last-dot strip. Pure: unit-tested.
fn archive_extract_stem(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    for ext in [".tar.gz", ".tar.xz", ".tar.bz2"] {
        if lower.ends_with(ext) {
            return file_name[..file_name.len() - ext.len()].to_string();
        }
    }
    for ext in [
        ".tgz",
        ".txz",
        ".tbz2",
        ".tar",
        ".zip",
        ".7z",
        ".rar",
        ".aerozip",
        ".aerovault",
    ] {
        if lower.ends_with(ext) {
            return file_name[..file_name.len() - ext.len()].to_string();
        }
    }
    match file_name.rfind('.') {
        Some(i) if i > 0 => file_name[..i].to_string(),
        _ => file_name.to_string(),
    }
}

/// Resolve a never-clobbering destination subfolder for "Extract to folder":
/// `parent/stem`, or `parent/stem (2)`, `(3)`, ... if earlier candidates exist.
/// `exists` abstracts the filesystem so the policy is unit-testable. Pure.
fn unique_extract_dir_with<P: Fn(&std::path::Path) -> bool>(
    parent: &std::path::Path,
    archive_name: &str,
    exists: P,
) -> Result<std::path::PathBuf, String> {
    let stem = archive_extract_stem(archive_name);
    let stem = if stem.trim().is_empty() {
        "extracted"
    } else {
        stem.as_str()
    };
    let first = parent.join(stem);
    if !exists(&first) {
        return Ok(first);
    }
    for n in 2..10_000u32 {
        let cand = parent.join(format!("{stem} ({n})"));
        if !exists(&cand) {
            return Ok(cand);
        }
    }
    Err("could not find a free destination folder".to_string())
}

#[tauri::command]
async fn resolve_unique_extract_dir(
    parent_dir: String,
    archive_name: String,
) -> Result<String, String> {
    let parent = std::path::Path::new(&parent_dir);
    let dir = unique_extract_dir_with(parent, &archive_name, |p| p.exists())?;
    Ok(dir.to_string_lossy().to_string())
}

/// Recognize a multi-volume / split-archive *part* by its (lowercased) file name
/// so the probe can reject it with a specific message instead of the generic
/// "Unsupported archive type". Covers the split-volume naming produced by 7-Zip
/// and WinZip/WinRAR, all of which end in a numeric part index:
///   - 7-Zip / generic split:   `foo.7z.001`, `foo.zip.001`  (`.<ext>.NNN`)
///   - WinZip split ZIP:        `foo.z01`, `foo.z02`         (`.zNN`)
///   - old-style RAR volumes:   `foo.r00`, `foo.r01`         (`.rNN`)
///
/// New-style `.partN.rar` is deliberately NOT matched (it ends in a letter, never
/// a digit): the unrar backend follows the co-located `.partN.rar` (and old-style
/// `.rNN`) volumes automatically when the FIRST volume is opened, so a real
/// multi-part RAR set extracts correctly through the normal `.rar` lane. We only
/// intercept the part names that (a) are never a valid extraction entry point and
/// (b) fall to the generic error today. Pure: unit-tested.
fn is_multivolume_part(lower: &str) -> bool {
    // Split trailing run of ASCII digits; every split scheme ends in a part index.
    let head = lower.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = lower.len() - head.len();
    if digits == 0 {
        return false;
    }
    // `foo.7z.001` / `foo.zip.001`: an inner archive extension then a numeric part.
    if head.ends_with(".7z.") || head.ends_with(".zip.") {
        return true;
    }
    // `foo.z01` (split ZIP) / `foo.r00` (old-style RAR): a single letter then >= 2
    // digits. Requiring the `.z`/`.r` prefix keeps `.zip`, `.rar`, `.gz`, `.xz`
    // (no trailing digits, filtered above) and `.bz2` (only one digit) off this lane.
    if digits >= 2 && (head.ends_with(".z") || head.ends_with(".r")) {
        return true;
    }
    false
}

/// Probe an archive/vault path for the dedicated extract window. Aero containers
/// are detected by content magic first (the extension is cosmetic for them), so
/// an encrypted `.aerozip` is correctly routed to the v3 vault extractor.
#[tauri::command]
async fn extract_probe(path: String) -> Result<ExtractProbe, String> {
    let archive_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    // Aero containers first: detect_aero_container reads the AEROVAULT3 header.
    // (`aerovault` in lib.rs is the local module, so reach the helpers and the
    // external crate version check through `crate::aerovault_v3::*`.)
    if let Some(fmt) = crate::aerovault_v3::detect_aero_container(path.clone()).await? {
        if fmt == "zip" {
            return Ok(ExtractProbe {
                kind: "aerozip".to_string(),
                encrypted: false,
                archive_bytes,
            });
        }
        // Encrypted aero container: pick the extractor by header version.
        let kind = if crate::aerovault_v3::is_vault_v3(path.clone()).await? {
            "aerovault_v3"
        } else {
            "aerovault_v2"
        };
        return Ok(ExtractProbe {
            kind: kind.to_string(),
            encrypted: true,
            archive_bytes,
        });
    }

    // General formats by extension; encryption sniffed per format.
    let lower = path.to_ascii_lowercase();
    // Reject split/multi-volume PARTS up front, with a specific message instead of
    // the generic error below. Checked before the `.zip`/`.7z`/`.rar` branches
    // because a part like `foo.7z.001` does not end with `.7z`; `.partN.rar` is
    // intentionally excluded (unrar follows those volumes on the normal `.rar` lane).
    if is_multivolume_part(&lower) {
        return Err(
            "multi-volume (split) archives are not supported: rejoin the \
                    volumes with 7-Zip/WinRAR into a single archive, then extract"
                .to_string(),
        );
    }
    if lower.ends_with(".zip") {
        let encrypted = is_zip_encrypted(path.clone()).await?;
        return Ok(ExtractProbe {
            kind: "zip".to_string(),
            encrypted,
            archive_bytes,
        });
    }
    if lower.ends_with(".7z") {
        let encrypted = is_7z_encrypted(path.clone()).await?;
        return Ok(ExtractProbe {
            kind: "sevenz".to_string(),
            encrypted,
            archive_bytes,
        });
    }
    if lower.ends_with(".rar") {
        let encrypted = is_rar_encrypted(path.clone()).await?;
        return Ok(ExtractProbe {
            kind: "rar".to_string(),
            encrypted,
            archive_bytes,
        });
    }
    if lower.ends_with(".tar")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".txz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tbz2")
    {
        return Ok(ExtractProbe {
            kind: "tar".to_string(),
            encrypted: false,
            archive_bytes,
        });
    }
    // Standalone single-stream codecs (no tar wrapper). Checked AFTER the tar
    // family so `.tar.gz` / `.tgz` etc. stay `tar`; a bare `.gz`/`.xz`/`.bz2`
    // reaching here is a lone-member file. Never encrypted.
    if lower.ends_with(".gz") || lower.ends_with(".xz") || lower.ends_with(".bz2") {
        return Ok(ExtractProbe {
            kind: "single".to_string(),
            encrypted: false,
            archive_bytes,
        });
    }

    Err(format!("Unsupported archive type: {path}"))
}

/// Monotonic label sequence so concurrent extract intents each get a window.
static EXTRACT_WINDOW_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Recognize the OS "Extract here / to folder" verbs in a process argv, returning
/// `(mode, canonical_path)` where mode is "here" or "to". The path is canonicalized
/// and must be an existing file (same validation as the .aerovault open intent).
fn parse_extract_intent(argv: &[String]) -> Option<(String, String)> {
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        let mode = match arg.as_str() {
            "--extract-here" => "here",
            "--extract-to" => "to",
            _ => continue,
        };
        let raw = iter.next()?;
        let canonical = std::fs::canonicalize(raw).ok()?;
        if canonical
            .symlink_metadata()
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some((mode.to_string(), canonical.to_string_lossy().to_string()));
        }
        return None;
    }
    None
}

/// Open the dedicated lightweight `extract` window for the given intent. The
/// `mode`/`path` are injected as `window.__AEROFTP_EXTRACT__` before the page
/// scripts run (same mechanism as the splash version injection). The main window
/// is intentionally left untouched: this never boots the full app.
/// Two-letter desktop language code from the locale environment (default "en"),
/// so the dedicated extract window reads in the same language as the OS (and the
/// Nautilus verbs), not whatever language the main app was last left in.
fn detect_desktop_lang() -> String {
    for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            let code = val
                .split(':')
                .next()
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("")
                .split('_')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if !code.is_empty() {
                return code;
            }
        }
    }
    "en".to_string()
}

fn open_extract_window(app: &AppHandle, mode: &str, path: &str) {
    let payload = serde_json::json!({ "mode": mode, "path": path, "lang": detect_desktop_lang() })
        .to_string();
    let init = format!("window.__AEROFTP_EXTRACT__ = {payload};");

    let url: WebviewUrl = {
        #[cfg(dev)]
        {
            WebviewUrl::App("extract.html".into())
        }
        #[cfg(all(not(dev), target_os = "linux"))]
        {
            WebviewUrl::External(
                url::Url::parse("http://127.0.0.1:14321/extract.html")
                    .expect("valid localhost URL"),
            )
        }
        #[cfg(all(not(dev), not(target_os = "linux")))]
        {
            WebviewUrl::App("extract.html".into())
        }
    };

    let n = EXTRACT_WINDOW_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let label = if n == 0 {
        "extract".to_string()
    } else {
        format!("extract-{n}")
    };

    let builder = WebviewWindowBuilder::new(app, &label, url)
        .title("AeroFTP")
        .inner_size(460.0, 320.0)
        .min_inner_size(380.0, 240.0)
        .resizable(false)
        .center()
        .initialization_script(&init);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.decorations(false);
    let builder = match portable::webview_data_dir() {
        Some(dir) => builder.data_directory(dir),
        None => builder,
    };
    if let Err(e) = builder.build() {
        log::error!("Failed to open extract window ({label}): {e}");
    }
}

#[tauri::command]
async fn ftp_read_file_base64(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
    path: String,
    max_size_mb: Option<u32>,
) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    // Preview size cap, supplied by the caller so backend and frontend share one
    // limit. The preview path passes the UI's MAX_PREVIEW_SIZE_BYTES (25 MB);
    // thumbnails pass a smaller value. Defaults to 25 MB. Previously hard-coded to
    // 10 MB, which rejected full-resolution photos (10-25 MB) the UI allowed
    // (issue #128 item B).
    let max_size: u64 = (max_size_mb.unwrap_or(25) as u64) * 1024 * 1024;

    // Try provider path first (cloud providers, GitHub, etc.).
    // Mirrors `preview_remote_file` so image / binary preview works on every
    // backend, not just FTP/SFTP/WebDAV.
    let provider_connected = {
        let guard = provider_state.provider.lock().await;
        guard.is_some()
    };

    if provider_connected {
        let mut guard = provider_state.provider.lock().await;
        if let Some(provider) = guard.as_mut() {
            let file_size = provider.size(&path).await.unwrap_or(0);
            if file_size > max_size {
                return Err(format!(
                    "File too large for preview ({:.1} MB). Max: {} MB",
                    file_size as f64 / 1024.0 / 1024.0,
                    max_size / (1024 * 1024)
                ));
            }

            let data = provider
                .download_to_bytes(&path)
                .await
                .map_err(|e| format!("Failed to download: {}", e))?;
            return Ok(STANDARD.encode(data));
        }
    }

    // Fallback to FTP manager for FTP/SFTP/WebDAV connections.
    let mut ftp_manager = state.ftp_manager.lock().await;

    let file_size = ftp_manager.get_file_size(&path).await.unwrap_or(0);

    if file_size > max_size {
        return Err(format!(
            "File too large for preview ({:.1} MB). Max: {} MB",
            file_size as f64 / 1024.0 / 1024.0,
            max_size / (1024 * 1024)
        ));
    }

    let data = ftp_manager
        .download_to_bytes(&path)
        .await
        .map_err(|e| format!("Failed to download: {}", e))?;

    Ok(STANDARD.encode(data))
}

// ============ DevTools Commands ============

#[tauri::command]
async fn read_local_file(path: String, max_size_mb: Option<u32>) -> Result<String, String> {
    validate_path(&path)?;
    // Size cap to prevent OOM on large text files (default 10MB)
    let max_size: u64 = (max_size_mb.unwrap_or(10) as u64) * 1024 * 1024;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| "Failed to read file metadata".to_string())?;
    if metadata.len() > max_size {
        return Err(format!(
            "File too large for text preview ({:.1} MB). Max: {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            max_size / (1024 * 1024)
        ));
    }

    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;

    // Detect binary content (null bytes in first 8KB)
    let check_len = bytes.len().min(8192);
    let null_count = bytes[..check_len].iter().filter(|&&b| b == 0).count();
    if null_count > 0 {
        return Err(
            "Binary file detected (contains null bytes). Use read_file_base64 for binary files."
                .to_string(),
        );
    }

    String::from_utf8(bytes).map_err(|_| {
        "File contains invalid UTF-8. Use read_file_base64 for binary files.".to_string()
    })
}

#[tauri::command]
async fn read_local_file_base64(path: String, max_size_mb: Option<u32>) -> Result<String, String> {
    validate_path(&path)?;
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    // Default max size is 50MB for media files (audio/video)
    let max_size: u64 = (max_size_mb.unwrap_or(50) as u64) * 1024 * 1024;

    // Check file size first
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;

    if metadata.len() > max_size {
        return Err(format!(
            "File too large for preview ({:.1} MB). Max: {} MB",
            metadata.len() as f64 / (1024.0 * 1024.0),
            max_size / (1024 * 1024)
        ));
    }

    // Read file as binary
    let content = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    // Encode as base64
    Ok(STANDARD.encode(&content))
}

#[tauri::command]
async fn preview_remote_file(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
    path: String,
) -> Result<String, String> {
    let temp_path = std::env::temp_dir().join(format!(
        "aeroftp_preview_{}",
        chrono::Utc::now().timestamp_millis()
    ));
    let temp_path_str = temp_path.to_string_lossy().to_string();

    // Try provider path first (cloud providers, GitHub, etc.)
    let provider_connected = {
        let guard = provider_state.provider.lock().await;
        guard.is_some()
    };

    if provider_connected {
        let mut guard = provider_state.provider.lock().await;
        if let Some(provider) = guard.as_mut() {
            provider
                .download(&path, &temp_path_str, None)
                .await
                .map_err(|e| format!("Failed to download for preview: {}", e))?;

            let content = tokio::fs::read_to_string(&temp_path)
                .await
                .map_err(|e| format!("Failed to read preview content: {}", e))?;

            let _ = tokio::fs::remove_file(&temp_path).await;
            return Ok(content);
        }
    }

    // Fallback to FTP manager for FTP/SFTP connections
    let mut ftp_manager = state.ftp_manager.lock().await;

    // Download file content to memory (limit to 1MB for preview)
    let max_size: u64 = 1024 * 1024; // 1MB limit

    // Get file size first
    let file_size = ftp_manager.get_file_size(&path).await.unwrap_or(0);

    if file_size > max_size {
        return Err(format!(
            "File too large for preview ({} KB). Max: 1024 KB",
            file_size / 1024
        ));
    }

    ftp_manager
        .download_file_with_progress(&path, &temp_path_str, |_| true)
        .await
        .map_err(|e| format!("Failed to download for preview: {}", e))?;

    // Read content
    let content = tokio::fs::read_to_string(&temp_path)
        .await
        .map_err(|e| format!("Failed to read preview content: {}", e))?;

    // Clean up temp file
    let _ = tokio::fs::remove_file(&temp_path).await;

    Ok(content)
}

// ============ Favicon Detection ============

/// Parse manifest.json/site.webmanifest to find the best icon path
fn parse_manifest_icons(json_bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(json_bytes).ok()?;
    let icons = value.get("icons")?.as_array()?;

    // Find best icon: prefer PNG ≥48px, fallback to first available
    let mut best: Option<(String, u32)> = None;
    for icon in icons {
        let src = match icon.get("src").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => continue,
        };
        // Parse sizes like "48x48", "192x192"
        let size = icon
            .get("sizes")
            .and_then(|s| s.as_str())
            .and_then(|s| s.split('x').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);

        let is_png = src.ends_with(".png")
            || icon
                .get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.contains("png"));

        match &best {
            None => best = Some((src.to_string(), size)),
            Some((_, best_size)) => {
                // Prefer sizes between 48-192, favor PNG
                if ((48..=192).contains(&size)
                    && (!(48..=192).contains(best_size) || (is_png && size >= *best_size)))
                    || (*best_size == 0 && size > 0)
                {
                    best = Some((src.to_string(), size));
                }
            }
        }
    }

    best.map(|(src, _)| src)
}

/// Guess MIME type from file extension (SVG rejected for XSS safety)
fn guess_mime_from_path(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".svg") {
        return Some("image/svg+xml");
    }
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".ico") {
        Some("image/x-icon")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else {
        Some("image/png")
    }
}

/// Validate SVG content: must contain <svg tag (safe when rendered via <img> data URL)
fn is_valid_svg(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .map(|s| s.contains("<svg"))
        .unwrap_or(false)
}

/// Validate image magic bytes (defense-in-depth against content spoofing)
fn is_valid_image_magic(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    // PNG
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return true;
    }
    // JPEG
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return true;
    }
    // ICO / CUR
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0x02, 0x00])
    {
        return true;
    }
    // GIF
    if bytes.starts_with(b"GIF8") {
        return true;
    }
    // WebP
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return true;
    }
    false
}

/// Convert bytes to base64 data URL
fn bytes_to_data_url(bytes: &[u8], mime: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    format!("data:{};base64,{}", mime, STANDARD.encode(bytes))
}

/// Resolve an icon path from manifest relative to the base directory.
/// In FTP context, "/" in manifest means the web root (= base), not the FTP root.
fn resolve_icon_path(base: &str, icon_src: &str) -> Option<String> {
    if icon_src.starts_with("http://") || icon_src.starts_with("https://") {
        return None; // Can't download absolute URLs via FTP
    }
    // Reject path traversal and null bytes
    if icon_src.contains("..") || icon_src.contains('\0') {
        return None;
    }
    let prefix = base.trim_end_matches('/');
    let clean_src = icon_src.trim_start_matches('/');
    if clean_src.is_empty() {
        return None;
    }
    if prefix.is_empty() {
        Some(format!("/{}", clean_src))
    } else {
        Some(format!("{}/{}", prefix, clean_src))
    }
}

/// Build path for a file in a base directory
fn make_path(base: &str, filename: &str) -> String {
    let prefix = base.trim_end_matches('/');
    if prefix.is_empty() {
        format!("/{}", filename)
    } else {
        format!("{}/{}", prefix, filename)
    }
}

/// Detect favicon from FTP server using the project's remote root path.
/// Uses SIZE command (control channel only) to check file existence before
/// downloading, preventing FTP data connection corruption on 550 errors.
/// Times out after 10 seconds to avoid holding the FTP mutex too long.
#[tauri::command]
async fn detect_server_favicon(
    state: State<'_, AppState>,
    search_paths: Vec<String>,
) -> Result<Option<String>, String> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut ftp_manager = state.ftp_manager.lock().await;

        for base in &search_paths {
            // 1) Try icon files in order of preference
            for (filename, mime, use_magic) in &[
                ("favicon.ico", "image/x-icon", true),
                ("icon.png", "image/png", true),
                ("icon.svg", "image/svg+xml", false),
            ] {
                let path = make_path(base, filename);
                let file_size = ftp_manager.get_file_size(&path).await.unwrap_or(0);
                if file_size == 0 || file_size > 500_000 {
                    continue;
                }
                if let Ok(bytes) = ftp_manager.download_to_bytes(&path).await {
                    if bytes.is_empty() {
                        continue;
                    }
                    let valid = if *use_magic {
                        is_valid_image_magic(&bytes)
                    } else {
                        is_valid_svg(&bytes)
                    };
                    if valid {
                        return Ok(Some(bytes_to_data_url(&bytes, mime)));
                    }
                }
            }

            // 2) Fallback: manifest.json / site.webmanifest → parse icon
            for name in &["manifest.json", "site.webmanifest"] {
                let manifest_path = make_path(base, name);
                let manifest_size = ftp_manager.get_file_size(&manifest_path).await.unwrap_or(0);
                if manifest_size == 0 || manifest_size > 500_000 {
                    continue;
                }

                if let Ok(manifest_bytes) = ftp_manager.download_to_bytes(&manifest_path).await {
                    if manifest_bytes.is_empty() {
                        continue;
                    }
                    if let Some(icon_src) = parse_manifest_icons(&manifest_bytes) {
                        if let Some(icon_full) = resolve_icon_path(base, &icon_src) {
                            let icon_size =
                                ftp_manager.get_file_size(&icon_full).await.unwrap_or(0);
                            if icon_size > 0 && icon_size <= 500_000 {
                                if let Ok(icon_bytes) =
                                    ftp_manager.download_to_bytes(&icon_full).await
                                {
                                    if !icon_bytes.is_empty() && is_valid_image_magic(&icon_bytes) {
                                        if let Some(mime) = guess_mime_from_path(&icon_full) {
                                            return Ok(Some(bytes_to_data_url(&icon_bytes, mime)));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    })
    .await;
    match result {
        Ok(inner) => inner,
        Err(_) => Ok(None), // Timeout: no favicon found
    }
}

/// Detect favicon from SFTP/provider server using the project's remote root path.
/// Times out after 10 seconds to avoid holding the provider mutex too long.
#[tauri::command]
async fn detect_provider_favicon(
    state: State<'_, provider_commands::ProviderState>,
    search_paths: Vec<String>,
) -> Result<Option<String>, String> {
    // Safety net for Fix H: never probe a crypt-overlay session for a web
    // favicon. An encrypted store has no web project, so the reads only waste
    // I/O and flood the remote log; the frontend already skips crypt sessions,
    // this guards any other caller and the wrapped-provider path.
    if state
        .active_crypt_overlay
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Ok(None);
    }

    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut provider_lock = state.provider.lock().await;
        let provider: &mut Box<dyn providers::StorageProvider> = provider_lock
            .as_mut()
            .ok_or("Not connected to any provider")?;

        if provider.provider_type() == providers::ProviderType::S3 {
            return Ok(None);
        }

        for base in &search_paths {
            // 1) Try icon files in order of preference
            for (filename, mime, use_magic) in &[
                ("favicon.ico", "image/x-icon", true),
                ("icon.png", "image/png", true),
                ("icon.svg", "image/svg+xml", false),
            ] {
                if let Ok(bytes) = provider.download_to_bytes(&make_path(base, filename)).await {
                    if bytes.is_empty() || bytes.len() > 500_000 {
                        continue;
                    }
                    let valid = if *use_magic {
                        is_valid_image_magic(&bytes)
                    } else {
                        is_valid_svg(&bytes)
                    };
                    if valid {
                        return Ok(Some(bytes_to_data_url(&bytes, mime)));
                    }
                }
            }

            // 2) Fallback: manifest.json / site.webmanifest → parse icon
            for name in &["manifest.json", "site.webmanifest"] {
                let manifest_path = make_path(base, name);
                if let Ok(manifest_bytes) = provider.download_to_bytes(&manifest_path).await {
                    if manifest_bytes.is_empty() || manifest_bytes.len() > 500_000 {
                        continue;
                    }
                    if let Some(icon_src) = parse_manifest_icons(&manifest_bytes) {
                        if let Some(icon_full) = resolve_icon_path(base, &icon_src) {
                            if let Ok(icon_bytes) = provider.download_to_bytes(&icon_full).await {
                                if !icon_bytes.is_empty()
                                    && icon_bytes.len() <= 500_000
                                    && is_valid_image_magic(&icon_bytes)
                                {
                                    if let Some(mime) = guess_mime_from_path(&icon_full) {
                                        return Ok(Some(bytes_to_data_url(&icon_bytes, mime)));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    })
    .await;
    match result {
        Ok(inner) => inner,
        Err(_) => Ok(None), // Timeout: no favicon found
    }
}

#[tauri::command]
async fn save_local_file(path: String, content: String) -> Result<(), String> {
    validate_path(&path)?;

    // Additional hardened validation (M63: match ai_tools validate_path level)
    let normalized = path.replace('\\', "/");
    for component in normalized.split('/') {
        if component == ".." {
            return Err("Path traversal ('..') not allowed".to_string());
        }
    }
    let resolved = std::fs::canonicalize(&path).or_else(|_| {
        std::path::Path::new(&path)
            .parent()
            .map(std::fs::canonicalize)
            .unwrap_or(Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no parent",
            )))
    });
    if let Ok(canonical) = resolved {
        let s = canonical.to_string_lossy();
        let denied = [
            "/proc",
            "/sys",
            "/dev",
            "/boot",
            "/root",
            "/etc/shadow",
            "/etc/passwd",
            "/etc/ssh",
            "/etc/sudoers",
        ];
        if denied.iter().any(|d| s.starts_with(d)) {
            return Err(format!("Access to system path denied: {}", s));
        }
    }

    // Atomic write: temp file + rename prevents corruption on crash/power-loss (M35)
    let target = std::path::Path::new(&path);
    let parent = target
        .parent()
        .ok_or_else(|| "Cannot determine parent directory".to_string())?;
    let tmp_path = parent.join(format!(
        ".aeroftp_save_{}.tmp",
        chrono::Utc::now().timestamp_millis()
    ));
    tokio::fs::write(&tmp_path, &content).await.map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("Failed to write temp file: {}", e)
    })?;
    tokio::fs::rename(&tmp_path, &path).await.map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("Failed to finalize file save: {}", e)
    })?;

    Ok(())
}

#[tauri::command]
async fn save_remote_file(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
    path: String,
    content: String,
) -> Result<(), String> {
    // Fail-closed: the remote editor save is a direct provider write, so it must
    // refuse when the session has a crypt overlay that is currently unwrapped
    // (badge locked / stepped outside the encrypted scope). Without this an
    // edit-in-place saved after the overlay was cleared would inject plaintext
    // into the encrypted store, mirroring the guard on provider_upload_file.
    provider_state.guard_no_raw_crypt_write("Save file")?;

    // Write content to temp file first
    let temp_path = std::env::temp_dir().join(format!(
        "aeroftp_upload_{}",
        chrono::Utc::now().timestamp_millis()
    ));
    let temp_path_str = temp_path.to_string_lossy().to_string();

    tokio::fs::write(&temp_path, &content)
        .await
        .map_err(|e| format!("Failed to write temp file: {}", e))?;

    // Try provider path first (cloud providers, GitHub, etc.)
    let provider_connected = {
        let guard = provider_state.provider.lock().await;
        guard.is_some()
    };

    if provider_connected {
        let mut guard = provider_state.provider.lock().await;
        if let Some(provider) = guard.as_mut() {
            // Try AeroRsync delta upload first when the destination is an
            // SFTP provider with key-based auth and a remote rsync helper.
            // This makes editor saves of large files (CSS, JSON, logs, code)
            // ship only the changed bytes instead of the full content.
            // try_delta_transfer returns None for non-SFTP providers, so the
            // classic upload is the natural fallback.
            let delta_attempt = if crate::delta_sync_rsync::gui_delta_enabled() {
                crate::delta_sync_rsync::try_delta_transfer(
                    provider.as_mut(),
                    crate::delta_sync_rsync::SyncDirection::Upload,
                    &temp_path,
                    &path,
                )
                .await
            } else {
                None
            };
            if let Some(delta) = delta_attempt {
                if delta.used_delta {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    return Ok(());
                }
                if let Some(err) = delta.hard_error {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    return Err(format!("delta save rejected: {err}"));
                }
                // fallback_reason set: declined gracefully, fall through to
                // the classic upload below.
            }

            let result = provider.upload(&temp_path_str, &path, None).await;
            let _ = tokio::fs::remove_file(&temp_path).await;
            return result.map_err(|e| format!("Failed to save file: {}", e));
        }
    }

    // Fallback to FTP manager
    let mut ftp_manager = state.ftp_manager.lock().await;
    ftp_manager
        .upload_file_with_progress(&temp_path_str, &path, content.len() as u64, |_| true)
        .await
        .map_err(|e| format!("Failed to upload file: {}", e))?;

    let _ = tokio::fs::remove_file(&temp_path).await;
    Ok(())
}

// ============ Splash Screen ============

/// Global flag: set to true once app_ready has run, so the safety timeout
/// does not re-show the main window after the user has already closed it.
static APP_READY_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// User preference: when true, clicking the window close button hides to the
/// system tray instead of quitting the app. Set by the frontend via
/// `set_close_to_tray` on startup and whenever the user toggles the option.
static CLOSE_TO_TRAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a usable system tray was created at startup. False on minimal or
/// immutable Linux distros that ship without libappindicator (#362): in that
/// case close-to-tray must not hide the window, or it would vanish with no
/// tray to restore it from. Defaults true so every other platform is unchanged.
static TRAY_AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[tauri::command]
fn set_close_to_tray(enabled: bool) {
    CLOSE_TO_TRAY.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

/// Timestamp when splash screen was created: used to enforce minimum display time.
static SPLASH_CREATED_AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Returns true if the app was launched via the OS autostart entry.
/// Detected by the `--autostart` arg passed by tauri-plugin-autostart.
#[tauri::command]
fn is_autostart_launch() -> bool {
    std::env::args().any(|a| a == "--autostart")
}

/// Called by the frontend when React has finished initializing.
/// Emits detailed main-window state to the logs right after a `show()` attempt.
///
/// macOS (Tahoe especially) can report a successful `show()` while the window
/// never actually presents (issue #290): only the Dock icon appears. These
/// lines let a tester's terminal capture the real post-show state so we can
/// tell "shown but off-screen / on a missing monitor" from "shown but the
/// compositor never presented it". Cheap, log-only, all platforms.
fn log_window_diagnostics(window: &tauri::WebviewWindow, ctx: &str) {
    match window.is_visible() {
        Ok(v) => info!("[diag #290] {ctx}: is_visible={v}"),
        Err(e) => warn!("[diag #290] {ctx}: is_visible error: {e}"),
    }
    if let Ok(m) = window.is_minimized() {
        info!("[diag #290] {ctx}: is_minimized={m}");
    }
    if let Ok(f) = window.is_focused() {
        info!("[diag #290] {ctx}: is_focused={f}");
    }
    if let Ok(scale) = window.scale_factor() {
        info!("[diag #290] {ctx}: scale_factor={scale}");
    }
    if let Ok(pos) = window.outer_position() {
        info!("[diag #290] {ctx}: outer_position=({}, {})", pos.x, pos.y);
    }
    if let Ok(size) = window.inner_size() {
        info!(
            "[diag #290] {ctx}: inner_size={}x{}",
            size.width, size.height
        );
    }
    match window.current_monitor() {
        Ok(Some(mon)) => {
            let ms = mon.size();
            let mp = mon.position();
            info!(
                "[diag #290] {ctx}: monitor={:?} size={}x{} origin=({}, {})",
                mon.name(),
                ms.width,
                ms.height,
                mp.x,
                mp.y
            );
        }
        Ok(None) => warn!("[diag #290] {ctx}: current_monitor=None (window off all monitors?)"),
        Err(e) => warn!("[diag #290] {ctx}: current_monitor error: {e}"),
    }
}

/// Minimum sane inner size for the main window, mirroring the `min_inner_size`
/// passed to the builder. Used both as a builder constraint reference and to
/// detect a degenerate restored size (issue #290).
const MAIN_MIN_INNER_W: f64 = 1024.0;
const MAIN_MIN_INNER_H: f64 = 600.0;

/// Compute the initial inner size for the main window, clamped to the primary
/// monitor so the window never opens off-screen on small Retina displays.
/// Shared by the builder (fresh launch) and the post-restore self-heal so a
/// poisoned/degenerate restored size falls back to exactly the dimensions a
/// first launch would have used. Falls back to 1540x1050 when the monitor
/// cannot be probed (very early startup, headless). See issue #241/#290.
fn computed_initial_inner_size(app: &AppHandle) -> (f64, f64) {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let size = m.size().to_logical::<f64>(m.scale_factor());
            let target_w = 1540.0_f64.min((size.width - 60.0_f64).max(MAIN_MIN_INNER_W));
            let target_h = 1050.0_f64.min((size.height - 100.0_f64).max(MAIN_MIN_INNER_H));
            (target_w, target_h)
        })
        .unwrap_or((1540.0, 1050.0))
}

/// Heal a degenerate window size restored by the window-state plugin.
///
/// Issue #290: the borderless macOS builds (<= v4.0.2) never presented the
/// main window, so its inner size stayed 0x0 for the whole session. On exit
/// the plugin refuses to *save* a 0x0 size, but it still saves the position,
/// leaving a non-default record `{x, y, width: 0, height: 0}` on disk. Because
/// that record differs from `WindowState::default()`, the next launch's
/// `restore_state(SIZE)` enters its restore branch and faithfully calls
/// `set_size(0, 0)` — so even after the v4.0.3 Overlay chrome fix the window
/// opens at 0x0: visible and focused, but invisible (matching the `[diag #290]
/// inner_size=0x0` report). We detect any inner size below the minimum and
/// reset it to the computed initial size, then re-center. Cross-platform so it
/// repairs already-poisoned state files without the user deleting anything.
fn restored_size_is_degenerate(logical_w: f64, logical_h: f64) -> bool {
    logical_w < MAIN_MIN_INNER_W || logical_h < MAIN_MIN_INNER_H
}

fn heal_restored_window_size(window: &tauri::WebviewWindow) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let Ok(size) = window.inner_size() else {
        return;
    };
    let logical = size.to_logical::<f64>(scale);
    if !restored_size_is_degenerate(logical.width, logical.height) {
        return;
    }
    let (w, h) = computed_initial_inner_size(window.app_handle());
    warn!(
        "[diag #290] restored inner_size {}x{} below minimum {}x{}; healing to {}x{}",
        size.width, size.height, MAIN_MIN_INNER_W as u32, MAIN_MIN_INNER_H as u32, w, h
    );
    let _ = window.set_size(tauri::LogicalSize::new(w, h));
    let _ = window.center();
}

/// Closes the splash screen, sets the app menu (deferred from setup to
/// prevent GTK menu flash on the borderless splash), and shows the main window.
///
/// `start_minimized`: when true, the main window stays hidden and the user
/// only sees the tray icon: used for launches from the OS autostart entry.
#[tauri::command]
async fn app_ready(app: AppHandle, start_minimized: Option<bool>) {
    use tauri_plugin_window_state::{StateFlags, WindowExt};
    let start_minimized = start_minimized.unwrap_or(false);

    // IMPORTANT: Do NOT set APP_READY_DONE here! Setting it early creates a race
    // condition: rebuild_menu sees the flag, calls app.set_menu() globally, and GTK
    // applies it to the splash window that hasn't been destroyed yet → menu flash.
    // The flag is set at the very END, after splash is dead and menu is installed.

    // 0. Enforce minimum splash display time (2s) so users can read version/license
    //    and the window has time to fully render even on fast machines / Wayland.
    const MIN_SPLASH_SECS: f64 = 2.0;
    if let Some(created) = SPLASH_CREATED_AT.get() {
        let elapsed = created.elapsed().as_secs_f64();
        if elapsed < MIN_SPLASH_SECS {
            let remaining = std::time::Duration::from_secs_f64(MIN_SPLASH_SECS - elapsed);
            info!("Splash minimum wait: {remaining:?}");
            tokio::time::sleep(remaining).await;
        }
    }

    // app_ready is an ASYNC command, so it runs on the async runtime and OFF the
    // GTK main thread. Every GTK/GLib touch below (splash teardown, app menu,
    // window restore/show, monitor + scale queries) MUST be marshalled onto the
    // main thread via `run_on_main_thread`; doing it directly from here races the
    // GLib main loop and corrupts the GLib heap, which later aborts a GDBus worker
    // with "malloc(): unaligned fastbin chunk detected" (intermittent, surfaces on
    // suspend / monitor standby). Same discipline as tray_badge::update_tray_badge.
    // The async sleeps that pace the splash teardown stay off the main thread.

    // 1. Close splash on the main thread (GTK window destruction, ~500ms async).
    {
        let app_main = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(splash) = app_main.get_webview_window("splashscreen") {
                let _ = splash.close();
                info!("Splash screen closed");
            }
        });
    }

    // 2. Wait for GTK to fully destroy the splash window (Linux only).
    // During this wait, rebuild_menu still sees APP_READY_DONE==false and defers.
    // macOS/Windows do not use GTK and do not need this delay.
    #[cfg(target_os = "linux")]
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // 3 + 4. On the main thread: install the deferred app menu (splash is dead now),
    // restore the saved size/position/maximized state, heal a poisoned 0x0 size
    // (#290) and show the window. APP_READY_DONE is flipped LAST, inside this same
    // main-thread closure after the menu is installed, preserving the invariant
    // rebuild_menu relies on (it defers its own set_menu while the flag is false).
    {
        let app_main = app.clone();
        let _ = app.run_on_main_thread(move || {
            // 3. Splash is dead: safe to set the global app menu.
            if let Some(deferred) =
                app_main.try_state::<std::sync::Mutex<Option<tauri::menu::Menu<tauri::Wry>>>>()
            {
                if let Ok(mut guard) = deferred.lock() {
                    if let Some(menu) = guard.take() {
                        let _ = app_main.set_menu(menu);
                        info!("App menu set (deferred)");
                    }
                }
            }

            // 4. Restore saved size/position/maximized state, then show the main
            // window without a menu (the frontend controls visibility via
            // toggle_menu_bar). When start_minimized is true (autostart launch +
            // user opt-in) the window stays hidden and the user reaches the app
            // via the tray icon.
            if let Some(main_window) = app_main.get_webview_window("main") {
                let _ = main_window.remove_menu();
                let _ = main_window
                    .restore_state(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED);
                // Heal a poisoned 0x0 size restored from a saved state (#290) before
                // the window is shown, so it never flashes at zero size on that path.
                log_window_diagnostics(&main_window, "app_ready pre-show");
                heal_restored_window_size(&main_window);
                if start_minimized {
                    info!("Main window kept hidden (autostart minimized)");
                } else {
                    let _ = main_window.show();
                    let _ = main_window.set_focus();
                    info!("Main window shown");
                    log_window_diagnostics(&main_window, "app_ready post-show");
                    // macOS 26 Tahoe: the window can collapse to a 0x0 content frame
                    // at show() time even with a valid built size and no saved state
                    // at all (#290). Re-assert the size once it is on screen; the
                    // +300ms/+1200ms re-heal below catches a collapse that lands a
                    // frame or two after present.
                    heal_restored_window_size(&main_window);
                }
            }

            // 5. LAST (still on the main thread, right after the menu is installed):
            // let rebuild_menu call app.set_menu() freely and tell the safety
            // timeout it does not need to fire.
            APP_READY_DONE.store(true, Ordering::SeqCst);
        });
    }

    // 4b. macOS/#290 follow-up: the 0x0 collapse can land a frame or two after
    // present, so re-heal at +300ms and +1200ms. The delays stay off the main
    // thread; only the GTK queries + heal are marshalled back onto it.
    if !start_minimized {
        let app_spawn = app.clone();
        tauri::async_runtime::spawn(async move {
            for (delay_ms, ctx) in [(300u64, "app_ready +300ms"), (1200u64, "app_ready +1200ms")] {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                let app_cb = app_spawn.clone();
                let _ = app_spawn.run_on_main_thread(move || {
                    if let Some(w) = app_cb.get_webview_window("main") {
                        log_window_diagnostics(&w, ctx);
                        heal_restored_window_size(&w);
                    }
                });
            }
        });
    }
}

#[tauri::command]
fn toggle_menu_bar(app: AppHandle, window: tauri::Window, visible: bool) {
    if visible {
        if let Some(menu) = app.menu() {
            let _ = window.set_menu(menu);
        }
    } else {
        let _ = window.remove_menu();
    }
}

#[tauri::command]
fn rebuild_menu(
    app: AppHandle,
    labels: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};

    let accel = |shortcut: &'static str| -> Option<&'static str> {
        #[cfg(target_os = "linux")]
        {
            let _ = shortcut;
            None
        }
        #[cfg(not(target_os = "linux"))]
        {
            Some(shortcut)
        }
    };

    let get = |key: &str, fallback: &str| -> String {
        labels
            .get(key)
            .cloned()
            .unwrap_or_else(|| fallback.to_string())
    };

    let quit = MenuItem::with_id(
        &app,
        "quit",
        get("quit", "Quit AeroFTP"),
        true,
        accel("CmdOrCtrl+Q"),
    )
    .map_err(|e| e.to_string())?;
    let about = MenuItem::with_id(
        &app,
        "about",
        get("about", "About AeroFTP"),
        true,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let settings = MenuItem::with_id(
        &app,
        "settings",
        get("settings", "Settings..."),
        true,
        accel("CmdOrCtrl+,"),
    )
    .map_err(|e| e.to_string())?;
    let refresh = MenuItem::with_id(
        &app,
        "refresh",
        get("refresh", "Refresh"),
        true,
        accel("CmdOrCtrl+R"),
    )
    .map_err(|e| e.to_string())?;
    let shortcuts = MenuItem::with_id(
        &app,
        "shortcuts",
        get("shortcuts", "Keyboard Shortcuts"),
        true,
        accel("F1"),
    )
    .map_err(|e| e.to_string())?;
    let support = MenuItem::with_id(
        &app,
        "support",
        get("support", "Support Development"),
        true,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;

    let file_menu = Submenu::with_items(
        &app,
        get("file", "File"),
        true,
        &[
            &MenuItem::with_id(
                &app,
                "new_folder",
                get("newFolder", "New Folder"),
                true,
                accel("CmdOrCtrl+N"),
            )
            .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &settings,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(
                &app,
                "toggle_debug_mode",
                get("debugMode", "Debug Mode"),
                true,
                accel("CmdOrCtrl+Shift+F12"),
            )
            .map_err(|e| e.to_string())?,
            &MenuItem::with_id(
                &app,
                "show_dependencies",
                get("dependencies", "Dependencies..."),
                true,
                None::<&str>,
            )
            .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &quit,
        ],
    )
    .map_err(|e| e.to_string())?;

    let edit_menu = Submenu::with_items(
        &app,
        get("edit", "Edit"),
        true,
        &[
            &PredefinedMenuItem::undo(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::redo(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::cut(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::copy(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::paste(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::select_all(&app, None).map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(&app, "rename", get("rename", "Rename"), true, accel("F2"))
                .map_err(|e| e.to_string())?,
            &MenuItem::with_id(
                &app,
                "delete",
                get("delete", "Delete"),
                true,
                accel("Delete"),
            )
            .map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;

    let devtools_submenu = Submenu::with_items(
        &app,
        get("devtools", "DevTools"),
        true,
        &[
            &MenuItem::with_id(
                &app,
                "toggle_devtools",
                get("toggleDevtools", "Toggle DevTools"),
                true,
                accel("CmdOrCtrl+Shift+D"),
            )
            .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(
                &app,
                "toggle_editor",
                get("toggleEditor", "Toggle Editor"),
                true,
                accel("CmdOrCtrl+1"),
            )
            .map_err(|e| e.to_string())?,
            &MenuItem::with_id(
                &app,
                "toggle_terminal",
                get("toggleTerminal", "Toggle Terminal"),
                true,
                accel("CmdOrCtrl+2"),
            )
            .map_err(|e| e.to_string())?,
            &MenuItem::with_id(
                &app,
                "toggle_agent",
                get("toggleAgent", "Toggle Agent"),
                true,
                accel("CmdOrCtrl+3"),
            )
            .map_err(|e| e.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;

    let view_menu = Submenu::with_items(
        &app,
        get("view", "View"),
        true,
        &[
            &refresh,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &MenuItem::with_id(
                &app,
                "toggle_theme",
                get("toggleTheme", "Toggle Theme"),
                true,
                accel("CmdOrCtrl+T"),
            )
            .map_err(|e| e.to_string())?,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &devtools_submenu,
        ],
    )
    .map_err(|e| e.to_string())?;

    let check_update_item = MenuItem::with_id(
        &app,
        "check_update",
        get("checkForUpdates", "Check for Updates"),
        true,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;

    let help_menu = Submenu::with_items(
        &app,
        get("help", "Help"),
        true,
        &[
            &check_update_item,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &shortcuts,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &support,
            &PredefinedMenuItem::separator(&app).map_err(|e| e.to_string())?,
            &about,
        ],
    )
    .map_err(|e| e.to_string())?;

    let menu = Menu::with_items(&app, &[&file_menu, &edit_menu, &view_menu, &help_menu])
        .map_err(|e| e.to_string())?;

    // If splash is still open (APP_READY_DONE==false), store menu for later -
    // don't set globally (GTK applies global menus to ALL windows, causing flash).
    if !APP_READY_DONE.load(Ordering::SeqCst) {
        if let Some(deferred) =
            app.try_state::<std::sync::Mutex<Option<tauri::menu::Menu<tauri::Wry>>>>()
        {
            if let Ok(mut guard) = deferred.lock() {
                *guard = Some(menu);
            }
        }
    } else {
        app.set_menu(menu).map_err(|e| e.to_string())?;
    }

    // Defense-in-depth: if splash somehow still exists, strip its menu
    if let Some(splash) = app.get_webview_window("splashscreen") {
        let _ = splash.remove_menu();
    }

    Ok(())
}

// ============ Sync Commands ============

use cloud_config::{CloudConfig, CloudSyncStatus, ConflictStrategy};
use sync::{
    build_comparison_results_with_index, classify_sync_error, delete_sync_journal,
    journal_sig_filename, load_sync_index, load_sync_journal, save_sync_index, save_sync_journal,
    select_canary_sample, should_exclude, sign_journal, verify_local_file, CanaryResult,
    CanarySampleResult, CanarySummary, CompareOptions, FileComparison, FileInfo, RetryPolicy,
    SyncEcStatus, SyncErrorInfo, SyncIndex, SyncJournal, VerifyPolicy, VerifyResult,
};

#[derive(Debug, Clone, Serialize)]
struct SyncEcCommandResult {
    status: SyncEcStatus,
    sidecar_path: Option<String>,
    message: Option<String>,
}

fn sync_ec_message(status: SyncEcStatus, message: impl Into<String>) -> SyncEcCommandResult {
    SyncEcCommandResult {
        status,
        sidecar_path: None,
        message: Some(message.into()),
    }
}

fn remote_missing_error_text(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("does not exist")
        || lower.contains("550")
}

async fn sync_ec_use_provider(
    provider_state: &provider_commands::ProviderState,
    is_provider: Option<bool>,
) -> bool {
    if let Some(is_provider) = is_provider {
        return is_provider;
    }
    provider_state.provider.lock().await.is_some()
}

async fn sync_ec_upload_remote_file(
    state: &AppState,
    provider_state: &provider_commands::ProviderState,
    is_provider: Option<bool>,
    local_path: &str,
    remote_path: &str,
) -> Result<(), String> {
    if sync_ec_use_provider(provider_state, is_provider).await {
        let mut provider_lock = provider_state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;
        provider
            .upload(local_path, remote_path, None)
            .await
            .map_err(|e| e.to_string())
    } else {
        let mut ftp_manager = state.ftp_manager.lock().await;
        ftp_manager
            .upload_file(local_path, remote_path)
            .await
            .map_err(|e| e.to_string())
    }
}

async fn sync_ec_download_remote_bytes(
    state: &AppState,
    provider_state: &provider_commands::ProviderState,
    is_provider: Option<bool>,
    remote_path: &str,
) -> Result<Option<Vec<u8>>, String> {
    if sync_ec_use_provider(provider_state, is_provider).await {
        let tmp = tempfile::NamedTempFile::new()
            .map_err(|e| format!("create AeroSync EC temp download: {e}"))?;
        let tmp_path = tmp.path().to_string_lossy().to_string();
        let mut provider_lock = provider_state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;
        match provider.download(remote_path, &tmp_path, None).await {
            Ok(()) => std::fs::read(tmp.path())
                .map(Some)
                .map_err(|e| format!("read AeroSync EC temp download: {e}")),
            Err(crate::providers::ProviderError::NotFound(_)) => Ok(None),
            Err(e) if remote_missing_error_text(&e.to_string()) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    } else {
        let mut ftp_manager = state.ftp_manager.lock().await;
        match ftp_manager.download_to_bytes(remote_path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if remote_missing_error_text(&e.to_string()) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn sync_ec_generate(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
    local_path: String,
    remote_path: String,
    relative_path: String,
    pct: Option<u32>,
    max_file_size: Option<u64>,
    is_provider: Option<bool>,
) -> Result<SyncEcCommandResult, String> {
    let options = sync::SyncErrorCorrectionOptions {
        enabled: true,
        pct: pct.unwrap_or(crate::error_correction::ERROR_CORRECTION_DEFAULT_PCT),
        max_file_size: max_file_size
            .unwrap_or(crate::error_correction::aerosync::AEROSYNC_EC_MAX_FILE_SIZE),
        max_overhead_pct: 0,
    };
    let generated = match crate::error_correction::aerosync::generate_sync_sidecar_for_file_capped(
        &relative_path,
        Path::new(&local_path),
        options.pct(),
        options.max_file_size(),
        options.max_overhead_pct(),
    ) {
        Ok(result) => result,
        Err(e) => return Ok(sync_ec_message(SyncEcStatus::GenerateFailed, e)),
    };
    let sidecar = match generated {
        crate::error_correction::aerosync::SyncEcGenerateResult::Generated(sidecar) => sidecar,
        crate::error_correction::aerosync::SyncEcGenerateResult::SkippedTooLarge { .. } => {
            return Ok(SyncEcCommandResult {
                status: SyncEcStatus::SkippedTooLarge,
                sidecar_path: None,
                message: None,
            });
        }
        crate::error_correction::aerosync::SyncEcGenerateResult::SkippedLowBenefit { .. } => {
            return Ok(SyncEcCommandResult {
                status: SyncEcStatus::SkippedLowBenefit,
                sidecar_path: None,
                message: None,
            });
        }
    };

    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new()
        .map_err(|e| format!("create AeroSync EC temp sidecar: {e}"))?;
    tmp.write_all(&sidecar.sidecar_bytes)
        .map_err(|e| format!("write AeroSync EC temp sidecar: {e}"))?;
    tmp.flush()
        .map_err(|e| format!("flush AeroSync EC temp sidecar: {e}"))?;
    let tmp_path = tmp.path().to_string_lossy().to_string();
    let sidecar_path =
        crate::error_correction::aerosync::sync_error_correction_sidecar_path(&remote_path);

    match sync_ec_upload_remote_file(
        &state,
        &provider_state,
        is_provider,
        &tmp_path,
        &sidecar_path,
    )
    .await
    {
        Ok(()) => Ok(SyncEcCommandResult {
            status: SyncEcStatus::Generated,
            sidecar_path: Some(sidecar_path),
            message: None,
        }),
        Err(e) => Ok(SyncEcCommandResult {
            status: SyncEcStatus::GenerateFailed,
            sidecar_path: Some(sidecar_path),
            message: Some(e),
        }),
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn sync_ec_verify_repair(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
    local_path: String,
    remote_path: String,
    relative_path: String,
    expected_sha256: Option<String>,
    expected_mtime: Option<String>,
    is_provider: Option<bool>,
) -> Result<SyncEcCommandResult, String> {
    let Some(expected_sha256) = expected_sha256.filter(|s| !s.trim().is_empty()) else {
        return Ok(SyncEcCommandResult {
            status: SyncEcStatus::MissingExpectedHash,
            sidecar_path: None,
            message: None,
        });
    };
    let expected_sha256 =
        match crate::error_correction::aerosync::parse_sha256_hex(&expected_sha256) {
            Ok(hash) => hash,
            Err(e) => return Ok(sync_ec_message(SyncEcStatus::VerifyFailed, e)),
        };
    let sidecar_path =
        crate::error_correction::aerosync::sync_error_correction_sidecar_path(&remote_path);
    let Some(sidecar_bytes) =
        sync_ec_download_remote_bytes(&state, &provider_state, is_provider, &sidecar_path).await?
    else {
        return Ok(SyncEcCommandResult {
            status: SyncEcStatus::MissingSidecar,
            sidecar_path: Some(sidecar_path),
            message: None,
        });
    };

    match crate::error_correction::aerosync::verify_repair_sync_file(
        &relative_path,
        &expected_sha256,
        Path::new(&local_path),
        &sidecar_bytes,
    ) {
        Ok(crate::error_correction::aerosync::SyncEcRepairResult::Verified) => {
            Ok(SyncEcCommandResult {
                status: SyncEcStatus::Verified,
                sidecar_path: Some(sidecar_path),
                message: None,
            })
        }
        Ok(crate::error_correction::aerosync::SyncEcRepairResult::Repaired { .. }) => {
            preserve_remote_mtime(&local_path, expected_mtime.as_deref());
            Ok(SyncEcCommandResult {
                status: SyncEcStatus::Repaired,
                sidecar_path: Some(sidecar_path),
                message: None,
            })
        }
        Err(e) => Ok(SyncEcCommandResult {
            status: SyncEcStatus::VerifyFailed,
            sidecar_path: Some(sidecar_path),
            message: Some(e),
        }),
    }
}

#[tauri::command]
async fn compare_directories(
    app: AppHandle,
    state: State<'_, AppState>,
    local_path: String,
    remote_path: String,
    options: Option<CompareOptions>,
) -> Result<Vec<FileComparison>, String> {
    let mut options = options.unwrap_or_default();
    sync::apply_error_correction_excludes(&mut options);

    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    info!(
        "Comparing directories: local={}, remote={}",
        local_path, remote_path
    );

    // Reset the shared cancel flag so this compare starts clean.
    // A user who cancelled a previous operation may have left the flag in whatever
    // state; we take ownership of it for the duration of this compare and rely on
    // `cancel_transfer` to flip it back to true if the user asks for a stop.
    state.cancel_flag.store(false, Ordering::Relaxed);

    // Emit scan phase: scanning (both local and remote concurrently)
    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "local",
            "files_found": 0,
        }),
    );

    // Run local and remote scans concurrently (F2 optimization)
    // Local scan runs on filesystem; remote scan holds FTP lock.
    // tokio::join! runs both futures on the same task but interleaves their I/O waits.
    let local_future = get_local_files_recursive_with_progress(
        &local_path,
        &local_path,
        &options.exclude_patterns,
        options.compare_checksum,
        Some(&state.cancel_flag),
        Some(&app),
    );

    let remote_future = async {
        let mut ftp_manager = state.ftp_manager.lock().await;
        get_remote_files_recursive_with_progress(
            &app,
            &mut ftp_manager,
            &remote_path,
            &remote_path,
            &options.exclude_patterns,
            0,
            Some(&state.cancel_flag),
        )
        .await
    };

    let (local_result, remote_result) = tokio::join!(local_future, remote_future);
    let local_files = local_result.map_err(|e| format!("Failed to scan local directory: {}", e))?;
    let remote_files =
        remote_result.map_err(|e| format!("Failed to scan remote directory: {}", e))?;

    // Emit scan phase: comparing
    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "comparing",
            "files_found": local_files.len() + remote_files.len(),
        }),
    );

    // Load sync index if available for conflict detection
    let index = load_sync_index(&local_path, &remote_path).ok().flatten();
    let results =
        build_comparison_results_with_index(local_files, remote_files, &options, index.as_ref());

    info!(
        "Comparison complete: {} differences found (index: {})",
        results.len(),
        if index.is_some() { "used" } else { "none" }
    );

    Ok(results)
}

/// GAP-10: recursive comparison of two local directories.
///
/// The AeroSync modal opened against a dual-local AeroFile pair used to run a
/// flat, top-level classify, so a Mirror / Backup preset only ever acted on
/// the first directory level. This command scans both trees with the same
/// `get_local_files_recursive_with_progress` walker that `compare_directories`
/// uses for the local side, then reuses `build_comparison_results_with_index`
/// so the unified Compare / Plan tabs and the runner operate on every nested
/// level. The `left` directory maps onto `local_info`, `right` onto
/// `remote_info`; the frontend adapts the result with `leftIsLocal = true`.
#[tauri::command]
async fn compare_local_directories(
    app: AppHandle,
    state: State<'_, AppState>,
    left_path: String,
    right_path: String,
    options: Option<CompareOptions>,
) -> Result<Vec<FileComparison>, String> {
    let mut options = options.unwrap_or_default();
    sync::apply_error_correction_excludes(&mut options);

    validate_path(&left_path)?;
    validate_path(&right_path)?;

    info!(
        "Comparing local directories: left={}, right={}",
        left_path, right_path
    );

    // Take ownership of the shared cancel flag for the duration of this scan,
    // matching `compare_directories`.
    state.cancel_flag.store(false, Ordering::Relaxed);

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({ "phase": "local", "files_found": 0 }),
    );

    // Both sides are filesystem scans; run them concurrently.
    let left_future = get_local_files_recursive_with_progress(
        &left_path,
        &left_path,
        &options.exclude_patterns,
        options.compare_checksum,
        Some(&state.cancel_flag),
        Some(&app),
    );
    let right_future = get_local_files_recursive_with_progress(
        &right_path,
        &right_path,
        &options.exclude_patterns,
        options.compare_checksum,
        Some(&state.cancel_flag),
        Some(&app),
    );

    let (left_result, right_result) = tokio::join!(left_future, right_future);
    let left_files = left_result.map_err(|e| format!("Failed to scan left directory: {}", e))?;
    let right_files = right_result.map_err(|e| format!("Failed to scan right directory: {}", e))?;

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "comparing",
            "files_found": left_files.len() + right_files.len(),
        }),
    );

    // The sync index is keyed by the (left, right) path pair, so previous
    // local-local runs feed conflict detection just like the remote case.
    let index = load_sync_index(&left_path, &right_path).ok().flatten();
    let results =
        build_comparison_results_with_index(left_files, right_files, &options, index.as_ref());

    info!(
        "Local comparison complete: {} differences found (index: {})",
        results.len(),
        if index.is_some() { "used" } else { "none" }
    );

    Ok(results)
}

/// Compute SHA-256 hash of a local file (streaming, 64KB chunks)
async fn compute_sha256(path: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65_536];
    loop {
        let n = file.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Scan local directory iteratively and build file info map.
/// When `compare_checksum` is true, computes SHA-256 for each file.
pub async fn get_local_files_recursive(
    base_path: &str,
    _current_path: &str,
    exclude_patterns: &[String],
    compare_checksum: bool,
    cancel_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<HashMap<String, FileInfo>, String> {
    get_local_files_recursive_with_progress(
        base_path,
        _current_path,
        exclude_patterns,
        compare_checksum,
        cancel_flag,
        None,
    )
    .await
}

/// Same as get_local_files_recursive, but emits `sync_scan_progress` events
/// while traversing. Without this, scanning large trees (e.g. a home directory)
/// leaves the UI stuck on "0 files found" for minutes, which looks like a stall.
pub async fn get_local_files_recursive_with_progress(
    base_path: &str,
    _current_path: &str,
    exclude_patterns: &[String],
    compare_checksum: bool,
    cancel_flag: Option<&std::sync::atomic::AtomicBool>,
    app: Option<&AppHandle>,
) -> Result<HashMap<String, FileInfo>, String> {
    let mut files = HashMap::new();
    let base = PathBuf::from(base_path);

    if !base.exists() {
        return Ok(files);
    }

    // Use a stack for iterative traversal instead of recursion
    let mut dirs_to_process = vec![base.clone()];

    // Throttle progress emission: every 500 files OR every 200ms, whichever comes first
    let mut last_progress_emit = std::time::Instant::now();
    let mut last_progress_count: usize = 0;

    while let Some(current_dir) = dirs_to_process.pop() {
        // Check cancellation
        if let Some(flag) = cancel_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(files); // Return partial results
            }
        }

        // Emit scan progress between directories (throttled)
        if let Some(handle) = app {
            let now = std::time::Instant::now();
            let count = files.len();
            if count.saturating_sub(last_progress_count) >= 500
                || now.duration_since(last_progress_emit).as_millis() >= 200
            {
                let _ = handle.emit(
                    "sync_scan_progress",
                    serde_json::json!({
                        "phase": "local",
                        "files_found": count,
                    }),
                );
                last_progress_emit = now;
                last_progress_count = count;
            }
        }

        let mut entries = match tokio::fs::read_dir(&current_dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Calculate relative path
            let relative_path = path
                .strip_prefix(&base)
                .map(|p| p.to_string_lossy().to_string().replace('\\', "/"))
                .unwrap_or_else(|_| name.clone());

            // Skip excluded paths
            if should_exclude(&relative_path, exclude_patterns) {
                continue;
            }

            // H22: Use symlink_metadata to avoid following symlinks outside sync root.
            // This returns metadata about the symlink itself, not its target.
            let metadata = tokio::fs::symlink_metadata(&path).await.ok();

            // Skip symlinks entirely to prevent data exfiltration via malicious symlinks
            if metadata
                .as_ref()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                continue;
            }

            let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);

            let modified = metadata.as_ref().and_then(|m| {
                m.modified().ok().map(|t| {
                    let datetime: chrono::DateTime<chrono::Utc> = t.into();
                    datetime
                })
            });

            let size = if is_dir {
                0
            } else {
                metadata.as_ref().map(|m| m.len()).unwrap_or(0)
            };

            // Compute SHA-256 checksum if requested (only for files, not directories)
            let checksum = if compare_checksum && !is_dir {
                compute_sha256(&path).await
            } else {
                None
            };

            let file_info = FileInfo {
                name: name.clone(),
                path: path.to_string_lossy().to_string(),
                size,
                modified,
                is_dir,
                checksum_alg: checksum.as_ref().map(|_| "sha256".to_string()),
                checksum,
            };

            // P2-1: Cap file index at 1M entries to prevent unbounded memory growth
            if files.len() >= 1_000_000 {
                return Err(
                    "File scan exceeded 1,000,000 entries. Consider narrowing the scan scope."
                        .to_string(),
                );
            }

            files.insert(relative_path, file_info);

            // Add subdirectories to process
            if is_dir {
                dirs_to_process.push(path);
            }
        }
    }

    Ok(files)
}

/// Parallel local scan: directory traversal is sequential (fast), but SHA-256
/// checksums are computed concurrently using a bounded JoinSet + Semaphore.
/// Falls back to sequential scan when `compare_checksum` is false (no I/O benefit).
pub async fn get_local_files_recursive_parallel(
    base_path: &str,
    exclude_patterns: &[String],
    compare_checksum: bool,
    max_concurrent_hashes: usize,
    cancel_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<HashMap<String, FileInfo>, String> {
    let base = PathBuf::from(base_path);
    if !base.exists() {
        return Ok(HashMap::new());
    }

    // Phase 1: Walk the directory tree (sequential: fast, mostly metadata)
    #[allow(clippy::type_complexity)]
    let mut file_entries: Vec<(
        String,
        String,
        u64,
        Option<chrono::DateTime<chrono::Utc>>,
        bool,
    )> = Vec::new();
    let mut dirs_to_process = vec![base.clone()];

    while let Some(current_dir) = dirs_to_process.pop() {
        if let Some(flag) = cancel_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
        }
        let mut entries = match tokio::fs::read_dir(&current_dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            let relative_path = path
                .strip_prefix(&base)
                .map(|p| p.to_string_lossy().to_string().replace('\\', "/"))
                .unwrap_or_else(|_| name.clone());

            if should_exclude(&relative_path, exclude_patterns) {
                continue;
            }

            // H22: Use symlink_metadata to avoid following symlinks outside sync root.
            let metadata = tokio::fs::symlink_metadata(&path).await.ok();

            // Skip symlinks entirely to prevent data exfiltration via malicious symlinks
            if metadata
                .as_ref()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                continue;
            }

            let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let modified = metadata.as_ref().and_then(|m| {
                m.modified().ok().map(|t| {
                    let datetime: chrono::DateTime<chrono::Utc> = t.into();
                    datetime
                })
            });
            let size = if is_dir {
                0
            } else {
                metadata.as_ref().map(|m| m.len()).unwrap_or(0)
            };
            let abs_path = path.to_string_lossy().to_string();

            // P2-1: Cap file index at 1M entries to prevent unbounded memory growth
            if file_entries.len() >= 1_000_000 {
                return Err(
                    "File scan exceeded 1,000,000 entries. Consider narrowing the scan scope."
                        .to_string(),
                );
            }

            file_entries.push((relative_path, abs_path, size, modified, is_dir));

            if is_dir {
                dirs_to_process.push(path);
            }
        }
    }

    // Phase 2: Compute checksums in parallel (only when requested)
    let mut files = HashMap::with_capacity(file_entries.len());

    if compare_checksum {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_hashes.clamp(1, 16),
        ));
        let mut join_set = tokio::task::JoinSet::new();

        for (relative_path, abs_path, size, modified, is_dir) in file_entries {
            if is_dir {
                files.insert(
                    relative_path,
                    FileInfo {
                        name: std::path::Path::new(&abs_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        path: abs_path,
                        size,
                        modified,
                        is_dir: true,
                        checksum_alg: None,
                        checksum: None,
                    },
                );
                continue;
            }

            let sem = semaphore.clone();
            let path_clone = abs_path.clone();
            let rel_clone = relative_path.clone();

            join_set.spawn(async move {
                let _permit = sem.acquire().await;
                let checksum = compute_sha256(std::path::Path::new(&path_clone)).await;
                (rel_clone, path_clone, size, modified, checksum)
            });
        }

        while let Some(result) = join_set.join_next().await {
            if let Ok((rel_path, abs_path, size, modified, checksum)) = result {
                let name = std::path::Path::new(&abs_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                files.insert(
                    rel_path,
                    FileInfo {
                        name,
                        path: abs_path,
                        size,
                        modified,
                        is_dir: false,
                        checksum_alg: checksum.as_ref().map(|_| "sha256".to_string()),
                        checksum,
                    },
                );
            }
        }
    } else {
        // No checksums: just convert entries to FileInfo directly
        for (relative_path, abs_path, size, modified, is_dir) in file_entries {
            let name = std::path::Path::new(&abs_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            files.insert(
                relative_path,
                FileInfo {
                    name,
                    path: abs_path,
                    size,
                    modified,
                    is_dir,
                    checksum_alg: None,
                    checksum: None,
                },
            );
        }
    }

    Ok(files)
}

/// Scan remote directory with progress events
async fn get_remote_files_recursive_with_progress(
    app: &AppHandle,
    ftp_manager: &mut ftp::FtpManager,
    base_path: &str,
    _current_path: &str,
    exclude_patterns: &[String],
    local_count: usize,
    cancel_flag: Option<&std::sync::atomic::AtomicBool>,
) -> Result<HashMap<String, FileInfo>, String> {
    let mut files = HashMap::new();
    // (absolute_path, depth): depth limit prevents infinite loops on servers
    // that list the current directory itself as a child entry.
    let mut dirs_to_process: Vec<(String, u32)> = vec![(base_path.to_string(), 0)];
    let mut visited = std::collections::HashSet::new();
    visited.insert(base_path.to_string());
    const MAX_DEPTH: u32 = 64;

    while let Some((current_dir, depth)) = dirs_to_process.pop() {
        if depth > MAX_DEPTH {
            info!("Remote scan depth limit reached at {}", current_dir);
            continue;
        }

        // Check cancellation flag: release FTP lock immediately on cancel
        if let Some(flag) = cancel_flag {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                info!("Remote scan cancelled by user after {} files", files.len());
                return Ok(files); // Return partial results (will be discarded by frontend)
            }
        }
        if let Err(e) = ftp_manager.change_dir(&current_dir).await {
            info!(
                "Warning: Could not change to directory {}: {}",
                current_dir, e
            );
            continue;
        }

        let entries = match ftp_manager.list_files().await {
            Ok(e) => e,
            Err(e) => {
                info!("Warning: Could not list files in {}: {}", current_dir, e);
                continue;
            }
        };

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let relative_path = if current_dir == base_path {
                entry.name.clone()
            } else {
                let rel_dir = current_dir.strip_prefix(base_path).unwrap_or(&current_dir);
                let rel_dir = rel_dir.trim_start_matches('/');
                if rel_dir.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", rel_dir, entry.name)
                }
            };

            if should_exclude(&relative_path, exclude_patterns) {
                continue;
            }

            let file_info = FileInfo {
                name: entry.name.clone(),
                path: format!("{}/{}", current_dir, entry.name),
                size: entry.size.unwrap_or(0),
                modified: entry.modified.and_then(|s| {
                    let clean = s.strip_suffix('Z').unwrap_or(&s);
                    chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%d %H:%M")
                        .or_else(|_| {
                            chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%d %H:%M:%S")
                        })
                        .ok()
                        .map(|dt| {
                            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                                dt,
                                chrono::Utc,
                            )
                        })
                }),
                is_dir: entry.is_dir,
                checksum_alg: None,
                checksum: None,
            };

            files.insert(relative_path, file_info);

            if entry.is_dir {
                let child_path = format!("{}/{}", current_dir, entry.name);
                if visited.insert(child_path.clone()) {
                    dirs_to_process.push((child_path, depth + 1));
                } else {
                    info!("Skipping already-visited directory: {}", child_path);
                }
            }
        }

        // Emit progress after each directory listing
        let _ = app.emit(
            "sync_scan_progress",
            serde_json::json!({
                "phase": "remote",
                "files_found": local_count + files.len(),
            }),
        );
    }

    let _ = ftp_manager.change_dir(base_path).await;
    Ok(files)
}

#[tauri::command]
fn get_compare_options_default() -> CompareOptions {
    CompareOptions::default()
}

#[tauri::command]
fn load_sync_index_cmd(
    local_path: String,
    remote_path: String,
) -> Result<Option<SyncIndex>, String> {
    validate_path(&local_path)?;
    validate_path(&remote_path)?;
    load_sync_index(&local_path, &remote_path)
}

#[tauri::command]
fn save_sync_index_cmd(index: SyncIndex) -> Result<(), String> {
    validate_path(&index.local_path)?;
    validate_path(&index.remote_path)?;
    save_sync_index(&index)
}

// ============ Sync Journal Commands (Phase 2: Reliability) ============

#[tauri::command]
fn load_sync_journal_cmd(
    local_path: String,
    remote_path: String,
) -> Result<Option<SyncJournal>, String> {
    validate_path(&local_path)?;
    validate_path(&remote_path)?;
    load_sync_journal(&local_path, &remote_path)
}

#[tauri::command]
fn save_sync_journal_cmd(journal: SyncJournal) -> Result<(), String> {
    validate_path(&journal.local_path)?;
    validate_path(&journal.remote_path)?;
    save_sync_journal(&journal)
}

#[tauri::command]
fn delete_sync_journal_cmd(local_path: String, remote_path: String) -> Result<(), String> {
    validate_path(&local_path)?;
    validate_path(&remote_path)?;
    delete_sync_journal(&local_path, &remote_path)
}

#[tauri::command]
fn list_sync_journals_cmd() -> Result<Vec<sync::JournalSummary>, String> {
    sync::list_sync_journals()
}

#[tauri::command]
fn cleanup_old_journals_cmd(max_age_days: u32) -> Result<u32, String> {
    sync::cleanup_old_journals(max_age_days)
}

#[tauri::command]
fn clear_all_journals_cmd() -> Result<u32, String> {
    sync::clear_all_journals()
}

#[tauri::command]
fn load_sync_profiles_cmd() -> Result<Vec<sync::SyncProfile>, String> {
    sync::load_sync_profiles()
}

#[tauri::command]
fn save_sync_profile_cmd(profile: sync::SyncProfile) -> Result<(), String> {
    sync::save_sync_profile(&profile)
}

#[tauri::command]
fn delete_sync_profile_cmd(id: String) -> Result<(), String> {
    sync::delete_sync_profile(&id)
}

// ─── Phase 3A+ Commands: Parallel Scan, Scheduler, Watcher ─────────────

#[tauri::command]
async fn get_parallel_scan_files(
    base_path: String,
    exclude_patterns: Vec<String>,
    compare_checksum: bool,
    max_concurrent_hashes: Option<usize>,
) -> Result<HashMap<String, FileInfo>, String> {
    validate_path(&base_path)?;
    let concurrency = max_concurrent_hashes.unwrap_or(4);
    get_local_files_recursive_parallel(
        &base_path,
        &exclude_patterns,
        compare_checksum,
        concurrency,
        None,
    )
    .await
}

#[tauri::command]
fn get_sync_schedule_cmd() -> Result<sync_scheduler::SyncSchedule, String> {
    Ok(sync_scheduler::load_sync_schedule())
}

#[tauri::command]
fn save_sync_schedule_cmd(schedule: sync_scheduler::SyncSchedule) -> Result<(), String> {
    sync_scheduler::save_sync_schedule(&schedule)
}

#[tauri::command]
fn get_watcher_status_cmd(watch_path: Option<String>) -> Result<serde_json::Value, String> {
    // Validate the watch path if provided
    if let Some(ref p) = watch_path {
        filesystem::validate_path(p)?;
    }

    // Returns a snapshot of the filesystem watcher status
    // Watcher lifecycle is managed by background_sync_worker, not directly from frontend
    let native_backend = if cfg!(target_os = "linux") {
        "inotify"
    } else if cfg!(target_os = "macos") {
        "fsevent"
    } else if cfg!(target_os = "windows") {
        "readirectorychanges"
    } else {
        "poll"
    };

    let inotify_info = if cfg!(target_os = "linux") {
        watch_path.as_ref().map(|p| {
            let (count, should_warn, should_fallback) =
                file_watcher::check_inotify_capacity(std::path::Path::new(p));
            serde_json::json!({
                "subdirectory_count": count,
                "should_warn": should_warn,
                "should_fallback_to_poll": should_fallback,
            })
        })
    } else {
        None
    };

    Ok(serde_json::json!({
        "available": true,
        "native_backend": native_backend,
        "inotify_capacity": inotify_info,
    }))
}

/// Get transfer optimization hints for the current cloud provider
fn default_transfer_optimization_hints(
    provider_type: &str,
) -> providers::TransferOptimizationHints {
    match provider_type {
        "sftp" => providers::TransferOptimizationHints {
            supports_range_download: true,
            supports_compression: true,
            supports_delta_sync: true,
            ..Default::default()
        },
        "s3" => providers::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: 5 * 1024 * 1024,
            multipart_part_size: 5 * 1024 * 1024,
            multipart_max_parallel: 4,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("ETag".to_string()),
            ..Default::default()
        },
        "ftp" | "ftps" => providers::TransferOptimizationHints {
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        },
        "webdav" => providers::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        },
        _ => providers::TransferOptimizationHints::default(),
    }
}

fn native_rsync_runtime_enabled() -> bool {
    if !crate::settings::native_rsync_feature_compiled() {
        return false;
    }

    #[cfg(feature = "aerorsync")]
    {
        crate::settings::load_native_rsync_enabled()
    }

    #[cfg(not(feature = "aerorsync"))]
    {
        false
    }
}

#[derive(Serialize, Clone)]
struct DeltaServerIdentity {
    protocol: String,
    host: String,
    port: u16,
    username: String,
}

#[derive(Serialize, Clone)]
struct DeltaEligibilityProbeResult {
    eligible: bool,
    reason: Option<String>,
    server_identity: Option<DeltaServerIdentity>,
}

#[tauri::command]
async fn get_transfer_optimization_hints(
    state: State<'_, provider_commands::ProviderState>,
    provider_type: Option<String>,
) -> Result<providers::TransferOptimizationHints, String> {
    let requested = provider_type.unwrap_or_default().to_lowercase();
    let active_protocol = {
        let provider_lock = state.provider.lock().await;
        provider_lock
            .as_ref()
            .map(|provider| format!("{:?}", provider.provider_type()).to_lowercase())
    };

    let mut hints = if let Some(active) = active_protocol.as_deref() {
        if requested.is_empty() || requested == active {
            let provider_lock = state.provider.lock().await;
            provider_lock
                .as_ref()
                .map(|provider| provider.transfer_optimization_hints())
                .unwrap_or_else(|| default_transfer_optimization_hints(active))
        } else {
            default_transfer_optimization_hints(&requested)
        }
    } else {
        default_transfer_optimization_hints(&requested)
    };

    let inspect_sftp =
        requested == "sftp" || (requested.is_empty() && active_protocol.as_deref() == Some("sftp"));
    if inspect_sftp {
        let active_session_is_sftp = {
            let config_lock = state.config.lock().await;
            config_lock
                .as_ref()
                .map(|config| config.provider_type == providers::ProviderType::Sftp)
                .unwrap_or(false)
        };

        // PR-T11 F8 fix. Previous implementation read
        // `config.extra.get("private_key_path")` to decide whether a
        // key-based SSH session was in place. That split produced two
        // divergent codepaths for the same semantic question:
        //
        //   - `get_transfer_optimization_hints`: hints/badges/checkbox
        //   - `sftp_probe_delta_eligibility`: eligibility gate modal
        //
        // On Windows the extra-map was not reliably populated even
        // when the live `SftpProvider` instance held a valid
        // `private_key_path`, so the modal's probe could say "eligible"
        // while the hints said "not active": the AeroSync Delta Sync
        // checkbox stayed greyed out even when the user had toggled
        // native rsync ON in Settings and held an SSH key profile.
        //
        // Single source of truth is the provider instance's own
        // `delta_transport()` factory: if it returns Some(...), the
        // SFTP session can actually drive a DeltaTransport right now;
        // if it returns None, it cannot. The hints now mirror exactly
        // what the runtime dispatch will do.
        let provider_can_deliver_delta = {
            let mut provider_lock = state.provider.lock().await;
            provider_lock
                .as_mut()
                .and_then(|provider| {
                    provider
                        .as_any_mut()
                        .downcast_mut::<providers::sftp::SftpProvider>()
                })
                .map(|sftp| sftp.delta_transport().is_some())
                .unwrap_or(false)
        };

        let native_feature_compiled = crate::settings::native_rsync_feature_compiled();
        let native_feature_enabled = native_rsync_runtime_enabled();
        let private_key_configured = provider_can_deliver_delta;
        let delta_eligible =
            active_session_is_sftp && provider_can_deliver_delta && native_feature_enabled;

        hints.supports_resume_download = false;
        hints.supports_resume_upload = false;
        hints.supports_range_download = true;
        hints.supports_compression = true;
        hints.supports_delta_sync = true;
        hints.delta_sync_eligible = delta_eligible;
        hints.delta_sync_active = delta_eligible;
        hints.delta_sync_note = Some(if !active_session_is_sftp {
            "Connect an SFTP session to evaluate delta eligibility.".to_string()
        } else if !native_feature_compiled {
            "This build was compiled without native rsync support.".to_string()
        } else if !native_feature_enabled {
            "Enable Native Rsync in Settings to make SFTP delta eligible.".to_string()
        } else if !private_key_configured {
            "Requires an SSH key-based SFTP session; password auth stays on the classic path."
                .to_string()
        } else {
            "Session is ready for Delta Sync.".to_string()
        });
    }

    Ok(hints)
}

#[tauri::command]
async fn get_transfer_capabilities(
    state: State<'_, provider_commands::ProviderState>,
    provider_type: Option<String>,
) -> Result<transfer_dag::TransferCapabilities, String> {
    let requested = provider_type.unwrap_or_default().to_lowercase();
    let active_protocol = {
        let provider_lock = state.provider.lock().await;
        provider_lock
            .as_ref()
            .map(|provider| format!("{:?}", provider.provider_type()).to_lowercase())
    };

    if let Some(active) = active_protocol.as_deref() {
        if requested.is_empty() || requested == active {
            let provider_lock = state.provider.lock().await;
            return Ok(provider_lock
                .as_ref()
                .map(|provider| provider.transfer_capabilities())
                .unwrap_or_else(|| {
                    transfer_dag::TransferCapabilities::from_provider_hints(
                        provider_type_from_string(active).unwrap_or(providers::ProviderType::Ftp),
                        &default_transfer_optimization_hints(active),
                        false,
                    )
                }));
        }
    }

    let provider_type = provider_type_from_string(&requested).ok_or_else(|| {
        if requested.is_empty() {
            "No active provider is connected".to_string()
        } else {
            format!("Unknown provider type: {}", requested)
        }
    })?;

    Ok(transfer_dag::TransferCapabilities::from_provider_hints(
        provider_type,
        &default_transfer_optimization_hints(&requested),
        false,
    ))
}

fn provider_type_from_string(value: &str) -> Option<providers::ProviderType> {
    match value {
        "ftp" => Some(providers::ProviderType::Ftp),
        "ftps" => Some(providers::ProviderType::Ftps),
        "sftp" => Some(providers::ProviderType::Sftp),
        "webdav" | "web_dav" => Some(providers::ProviderType::WebDav),
        "s3" => Some(providers::ProviderType::S3),
        "aerocloud" | "aero_cloud" => Some(providers::ProviderType::AeroCloud),
        "googledrive" | "google_drive" | "google drive" => {
            Some(providers::ProviderType::GoogleDrive)
        }
        "dropbox" => Some(providers::ProviderType::Dropbox),
        "onedrive" | "one_drive" | "one drive" => Some(providers::ProviderType::OneDrive),
        "mega" => Some(providers::ProviderType::Mega),
        "box" => Some(providers::ProviderType::Box),
        "pcloud" | "p_cloud" => Some(providers::ProviderType::PCloud),
        "azure" => Some(providers::ProviderType::Azure),
        "filen" => Some(providers::ProviderType::Filen),
        "fourshared" | "four_shared" | "4shared" => Some(providers::ProviderType::FourShared),
        "zohoworkdrive" | "zoho_workdrive" | "zoho workdrive" => {
            Some(providers::ProviderType::ZohoWorkdrive)
        }
        "internxt" => Some(providers::ProviderType::Internxt),
        "kdrive" | "k_drive" => Some(providers::ProviderType::KDrive),
        "jottacloud" => Some(providers::ProviderType::Jottacloud),
        "drimecloud" | "drime_cloud" => Some(providers::ProviderType::DrimeCloud),
        "filelu" | "file_lu" => Some(providers::ProviderType::FileLu),
        "koofr" => Some(providers::ProviderType::Koofr),
        "opendrive" | "open_drive" => Some(providers::ProviderType::OpenDrive),
        "yandexdisk" | "yandex_disk" | "yandex disk" => Some(providers::ProviderType::YandexDisk),
        "github" => Some(providers::ProviderType::GitHub),
        "gitlab" => Some(providers::ProviderType::GitLab),
        "swift" => Some(providers::ProviderType::Swift),
        "googlephotos" | "google_photos" | "google photos" => {
            Some(providers::ProviderType::GooglePhotos)
        }
        "immich" => Some(providers::ProviderType::Immich),
        "imagekit" | "image_kit" => Some(providers::ProviderType::ImageKit),
        "uploadcare" => Some(providers::ProviderType::Uploadcare),
        "backblaze" | "b2" | "backblazeb2" | "backblaze_b2" => {
            Some(providers::ProviderType::Backblaze)
        }
        "cloudinary" => Some(providers::ProviderType::Cloudinary),
        _ => None,
    }
}

#[tauri::command]
async fn sftp_probe_delta_eligibility(
    provider_state: State<'_, provider_commands::ProviderState>,
) -> Result<DeltaEligibilityProbeResult, String> {
    #[cfg_attr(not(unix), allow(unused_variables))]
    let (active_session_is_sftp, private_key_configured, server_identity) = {
        let config_lock = provider_state.config.lock().await;
        let config = config_lock.as_ref();
        let active_session_is_sftp = config
            .map(|cfg| cfg.provider_type == providers::ProviderType::Sftp)
            .unwrap_or(false);
        let private_key_configured = config
            .and_then(|cfg| cfg.extra.get("private_key_path"))
            .map(|path| !path.trim().is_empty())
            .unwrap_or(false);
        let server_identity = config.and_then(|cfg| {
            (cfg.provider_type == providers::ProviderType::Sftp).then(|| DeltaServerIdentity {
                protocol: "sftp".to_string(),
                host: cfg.host.clone(),
                port: cfg.effective_port(),
                username: cfg.username.clone().unwrap_or_default(),
            })
        });
        (
            active_session_is_sftp,
            private_key_configured,
            server_identity,
        )
    };

    if !active_session_is_sftp {
        return Ok(DeltaEligibilityProbeResult {
            eligible: false,
            reason: Some("Connect an SFTP session to evaluate delta eligibility.".to_string()),
            server_identity,
        });
    }

    #[cfg(not(unix))]
    {
        Ok(DeltaEligibilityProbeResult {
            eligible: false,
            reason: Some(
                "Native delta sync runtime is currently only enabled on Unix builds (Windows support pending Z.4.3.f6 fix).".to_string(),
            ),
            server_identity,
        })
    }

    #[cfg(unix)]
    {
        let native_feature_compiled = crate::settings::native_rsync_feature_compiled();
        if !native_feature_compiled {
            return Ok(DeltaEligibilityProbeResult {
                eligible: false,
                reason: Some("This build was compiled without native rsync support.".to_string()),
                server_identity,
            });
        }

        if !native_rsync_runtime_enabled() {
            return Ok(DeltaEligibilityProbeResult {
                eligible: false,
                reason: Some(
                    "Enable Native Rsync in Settings to make SFTP delta eligible.".to_string(),
                ),
                server_identity,
            });
        }

        if !private_key_configured {
            return Ok(DeltaEligibilityProbeResult {
                eligible: false,
                reason: Some(
                    "Requires an SSH key-based SFTP session; password auth stays on the classic path."
                        .to_string(),
                ),
                server_identity,
            });
        }

        let mut provider_lock = provider_state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;

        let verdict = crate::delta_sync_rsync::check_delta_eligibility(provider.as_mut())
            .await
            .unwrap_or(crate::delta_sync_rsync::DeltaEligibilityStatus {
                eligible: false,
                reason: Some("Reconnect the SFTP session to evaluate delta sync.".to_string()),
            });

        Ok(DeltaEligibilityProbeResult {
            eligible: verdict.eligible,
            reason: verdict.reason,
            server_identity,
        })
    }
}

// =============================
// Multi-Path Sync Commands (#52)
// =============================

#[tauri::command]
fn get_multi_path_config() -> sync::MultiPathConfig {
    sync::load_multi_path_config()
}

#[tauri::command]
fn save_multi_path_config_cmd(config: sync::MultiPathConfig) -> Result<(), String> {
    sync::save_multi_path_config(&config)
}

#[tauri::command]
fn add_path_pair(pair: sync::PathPair) -> Result<sync::MultiPathConfig, String> {
    let mut config = sync::load_multi_path_config();
    config.pairs.push(pair);
    sync::save_multi_path_config(&config)?;
    Ok(config)
}

#[tauri::command]
fn remove_path_pair(pair_id: String) -> Result<sync::MultiPathConfig, String> {
    let mut config = sync::load_multi_path_config();
    config.pairs.retain(|p| p.id != pair_id);
    sync::save_multi_path_config(&config)?;
    Ok(config)
}

// =============================
// Sync Template Commands (#153)
// =============================

#[tauri::command]
fn export_sync_template_cmd(
    name: String,
    description: String,
    profile_id: String,
    local_path: String,
    remote_path: String,
    exclude_patterns: Vec<String>,
) -> Result<String, String> {
    let profiles = sync::load_sync_profiles()?;
    let profile = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .ok_or_else(|| format!("Profile '{}' not found", profile_id))?;
    let schedule = sync_scheduler::load_sync_schedule();
    let schedule_opt = if schedule.enabled {
        Some(&schedule)
    } else {
        None
    };
    let template = sync::export_sync_template(
        &name,
        &description,
        profile,
        &local_path,
        &remote_path,
        &exclude_patterns,
        schedule_opt,
    )?;
    serde_json::to_string_pretty(&template).map_err(|e| e.to_string())
}

#[tauri::command]
fn import_sync_template_cmd(json_content: String) -> Result<sync::SyncTemplate, String> {
    let template: sync::SyncTemplate = serde_json::from_str(&json_content)
        .map_err(|e| format!("Invalid template format: {}", e))?;
    if template.schema_version != 1 {
        return Err(format!(
            "Unsupported template version: {}",
            template.schema_version
        ));
    }
    Ok(template)
}

#[derive(serde::Deserialize)]
struct SyncScriptExportArgs {
    profile_id: String,
    profile_display_name: String,
    template_name: String,
    template_description: String,
    local_path: String,
    remote_path: String,
    exclude_patterns: Vec<String>,
    format: String,
}

#[tauri::command]
fn export_sync_script_cmd(args: SyncScriptExportArgs) -> Result<String, String> {
    let format = sync::SyncScriptFormat::parse(&args.format)
        .ok_or_else(|| format!("Unsupported script format: {}", args.format))?;
    let profiles = sync::load_sync_profiles()?;
    let profile = profiles
        .iter()
        .find(|p| p.id == args.profile_id)
        .ok_or_else(|| format!("Profile '{}' not found", args.profile_id))?;
    sync::export_sync_script(sync::SyncScriptExportOptions {
        profile,
        profile_display_name: &args.profile_display_name,
        template_name: &args.template_name,
        template_description: &args.template_description,
        local_path: &args.local_path,
        remote_path: &args.remote_path,
        exclude_patterns: &args.exclude_patterns,
        format,
    })
}

#[tauri::command]
fn import_sync_script_cmd(script_content: String) -> Result<sync::SyncScriptMeta, String> {
    sync::import_sync_script(&script_content)
}

// =============================
// AeroSync canonical script export/import (issue #133)
// =============================

#[derive(serde::Deserialize)]
struct AerosyncExportScriptArgs {
    profile_id: String,
    profile_display_name: Option<String>,
    local_path: String,
    remote_path: String,
    connect_profile: Option<String>,
    exclude_patterns_override: Option<Vec<String>>,
    #[serde(default)]
    dry_run: bool,
    conflict_mode: Option<String>,
    #[serde(default)]
    track_renames: bool,
    #[serde(default)]
    skip_matching: bool,
    #[serde(default)]
    resync: bool,
    #[serde(default)]
    watch: bool,
    output_path: String,
    #[serde(default)]
    also_generate_wrapper: bool,
}

#[derive(serde::Serialize)]
struct AerosyncExportScriptResult {
    canonical_path: String,
    wrapper_path: Option<String>,
}

#[tauri::command]
fn aerosync_export_script_cmd(
    args: AerosyncExportScriptArgs,
) -> Result<AerosyncExportScriptResult, String> {
    let profiles = sync::load_sync_profiles()?;
    let base_profile = profiles
        .iter()
        .find(|p| p.id == args.profile_id)
        .cloned()
        .ok_or_else(|| format!("Profile '{}' not found", args.profile_id))?;

    let mut profile = base_profile;
    if let Some(name) = args.profile_display_name.as_deref() {
        if !name.is_empty() {
            profile.name = name.to_string();
        }
    }
    if let Some(overrides) = args.exclude_patterns_override.clone() {
        profile.exclude_patterns = overrides;
    }

    let canonical_extension = sync_script::CANONICAL_EXTENSION;
    let canonical_path = std::path::PathBuf::from(&args.output_path);
    let canonical_path = if canonical_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case(canonical_extension))
        .unwrap_or(false)
    {
        canonical_path
    } else {
        canonical_path.with_extension(canonical_extension)
    };

    let canonical_filename = canonical_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "Invalid output path: cannot derive filename".to_string())?
        .to_string();

    let script_profile = sync_script::AerosyncScriptProfile {
        profile,
        local_path: args.local_path.clone(),
        remote_path: args.remote_path.clone(),
        connect_profile: args.connect_profile.clone(),
        connect_url: None,
        dry_run: args.dry_run,
        conflict_mode: args.conflict_mode.clone(),
        track_renames: args.track_renames,
        skip_matching: args.skip_matching,
        resync: args.resync,
        watch: args.watch,
    };

    let app_version = env!("CARGO_PKG_VERSION");
    let script = sync_script::generate_script(&script_profile, app_version);

    std::fs::write(&canonical_path, &script)
        .map_err(|e| format!("Failed to write '{}': {}", canonical_path.display(), e))?;

    let mut wrapper_path: Option<String> = None;
    if args.also_generate_wrapper {
        #[cfg(target_os = "windows")]
        let (wrapper_ext, wrapper_body) = ("ps1", sync_script::ps1_wrapper(&canonical_filename));
        #[cfg(not(target_os = "windows"))]
        let (wrapper_ext, wrapper_body) = ("sh", sync_script::sh_wrapper(&canonical_filename));
        let wrapper = canonical_path.with_extension(wrapper_ext);
        std::fs::write(&wrapper, wrapper_body)
            .map_err(|e| format!("Failed to write wrapper '{}': {}", wrapper.display(), e))?;
        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&wrapper) {
                let mut perm = meta.permissions();
                perm.set_mode(0o755);
                let _ = std::fs::set_permissions(&wrapper, perm);
            }
        }
        wrapper_path = Some(wrapper.to_string_lossy().to_string());
    }

    Ok(AerosyncExportScriptResult {
        canonical_path: canonical_path.to_string_lossy().to_string(),
        wrapper_path,
    })
}

#[derive(serde::Deserialize)]
struct AerosyncImportScriptArgs {
    input_path: String,
}

#[derive(serde::Serialize)]
struct AerosyncImportScriptResult {
    profile: sync_script::AerosyncScriptProfile,
    unmapped_fields: Vec<String>,
    warnings: Vec<String>,
    canonical_path: String,
    /// Set when the input was a wrapper (`.ps1` / `.sh`) and the
    /// sibling `.aeroftp-script` was followed.
    resolved_from_wrapper: bool,
}

#[tauri::command]
fn aerosync_import_script_cmd(
    args: AerosyncImportScriptArgs,
) -> Result<AerosyncImportScriptResult, String> {
    let input_path = std::path::PathBuf::from(&args.input_path);
    let raw = std::fs::read_to_string(&input_path)
        .map_err(|e| format!("Cannot read '{}': {}", input_path.display(), e))?;

    let (canonical_path, content, resolved_from_wrapper) =
        match sync_script::detect_wrapper_target(&input_path, &raw) {
            Some(target) => {
                let body = std::fs::read_to_string(&target).map_err(|e| {
                    format!(
                        "Wrapper references '{}' but it could not be read: {}",
                        target.display(),
                        e
                    )
                })?;
                (target, body, true)
            }
            None => (input_path, raw, false),
        };

    let parsed = sync_script::parse_script(&content).map_err(|e| e.user_message())?;

    Ok(AerosyncImportScriptResult {
        profile: parsed.profile,
        unmapped_fields: parsed.unmapped_fields,
        warnings: parsed.warnings,
        canonical_path: canonical_path.to_string_lossy().to_string(),
        resolved_from_wrapper,
    })
}

// =============================
// Rollback Commands (#154)
// =============================

#[tauri::command]
fn create_sync_snapshot_cmd(local_path: String, remote_path: String) -> Result<String, String> {
    let index = sync::load_sync_index(&local_path, &remote_path)?
        .ok_or_else(|| "No sync index found: run sync first".to_string())?;
    let snapshot = sync::create_sync_snapshot(&local_path, &remote_path, &index);
    sync::save_sync_snapshot(&snapshot)?;
    Ok(snapshot.id)
}

#[tauri::command]
fn list_sync_snapshots_cmd(
    local_path: Option<String>,
    remote_path: Option<String>,
) -> Result<Vec<sync::SyncSnapshot>, String> {
    let snapshots = sync::list_sync_snapshots()?;
    Ok(filter_snapshots_for_pair(
        snapshots,
        local_path.as_deref(),
        remote_path.as_deref(),
    ))
}

#[tauri::command]
fn delete_sync_snapshot_cmd(
    snapshot_id: String,
    local_path: Option<String>,
    remote_path: Option<String>,
) -> Result<(), String> {
    let snapshot = sync::load_sync_snapshot(&snapshot_id)?;
    if !snapshot_matches_pair(&snapshot, local_path.as_deref(), remote_path.as_deref()) {
        return Err("Snapshot does not belong to the current sync pair".to_string());
    }
    sync::delete_sync_snapshot(&snapshot_id)
}

#[tauri::command]
fn load_sync_snapshot_cmd(
    snapshot_id: String,
    local_path: Option<String>,
    remote_path: Option<String>,
) -> Result<sync::SyncSnapshot, String> {
    let snapshot = sync::load_sync_snapshot(&snapshot_id)?;
    if !snapshot_matches_pair(&snapshot, local_path.as_deref(), remote_path.as_deref()) {
        return Err("Snapshot does not belong to the current sync pair".to_string());
    }
    Ok(snapshot)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RestoreSnapshotResult {
    restored_from_remote: u32,
    restored_to_remote: u32,
    skipped: u32,
    failed: Vec<String>,
}

fn normalize_sync_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if normalized == "/" {
        normalized
    } else {
        normalized.trim_end_matches('/').to_string()
    }
}

fn snapshot_matches_pair(
    snapshot: &sync::SyncSnapshot,
    local_path: Option<&str>,
    remote_path: Option<&str>,
) -> bool {
    let local_ok = local_path
        .map(|p| normalize_sync_path(&snapshot.local_path) == normalize_sync_path(p))
        .unwrap_or(true);
    let remote_ok = remote_path
        .map(|p| normalize_sync_path(&snapshot.remote_path) == normalize_sync_path(p))
        .unwrap_or(true);
    local_ok && remote_ok
}

fn filter_snapshots_for_pair(
    snapshots: Vec<sync::SyncSnapshot>,
    local_path: Option<&str>,
    remote_path: Option<&str>,
) -> Vec<sync::SyncSnapshot> {
    snapshots
        .into_iter()
        .filter(|snapshot| snapshot_matches_pair(snapshot, local_path, remote_path))
        .collect()
}

fn parse_versioning_strategy(
    versioning_strategy: Option<&str>,
) -> sync_versioning::VersioningStrategy {
    match versioning_strategy.unwrap_or("trash_can") {
        "disabled" => sync_versioning::VersioningStrategy::Disabled,
        "simple" => sync_versioning::VersioningStrategy::Simple { max_copies: 5 },
        "staggered" => sync_versioning::VersioningStrategy::Staggered,
        _ => sync_versioning::VersioningStrategy::TrashCan { max_age_days: 30 },
    }
}

fn local_file_matches_snapshot(
    file_path: &std::path::Path,
    entry: &sync::FileSnapshotEntry,
) -> Result<bool, String> {
    let metadata = match std::fs::metadata(file_path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(format!("Failed to read local metadata: {}", err)),
    };

    if metadata.len() != entry.size {
        return Ok(false);
    }

    let current_modified = metadata
        .modified()
        .map_err(|err| format!("Failed to read local mtime: {}", err))?;
    let current_modified: chrono::DateTime<chrono::Utc> = current_modified.into();
    let expected_modified = match entry.modified.as_ref() {
        Some(modified) => modified,
        None => return Ok(true),
    };

    Ok((current_modified - *expected_modified).num_seconds().abs() <= 2)
}

#[tauri::command]
async fn restore_sync_snapshot_cmd(
    state: State<'_, AppState>,
    provider_state: State<'_, provider_commands::ProviderState>,
    snapshot_id: String,
    local_path: String,
    remote_path: String,
    is_provider: bool,
    versioning_strategy: Option<String>,
) -> Result<RestoreSnapshotResult, String> {
    let snapshot = sync::load_sync_snapshot(&snapshot_id)?;
    if !snapshot_matches_pair(&snapshot, Some(&local_path), Some(&remote_path)) {
        return Err("Snapshot does not belong to the current sync pair".to_string());
    }

    let local_root = std::path::PathBuf::from(&local_path);
    let remote_root = normalize_sync_path(&remote_path);
    let versioning = sync_versioning::SyncVersioning::new(
        &local_root,
        parse_versioning_strategy(versioning_strategy.as_deref()),
    );
    let mut result = RestoreSnapshotResult {
        restored_from_remote: 0,
        restored_to_remote: 0,
        skipped: 0,
        failed: Vec::new(),
    };

    let mut ftp_manager = if is_provider {
        None
    } else {
        Some(state.ftp_manager.lock().await)
    };
    let mut provider_lock = if is_provider {
        Some(provider_state.provider.lock().await)
    } else {
        None
    };

    for (relative_path, entry) in &snapshot.files {
        sync::validate_relative_path(relative_path)?;
        let local_file = local_root.join(relative_path);
        if !local_file.starts_with(&local_root) {
            result
                .failed
                .push(format!("{}: invalid local target path", relative_path));
            continue;
        }
        let remote_file = format!("{}/{}", remote_root, relative_path);
        let local_matches = local_file_matches_snapshot(&local_file, entry)?;

        if local_matches {
            let upload_result = if let Some(ftp) = ftp_manager.as_mut() {
                ftp.upload_file(&local_file.to_string_lossy(), &remote_file)
                    .await
                    .map_err(|err| err.to_string())
            } else if let Some(provider_guard) = provider_lock.as_mut() {
                let provider = provider_guard
                    .as_mut()
                    .ok_or_else(|| "Not connected to any provider".to_string())?;
                provider
                    .upload(&local_file.to_string_lossy(), &remote_file, None)
                    .await
                    .map_err(|err| format!("Upload failed: {}", err))
            } else {
                Err("No active sync backend".to_string())
            };

            match upload_result {
                Ok(_) => {
                    result.restored_to_remote += 1;
                    continue;
                }
                Err(upload_err) => {
                    tracing::warn!(
                        "[restore_sync_snapshot_cmd] Remote restore fallback failed for {}: {}",
                        relative_path,
                        upload_err
                    );
                }
            }
        }

        if local_file.exists() && versioning.is_enabled() {
            versioning.archive(&local_file).map_err(|err| {
                format!(
                    "Failed to archive {} before restore: {}",
                    relative_path, err
                )
            })?;
        }

        let download_result = if let Some(ftp) = ftp_manager.as_mut() {
            ftp.download_file(&remote_file, &local_file.to_string_lossy())
                .await
                .map_err(|err| err.to_string())
        } else if let Some(provider_guard) = provider_lock.as_mut() {
            let provider = provider_guard
                .as_mut()
                .ok_or_else(|| "Not connected to any provider".to_string())?;
            provider
                .download(&remote_file, &local_file.to_string_lossy(), None)
                .await
                .map_err(|err| format!("Download failed: {}", err))
        } else {
            Err("No active sync backend".to_string())
        };

        match download_result {
            Ok(_) => result.restored_from_remote += 1,
            Err(download_err) => {
                result
                    .failed
                    .push(format!("{}: {}", relative_path, download_err));
                result.skipped += 1;
            }
        }
    }

    Ok(result)
}

// =============================
// Rename Detection
// =============================

/// Detect file renames by matching SHA-256 hashes between local_only and remote_only files.
/// Returns pairs of (old_path, new_path, size) that are likely renames rather than delete+create.
#[tauri::command]
async fn detect_renames_cmd(
    local_path: String,
    comparisons: Vec<sync::FileComparison>,
) -> Result<Vec<serde_json::Value>, String> {
    use std::collections::HashMap;

    // Separate candidates: local_only = potential new files, remote_only = potential deleted files
    let local_only: Vec<&sync::FileComparison> = comparisons
        .iter()
        .filter(|c| c.status == sync::SyncStatus::LocalOnly && !c.is_dir)
        .collect();
    let remote_only: Vec<&sync::FileComparison> = comparisons
        .iter()
        .filter(|c| c.status == sync::SyncStatus::RemoteOnly && !c.is_dir)
        .collect();

    if local_only.is_empty() || remote_only.is_empty() {
        return Ok(Vec::new());
    }

    // Hash local_only files
    let mut local_hashes: HashMap<String, Vec<&sync::FileComparison>> = HashMap::new();
    for comp in &local_only {
        let file = std::path::PathBuf::from(&local_path).join(&comp.relative_path);
        if let Ok(hash) = sha256_file_hex(&file) {
            local_hashes.entry(hash).or_default().push(comp);
        }
    }

    // Match remote_only files by size against local hashes
    // (We can't hash remote files, so we match by size first, then confirm by local hash)
    let mut renames = Vec::new();
    let mut used_locals: std::collections::HashSet<String> = std::collections::HashSet::new();

    for remote_comp in &remote_only {
        let remote_size = remote_comp
            .remote_info
            .as_ref()
            .map(|i| i.size)
            .unwrap_or(0);
        // Find a local_only file with matching hash and similar size
        for (hash, locals) in &local_hashes {
            for local_comp in locals {
                if used_locals.contains(&local_comp.relative_path) {
                    continue;
                }
                let local_size = local_comp.local_info.as_ref().map(|i| i.size).unwrap_or(0);
                if local_size == remote_size {
                    renames.push(serde_json::json!({
                        "old_path": remote_comp.relative_path,
                        "new_path": local_comp.relative_path,
                        "size": local_size,
                        "hash": hash,
                    }));
                    used_locals.insert(local_comp.relative_path.clone());
                    break;
                }
            }
        }
    }

    Ok(renames)
}

// =============================
// Delta Sync Commands (#155)
// =============================

/// Analyze a file pair and return delta sync stats (preview, no actual transfer)
#[tauri::command]
async fn delta_sync_analyze(
    local_path: String,
    remote_path: String,
) -> Result<delta_sync::DeltaResult, String> {
    validate_path(&local_path)?;
    validate_path(&remote_path)?;

    // Read local file
    let local_data = tokio::fs::read(&local_path)
        .await
        .map_err(|e| format!("Failed to read local file: {}", e))?;

    if (local_data.len() as u64) < delta_sync::DELTA_MIN_FILE_SIZE {
        return Err(format!(
            "File too small for delta sync ({}B < {}B minimum)",
            local_data.len(),
            delta_sync::DELTA_MIN_FILE_SIZE
        ));
    }

    // For analysis, we use the local file as both source and simulate
    // In real usage, remote_data would come from provider.read_range()
    let block_size = delta_sync::compute_block_size(local_data.len() as u64);
    let sigs = delta_sync::compute_signatures(&local_data, block_size);

    // Read remote (local copy for now: real impl would use provider)
    let remote_data = tokio::fs::read(&remote_path)
        .await
        .map_err(|e| format!("Failed to read remote file: {}", e))?;

    let (_, result) = delta_sync::compute_delta(&remote_data, &sigs);
    Ok(result)
}

// =============================
// Canary Sync Commands
// =============================

/// Run a canary (sample-based) dry-run sync analysis.
/// Scans local files, selects a percentage-based sample, and projects
/// what a full sync would do without actually transferring anything.
#[tauri::command]
async fn sync_canary_run(
    local_path: String,
    remote_path: String,
    percent: u8,
    selection: String,
) -> Result<CanaryResult, String> {
    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    // Clamp percent to 5-50 range
    let percent = percent.clamp(5, 50);

    // Scan local files (no checksum for speed)
    let exclude_patterns = sync::CompareOptions::default().exclude_patterns;
    let local_files =
        get_local_files_recursive(&local_path, &local_path, &exclude_patterns, false, None).await?;

    // Only count non-directory files for sampling
    let total_files = local_files.iter().filter(|(_, f)| !f.is_dir).count();
    if total_files == 0 {
        return Ok(CanaryResult {
            sampled_files: 0,
            total_files: 0,
            results: Vec::new(),
            summary: CanarySummary {
                would_upload: 0,
                would_download: 0,
                would_delete: 0,
                conflicts: 0,
                errors: 0,
                estimated_transfer_size: 0,
            },
        });
    }

    // Calculate sample size: total * percent / 100, minimum 1
    let sample_size = ((total_files as u64 * percent as u64) / 100).max(1) as usize;

    // Select sample based on strategy
    let sample = select_canary_sample(&local_files, sample_size, &selection);

    // Build canary results by analyzing the sample
    // In dry-run mode, local-only files are projected as uploads
    let mut results = Vec::new();
    let mut would_upload: usize = 0;
    let mut would_download: usize = 0;
    let mut would_delete: usize = 0;
    let conflicts: usize = 0;
    let mut estimated_transfer_size: u64 = 0;

    // Load sync index for comparison if available
    let index = load_sync_index(&local_path, &remote_path).ok().flatten();

    for (rel_path, info) in &sample {
        // Determine projected action based on index state
        let action = if let Some(idx) = &index {
            if let Some(cached) = idx.files.get(rel_path) {
                // File exists in index: check if it changed locally
                let local_changed = info.size != cached.size
                    || !sync::timestamps_equal(info.modified, cached.modified);
                if local_changed {
                    "upload" // Changed since last sync
                } else {
                    "skip" // Unchanged
                }
            } else {
                "upload" // New file not in index
            }
        } else {
            "upload" // No index available: assume upload needed
        };

        if action == "skip" {
            continue;
        }

        match action {
            "upload" => {
                would_upload += 1;
                estimated_transfer_size += info.size;
            }
            "download" => {
                would_download += 1;
                estimated_transfer_size += info.size;
            }
            "delete" => {
                would_delete += 1;
            }
            _ => {}
        }

        results.push(CanarySampleResult {
            relative_path: rel_path.clone(),
            action: action.to_string(),
            success: true, // Dry-run always succeeds
            error: None,
            bytes: info.size,
        });
    }

    // Extrapolate totals based on sample ratio
    let sampled_files = sample.len();

    Ok(CanaryResult {
        sampled_files,
        total_files,
        results,
        summary: CanarySummary {
            would_upload,
            would_download,
            would_delete,
            conflicts,
            errors: 0,
            estimated_transfer_size,
        },
    })
}

/// Approve canary results: placeholder that returns a success message.
/// The actual full sync is triggered by the frontend calling `parallel_sync_execute`.
#[tauri::command]
async fn sync_canary_approve() -> Result<String, String> {
    Ok("Canary approved: proceed with full sync".to_string())
}

// =============================
// Signed Audit Log Commands
// =============================

/// Generate or retrieve a process-side journal signing key (A5-06 fix).
/// The key is stored in the app config directory, NOT in localStorage.
/// This prevents XSS from accessing the signing secret.
#[tauri::command]
async fn get_journal_signing_key(
    local_path: String,
    remote_path: String,
) -> Result<String, String> {
    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    let key_dir = portable::aeroftp_data_root()
        .ok_or_else(|| "Cannot determine AeroFTP data root".to_string())?
        .join("sync-journal");
    tokio::fs::create_dir_all(&key_dir)
        .await
        .map_err(|e| format!("Failed to create journal dir: {}", e))?;

    let key_file = key_dir.join("signing.key");

    // Load existing key or generate a new one
    let secret = if key_file.exists() {
        tokio::fs::read_to_string(&key_file)
            .await
            .map_err(|e| format!("Failed to read signing key: {}", e))?
    } else {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let hex_key = hex::encode(bytes);
        tokio::fs::write(&key_file, &hex_key)
            .await
            .map_err(|e| format!("Failed to write signing key: {}", e))?;
        // Restrict permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&key_file, perms).ok();
        }
        hex_key
    };

    // Derive per-path-pair key via HMAC-SHA256(secret, local|remote|salt)
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let data = format!("{}|{}|aeroftp-journal-signing", local_path, remote_path);
    let key_bytes =
        hex::decode(secret.trim()).map_err(|e| format!("Invalid signing key: {}", e))?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&key_bytes).map_err(|e| format!("HMAC key error: {}", e))?;
    mac.update(data.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Sign an existing sync journal with HMAC-SHA256.
/// Saves the hex-encoded signature as a .sig file alongside the journal.
#[tauri::command]
async fn sign_sync_journal(
    local_path: String,
    remote_path: String,
    signing_key: String,
) -> Result<String, String> {
    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    // Load the journal
    let journal = load_sync_journal(&local_path, &remote_path)?
        .ok_or_else(|| "No sync journal found for this path pair".to_string())?;

    // Decode hex signing key
    let key_bytes =
        hex::decode(&signing_key).map_err(|e| format!("Invalid hex signing key: {}", e))?;
    if key_bytes.is_empty() {
        return Err("Signing key cannot be empty".to_string());
    }
    if key_bytes.len() < 32 {
        return Err("Signing key must be at least 32 bytes (64 hex chars)".to_string());
    }

    // Compute HMAC-SHA256 signature
    let signature = sign_journal(&journal, &key_bytes)?;

    // Save .sig file alongside the journal
    let journal_dir = portable::aeroftp_data_root()
        .ok_or_else(|| "Cannot determine AeroFTP data root".to_string())?
        .join("sync-journal");
    let sig_path = journal_dir.join(journal_sig_filename(&local_path, &remote_path));
    tokio::fs::write(&sig_path, signature.as_bytes())
        .await
        .map_err(|e| format!("Failed to write signature file: {}", e))?;

    Ok(signature)
}

/// Verify an existing journal signature.
/// Returns true if the stored signature matches the recomputed HMAC.
#[tauri::command]
async fn verify_journal_signature(
    local_path: String,
    remote_path: String,
    signing_key: String,
) -> Result<bool, String> {
    validate_path(&local_path)?;
    if remote_path.contains('\0') {
        return Err("Remote path contains null bytes".to_string());
    }

    // Load the journal
    let journal = load_sync_journal(&local_path, &remote_path)?
        .ok_or_else(|| "No sync journal found for this path pair".to_string())?;

    // Read the .sig file
    let journal_dir = portable::aeroftp_data_root()
        .ok_or_else(|| "Cannot determine AeroFTP data root".to_string())?
        .join("sync-journal");
    let sig_path = journal_dir.join(journal_sig_filename(&local_path, &remote_path));
    let stored_sig = tokio::fs::read_to_string(&sig_path)
        .await
        .map_err(|e| format!("Failed to read signature file: {}", e))?;

    // Decode hex signing key
    let key_bytes =
        hex::decode(&signing_key).map_err(|e| format!("Invalid hex signing key: {}", e))?;
    if key_bytes.is_empty() {
        return Err("Signing key cannot be empty".to_string());
    }
    if key_bytes.len() < 32 {
        return Err("Signing key must be at least 32 bytes (64 hex chars)".to_string());
    }

    // Recompute HMAC-SHA256
    let computed_sig = sign_journal(&journal, &key_bytes)?;

    // Constant-time comparison to prevent timing attacks
    let a = computed_sig.as_bytes();
    let b = stored_sig.trim().as_bytes();
    let result = if a.len() != b.len() {
        false
    } else {
        a.iter()
            .zip(b.iter())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
    };
    Ok(result)
}

/// Execute sync transfers in parallel using a bounded Semaphore pool.
///
/// Each stream creates its own FTP connection (FTP doesn't support multiplexing).
/// Progress events are emitted per-stream with `stream_id` for UI tracking.
/// The journal is updated atomically after each transfer completes.
#[tauri::command]
async fn parallel_sync_execute(
    app: AppHandle,
    transfers: Vec<transfer_pool::SyncTransferEntry>,
    server_host: String,
    server_user: String,
    server_pass: String,
    max_streams: u8,
) -> Result<transfer_pool::ParallelSyncResult, String> {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // Validate all transfer entry paths before processing
    for entry in &transfers {
        filesystem::validate_path(&entry.local_path)?;
        // remote_path validation: reject null bytes and path traversal
        if entry.remote_path.contains('\0') || entry.remote_path.contains("..") {
            return Err(format!("Invalid remote path: {}", entry.relative_path));
        }
    }

    let start = Instant::now();
    // P2-5: Use validate_config for consistent validation (clamp streams + default timeout)
    let mut pool_config = transfer_pool::ParallelTransferConfig {
        max_streams,
        acquire_timeout_ms: 30000,
    };
    transfer_pool::validate_config(&mut pool_config);
    let max_streams = pool_config.max_streams as usize;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_streams));
    let result = Arc::new(Mutex::new(transfer_pool::ParallelSyncResult::new()));
    let total_count = transfers.len();

    info!(
        "parallel_sync_execute: {} transfers, {} streams, host={}",
        total_count, max_streams, server_host
    );

    // Emit start event
    let _ = app.emit(
        "sync-parallel-progress",
        serde_json::json!({
            "phase": "start",
            "total": total_count,
            "streams": max_streams,
        }),
    );

    let mut join_set = tokio::task::JoinSet::new();

    for (index, entry) in transfers.into_iter().enumerate() {
        let sem = semaphore.clone();
        let res = result.clone();
        let app_clone = app.clone();
        let host = server_host.clone();
        let user = server_user.clone();
        let pass = server_pass.clone();

        join_set.spawn(async move {
            // Acquire semaphore permit (bounds concurrency)
            let _permit = match sem.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    let mut r = res.lock().await;
                    r.errors.push(transfer_pool::ParallelTransferError {
                        relative_path: entry.relative_path.clone(),
                        action: entry.action.clone(),
                        error: "Semaphore closed".to_string(),
                        retryable: false,
                    });
                    return;
                }
            };

            let stream_id = index % 8; // Visual stream assignment

            // Emit per-file start
            let _ = app_clone.emit(
                "sync-parallel-progress",
                serde_json::json!({
                    "phase": "file_start",
                    "stream_id": stream_id,
                    "relative_path": entry.relative_path,
                    "action": entry.action,
                    "index": index,
                    "total": total_count,
                }),
            );

            // Skip directories (mkdir is handled separately)
            if entry.is_dir {
                if entry.action == transfer_pool::TransferAction::Mkdir {
                    // Create local directory
                    let _ = tokio::fs::create_dir_all(&entry.local_path).await;
                }
                let mut r = res.lock().await;
                r.skipped += 1;
                return;
            }

            // Execute transfer with a dedicated FTP connection
            let transfer_result = execute_single_transfer(
                &host,
                &user,
                &pass,
                &entry,
                &app_clone,
                stream_id,
                index,
                total_count,
            )
            .await;

            let mut r = res.lock().await;
            match transfer_result {
                Ok(action) => match action.as_str() {
                    "uploaded" => r.uploaded += 1,
                    "downloaded" => r.downloaded += 1,
                    "deleted" => r.deleted += 1,
                    _ => r.skipped += 1,
                },
                Err(e) => {
                    let retryable =
                        sync::classify_sync_error(&e, Some(&entry.relative_path)).retryable;
                    r.errors.push(transfer_pool::ParallelTransferError {
                        relative_path: entry.relative_path.clone(),
                        action: entry.action.clone(),
                        error: e,
                        retryable,
                    });
                }
            }

            // Emit per-file complete
            let _ = app_clone.emit(
                "sync-parallel-progress",
                serde_json::json!({
                    "phase": "file_complete",
                    "stream_id": stream_id,
                    "relative_path": entry.relative_path,
                    "action": entry.action,
                    "index": index,
                    "total": total_count,
                }),
            );
        });
    }

    // Wait for all transfers to complete, propagating JoinErrors (panics/cancellations)
    while let Some(join_result) = join_set.join_next().await {
        if let Err(join_err) = join_result {
            let mut r = result.lock().await;
            let err_index = r.errors.len();
            r.errors.push(transfer_pool::ParallelTransferError {
                relative_path: format!("task-{}", err_index),
                action: transfer_pool::TransferAction::Upload,
                error: format!("Task panicked: {}", join_err),
                retryable: false,
            });
        }
    }

    let mut final_result = result.lock().await;
    final_result.duration_ms = start.elapsed().as_millis() as u64;
    final_result.streams_used = max_streams as u8;

    let result_clone = final_result.clone();

    info!(
        "parallel_sync_execute complete: ↑{} ↓{} ✗{} skip={} in {}ms using {} streams",
        result_clone.uploaded,
        result_clone.downloaded,
        result_clone.errors.len(),
        result_clone.skipped,
        result_clone.duration_ms,
        result_clone.streams_used,
    );

    // Emit completion
    let _ = app.emit(
        "sync-parallel-progress",
        serde_json::json!({
            "phase": "complete",
            "uploaded": result_clone.uploaded,
            "downloaded": result_clone.downloaded,
            "errors": result_clone.errors.len(),
            "duration_ms": result_clone.duration_ms,
        }),
    );

    Ok(result_clone)
}

/// Execute a single FTP transfer (upload, download, or delete) with a dedicated connection.
/// Each call creates and tears down its own FTP connection to avoid multiplexing issues.
#[allow(clippy::too_many_arguments)]
async fn execute_single_transfer(
    host: &str,
    user: &str,
    pass: &str,
    entry: &transfer_pool::SyncTransferEntry,
    app: &AppHandle,
    stream_id: usize,
    index: usize,
    total: usize,
) -> Result<String, String> {
    let mut ftp = ftp::FtpManager::new();

    ftp.connect(host)
        .await
        .map_err(|e| format!("Stream {}: connect failed: {}", stream_id, e))?;
    ftp.login(user, pass)
        .await
        .map_err(|e| format!("Stream {}: login failed: {}", stream_id, e))?;

    let result = match entry.action {
        transfer_pool::TransferAction::Upload => {
            // Ensure parent directory exists
            if let Some(parent) = std::path::Path::new(&entry.remote_path).parent() {
                let parent_str = parent.to_string_lossy().to_string();
                if !parent_str.is_empty() && parent_str != "/" {
                    let _ = ftp.mkdir(&parent_str).await; // ignore if exists
                }
            }

            let file_size = tokio::fs::metadata(&entry.local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(entry.expected_size);

            let start_time = Instant::now();
            let app_ref = app.clone();
            let transfer_id = format!("psync-{}-{}", stream_id, index);
            let filename = entry.relative_path.clone();
            let mut last_emit_time_ul = Instant::now();
            let mut last_emit_pct_ul: u8 = 0;

            ftp.upload_file_with_progress(
                &entry.local_path,
                &entry.remote_path,
                file_size,
                move |transferred| {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 {
                        (transferred as f64 / elapsed) as u64
                    } else {
                        0
                    };
                    let pct = if file_size > 0 {
                        ((transferred as f64 / file_size as f64) * 100.0) as u8
                    } else {
                        0
                    };

                    // Throttle: emit every 150ms or 2% delta (matches standard transfer path)
                    let is_complete = transferred >= file_size && file_size > 0;
                    let time_ok = last_emit_time_ul.elapsed().as_millis() >= 150;
                    let pct_ok = pct.saturating_sub(last_emit_pct_ul) >= 2;
                    if time_ok || pct_ok || is_complete {
                        last_emit_time_ul = Instant::now();
                        last_emit_pct_ul = pct;
                        let _ = app_ref.emit(
                            "sync-parallel-progress",
                            serde_json::json!({
                                "phase": "transfer_progress",
                                "stream_id": stream_id,
                                "transfer_id": transfer_id,
                                "relative_path": filename,
                                "direction": "upload",
                                "transferred": transferred,
                                "total": file_size,
                                "percentage": pct,
                                "speed_bps": speed,
                                "index": index,
                                "total_files": total,
                            }),
                        );
                    }
                    true // continue
                },
            )
            .await
            .map_err(|e| format!("Upload failed: {}", e))?;

            Ok("uploaded".to_string())
        }
        transfer_pool::TransferAction::Download => {
            // Ensure local parent directory exists
            if let Some(parent) = std::path::Path::new(&entry.local_path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }

            let file_size = ftp
                .get_file_size(&entry.remote_path)
                .await
                .unwrap_or(entry.expected_size);

            let start_time = Instant::now();
            let app_ref = app.clone();
            let transfer_id = format!("psync-{}-{}", stream_id, index);
            let filename = entry.relative_path.clone();
            let mut last_emit_time_dl = Instant::now();
            let mut last_emit_pct_dl: u8 = 0;

            ftp.download_file_with_progress(
                &entry.remote_path,
                &entry.local_path,
                move |transferred| {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let speed = if elapsed > 0.0 {
                        (transferred as f64 / elapsed) as u64
                    } else {
                        0
                    };
                    let pct = if file_size > 0 {
                        ((transferred as f64 / file_size as f64) * 100.0) as u8
                    } else {
                        0
                    };

                    // Throttle: emit every 150ms or 2% delta (matches standard transfer path)
                    let is_complete = transferred >= file_size && file_size > 0;
                    let time_ok = last_emit_time_dl.elapsed().as_millis() >= 150;
                    let pct_ok = pct.saturating_sub(last_emit_pct_dl) >= 2;
                    if time_ok || pct_ok || is_complete {
                        last_emit_time_dl = Instant::now();
                        last_emit_pct_dl = pct;
                        let _ = app_ref.emit(
                            "sync-parallel-progress",
                            serde_json::json!({
                                "phase": "transfer_progress",
                                "stream_id": stream_id,
                                "transfer_id": transfer_id,
                                "relative_path": filename,
                                "direction": "download",
                                "transferred": transferred,
                                "total": file_size,
                                "percentage": pct,
                                "speed_bps": speed,
                                "index": index,
                                "total_files": total,
                            }),
                        );
                    }
                    true // continue
                },
            )
            .await
            .map_err(|e| format!("Download failed: {}", e))?;

            Ok("downloaded".to_string())
        }
        transfer_pool::TransferAction::Delete => {
            // Delete remote file
            ftp.remove(&entry.remote_path)
                .await
                .map_err(|e| format!("Delete failed: {}", e))?;
            Ok("deleted".to_string())
        }
        transfer_pool::TransferAction::Mkdir => {
            // Mkdir handled at task level, skip here
            Ok("skipped".to_string())
        }
    };

    // Disconnect gracefully
    let _ = ftp.disconnect().await;

    result
}

// ─── End Phase 3A+ Commands ────────────────────────────────────────────

#[tauri::command]
fn get_default_retry_policy() -> RetryPolicy {
    RetryPolicy::default()
}

#[tauri::command]
fn verify_local_transfer(
    local_path: String,
    expected_size: u64,
    expected_mtime: Option<String>,
    expected_hash: Option<String>,
    policy: VerifyPolicy,
) -> VerifyResult {
    let mtime = expected_mtime.and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    });
    verify_local_file(
        &local_path,
        expected_size,
        mtime,
        &policy,
        expected_hash.as_deref(),
    )
}

#[tauri::command]
fn classify_transfer_error(raw_error: String, file_path: Option<String>) -> SyncErrorInfo {
    classify_sync_error(&raw_error, file_path.as_deref())
}

// ============ AI Commands ============

/// Active non-streaming ai_chat requests, keyed by a caller-supplied id.
/// `ai_cancel_chat(id)` flips the token and the running `call_ai` future
/// drops: the underlying reqwest HTTP request is cancelled by drop.
static AI_CHAT_CANCEL_TOKENS: std::sync::LazyLock<
    tokio::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

#[tauri::command]
async fn ai_chat(
    request: ai::AIRequest,
    request_id: Option<String>,
) -> Result<ai::AIResponse, String> {
    let token = CancellationToken::new();

    // Register cancel token if the caller provided an id. Without an id the
    // call simply cannot be cancelled externally: same as before this fix -
    // but at least it will not block another id's cancel.
    if let Some(id) = request_id.as_ref() {
        AI_CHAT_CANCEL_TOKENS
            .lock()
            .await
            .insert(id.clone(), token.clone());
    }

    let call_future = ai::call_ai(request);
    let result = tokio::select! {
        _ = token.cancelled() => Err("AI request cancelled by user".to_string()),
        res = call_future => res.map_err(|e| e.to_string()),
    };

    if let Some(id) = request_id {
        AI_CHAT_CANCEL_TOKENS.lock().await.remove(&id);
    }

    result
}

/// Cancel a non-streaming `ai_chat` request identified by `request_id`.
/// No-op if the id is unknown.
#[tauri::command]
async fn ai_cancel_chat(request_id: String) -> Result<(), String> {
    if let Some(token) = AI_CHAT_CANCEL_TOKENS.lock().await.remove(&request_id) {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
async fn ai_test_provider(
    provider_type: ai::AIProviderType,
    base_url: String,
    api_key: Option<String>,
) -> Result<bool, String> {
    ai::test_provider(provider_type, base_url, api_key)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ai_list_models(
    provider_type: ai::AIProviderType,
    base_url: String,
    api_key: Option<String>,
) -> Result<Vec<String>, String> {
    ai::list_models(provider_type, base_url, api_key)
        .await
        .map_err(|e| e.to_string())
}

// Tool execution request
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolRequest {
    tool_name: String,
    args: serde_json::Value,
}

// Allowed AI tool names (whitelist)
const ALLOWED_AI_TOOLS: &[&str] = &[
    "list_files",
    "read_file",
    "create_folder",
    "delete_file",
    "rename_file",
    "download_file",
    "upload_file",
    "chmod",
];

/// Validate and sanitize a path argument from AI tool calls.
/// Rejects null bytes, path traversal sequences, and excessively long paths.
fn validate_tool_path(path: &str, param_name: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err(format!("{}: path exceeds 4096 characters", param_name));
    }
    if path.contains('\0') {
        return Err(format!("{}: path contains null bytes", param_name));
    }
    // Reject path traversal: literal ".." components
    for component in path.split('/') {
        if component == ".." {
            return Err(format!(
                "{}: path traversal ('..') is not allowed",
                param_name
            ));
        }
    }
    // Also check backslash-separated (Windows paths)
    for component in path.split('\\') {
        if component == ".." {
            return Err(format!(
                "{}: path traversal ('..') is not allowed",
                param_name
            ));
        }
    }
    Ok(())
}

/// Validate a chmod mode string (must be octal digits, 3-4 chars).
fn validate_chmod_mode(mode: &str) -> Result<(), String> {
    if mode.len() < 3 || mode.len() > 4 {
        return Err("mode must be 3-4 octal digits (e.g. '755')".to_string());
    }
    if !mode.chars().all(|c| c.is_ascii_digit() && c <= '7') {
        return Err("mode must contain only octal digits (0-7)".to_string());
    }
    Ok(())
}

// Execute AI tool - routes to existing FTP commands
#[tauri::command]
async fn ai_execute_tool(
    state: State<'_, AppState>,
    app: AppHandle,
    request: ToolRequest,
) -> Result<serde_json::Value, String> {
    // Validate tool name against whitelist
    if !ALLOWED_AI_TOOLS.contains(&request.tool_name.as_str()) {
        return Err(format!("Unknown or disallowed tool: {}", request.tool_name));
    }

    let args = request.args;

    match request.tool_name.as_str() {
        "list_files" => {
            let location = args
                .get("location")
                .and_then(|v| v.as_str())
                .unwrap_or("remote");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("/");
            validate_tool_path(path, "path")?;

            if location == "local" {
                let files = get_local_files(path.to_string(), Some(true))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "success": true,
                    "count": files.len(),
                    "files": files.iter().take(20).map(|f| {
                        serde_json::json!({
                            "name": f.name,
                            "is_dir": f.is_dir,
                            "size": f.size
                        })
                    }).collect::<Vec<_>>()
                }))
            } else {
                let mut manager = state.ftp_manager.lock().await;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "success": true,
                    "count": files.len(),
                    "files": files.iter().take(20).map(|f| {
                        serde_json::json!({
                            "name": f.name,
                            "is_dir": f.is_dir,
                            "size": f.size
                        })
                    }).collect::<Vec<_>>()
                }))
            }
        }

        "read_file" => {
            let location = args
                .get("location")
                .and_then(|v| v.as_str())
                .unwrap_or("remote");
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("path required")?;
            validate_tool_path(path, "path")?;

            if location == "local" {
                let content = read_local_file(path.to_string(), Some(5))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "success": true,
                    "content": content.chars().take(5000).collect::<String>(),
                    "truncated": content.len() > 5000
                }))
            } else {
                // AI tool preview: use FTP manager directly (provider path handled by Tauri command)
                let content = {
                    let mut ftp = state.ftp_manager.lock().await;
                    let temp = std::env::temp_dir().join(format!(
                        "aeroftp_ai_preview_{}",
                        chrono::Utc::now().timestamp_millis()
                    ));
                    let temp_str = temp.to_string_lossy().to_string();
                    ftp.download_file_with_progress(path, &temp_str, |_| true)
                        .await
                        .map_err(|e| format!("Failed to download: {}", e))?;
                    let c = tokio::fs::read_to_string(&temp)
                        .await
                        .map_err(|e| format!("Failed to read: {}", e))?;
                    let _ = tokio::fs::remove_file(&temp).await;
                    c
                };
                Ok(serde_json::json!({
                    "success": true,
                    "content": content.chars().take(5000).collect::<String>(),
                    "truncated": content.len() > 5000
                }))
            }
        }

        "create_folder" => {
            let location = args
                .get("location")
                .and_then(|v| v.as_str())
                .unwrap_or("remote");
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("path required")?;
            validate_tool_path(path, "path")?;

            if location == "local" {
                create_local_folder(path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                create_remote_folder(state.clone(), path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(
                serde_json::json!({ "success": true, "message": format!("Created folder: {}", path) }),
            )
        }

        "delete_file" => {
            let location = args
                .get("location")
                .and_then(|v| v.as_str())
                .unwrap_or("remote");
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("path required")?;
            validate_tool_path(path, "path")?;

            if location == "local" {
                delete_local_file(app.clone(), state.clone(), path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                // Assume file, not directory for simple delete
                delete_remote_file(app.clone(), state.clone(), path.to_string(), false)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(serde_json::json!({ "success": true, "message": format!("Deleted: {}", path) }))
        }

        "rename_file" => {
            let location = args
                .get("location")
                .and_then(|v| v.as_str())
                .unwrap_or("remote");
            let old_path = args
                .get("old_path")
                .and_then(|v| v.as_str())
                .ok_or("old_path required")?;
            let new_path = args
                .get("new_path")
                .and_then(|v| v.as_str())
                .ok_or("new_path required")?;
            validate_tool_path(old_path, "old_path")?;
            validate_tool_path(new_path, "new_path")?;

            if location == "local" {
                rename_local_file(old_path.to_string(), new_path.to_string(), None)
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                rename_remote_file(state.clone(), old_path.to_string(), new_path.to_string())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(
                serde_json::json!({ "success": true, "message": format!("Renamed {} to {}", old_path, new_path) }),
            )
        }

        "download_file" => {
            let remote_path = args
                .get("remote_path")
                .and_then(|v| v.as_str())
                .ok_or("remote_path required")?;
            let local_path = args
                .get("local_path")
                .and_then(|v| v.as_str())
                .ok_or("local_path required")?;
            validate_tool_path(remote_path, "remote_path")?;
            validate_tool_path(local_path, "local_path")?;

            download_file(
                app,
                state.clone(),
                DownloadParams {
                    remote_path: remote_path.to_string(),
                    local_path: local_path.to_string(),
                    modified: None,
                    use_delta: true,
                },
            )
            .await
            .map_err(|e| e.to_string())?;

            Ok(
                serde_json::json!({ "success": true, "message": format!("Downloaded {} to {}", remote_path, local_path) }),
            )
        }

        "upload_file" => {
            let local_path = args
                .get("local_path")
                .and_then(|v| v.as_str())
                .ok_or("local_path required")?;
            let remote_path = args
                .get("remote_path")
                .and_then(|v| v.as_str())
                .ok_or("remote_path required")?;
            validate_tool_path(local_path, "local_path")?;
            validate_tool_path(remote_path, "remote_path")?;

            // AI tool upload: use FTP manager directly
            {
                let mut ftp = state.ftp_manager.lock().await;
                ftp.upload_file_with_progress(local_path, remote_path, 0, |_| true)
                    .await
                    .map_err(|e| format!("Upload failed: {}", e))?;
            }

            Ok(
                serde_json::json!({ "success": true, "message": format!("Uploaded {} to {}", local_path, remote_path) }),
            )
        }

        "chmod" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("path required")?;
            let mode = args
                .get("mode")
                .and_then(|v| v.as_str())
                .ok_or("mode required")?;
            validate_tool_path(path, "path")?;
            validate_chmod_mode(mode)?;

            chmod_remote_file(state.clone(), path.to_string(), mode.to_string())
                .await
                .map_err(|e| e.to_string())?;

            Ok(
                serde_json::json!({ "success": true, "message": format!("Changed permissions of {} to {}", path, mode) }),
            )
        }

        _ => unreachable!(), // tool_name already validated against ALLOWED_AI_TOOLS
    }
}

// ============ AeroCloud Commands ============

#[tauri::command]
fn get_cloud_config() -> CloudConfig {
    cloud_config::load_cloud_config()
}

#[tauri::command]
fn save_cloud_config_cmd(config: CloudConfig) -> Result<(), String> {
    cloud_config::save_cloud_config(&config)
}

/// Update excluded folders for selective sync
#[tauri::command]
fn update_excluded_folders(excluded_folders: Vec<String>) -> Result<(), String> {
    let mut config = cloud_config::load_cloud_config();
    config.excluded_folders = excluded_folders;
    cloud_config::save_cloud_config(&config)
}

#[tauri::command]
fn list_file_versions(relative_path: String) -> Result<Vec<sync_versioning::VersionEntry>, String> {
    let config = cloud_config::load_cloud_config();
    let v = sync_versioning::SyncVersioning::new(&config.local_folder, config.versioning_strategy);
    v.list_versions(&relative_path)
}

#[tauri::command]
fn list_all_file_versions() -> Result<Vec<sync_versioning::VersionEntry>, String> {
    let config = cloud_config::load_cloud_config();
    let v = sync_versioning::SyncVersioning::new(&config.local_folder, config.versioning_strategy);
    v.list_all_versions()
}

#[tauri::command]
fn restore_file_version(archive_path: String, original_relative: String) -> Result<(), String> {
    let config = cloud_config::load_cloud_config();
    // Security: validate archive_path is within .aeroversions/ (prevent path traversal)
    let versions_dir = config.local_folder.join(".aeroversions");
    let canonical_archive = std::path::PathBuf::from(&archive_path);
    if !canonical_archive.starts_with(&versions_dir) || archive_path.contains("..") {
        return Err("Invalid archive path: must be within .aeroversions/".to_string());
    }
    // Security: validate original_relative does not escape local_folder (path traversal)
    if original_relative.contains("..")
        || original_relative.starts_with('/')
        || original_relative.starts_with('\\')
    {
        return Err("Invalid restore target: path traversal detected".to_string());
    }
    let target = config.local_folder.join(&original_relative);
    if !target.starts_with(&config.local_folder) {
        return Err("Invalid restore target: would write outside sync folder".to_string());
    }
    let v = sync_versioning::SyncVersioning::new(&config.local_folder, config.versioning_strategy);
    let entry = sync_versioning::VersionEntry {
        archive_path: canonical_archive,
        original_relative,
        archived_at: String::new(),
        size: 0,
    };
    v.restore(&entry)
}

#[tauri::command]
fn cleanup_versions() -> Result<sync_versioning::CleanupStats, String> {
    let config = cloud_config::load_cloud_config();
    let v = sync_versioning::SyncVersioning::new(&config.local_folder, config.versioning_strategy);
    v.cleanup()
}

#[tauri::command]
fn versions_disk_usage() -> u64 {
    let config = cloud_config::load_cloud_config();
    let v = sync_versioning::SyncVersioning::new(&config.local_folder, config.versioning_strategy);
    v.disk_usage()
}

/// Archive a local file before deleting it during sync (backup-before-delete safety net).
/// Uses TrashCan strategy with 30-day retention, archiving to <sync_root>/.aeroversions/.
#[tauri::command]
fn archive_before_sync_delete(
    sync_root: String,
    file_path: String,
    versioning_strategy: Option<String>,
) -> Result<String, String> {
    // Security: validate file_path is within sync_root
    let root = std::path::PathBuf::from(&sync_root);
    let target = std::path::PathBuf::from(&file_path);
    if file_path.contains("..") || !target.starts_with(&root) {
        return Err("Invalid path: must be within sync root".to_string());
    }
    let v = sync_versioning::SyncVersioning::new(
        &root,
        parse_versioning_strategy(versioning_strategy.as_deref()),
    );
    let archived = v.archive(&target)?;
    Ok(archived.to_string_lossy().to_string())
}

/// List remote folder tree for the selective sync UI.
/// Returns a flat list of folder paths with metadata.
#[tauri::command]
async fn list_remote_folders_tree(
    max_depth: Option<u32>,
) -> Result<Vec<serde_json::Value>, String> {
    let config = cloud_config::load_cloud_config();
    if !config.enabled {
        return Err("AeroCloud not configured".to_string());
    }

    let max_d = max_depth.unwrap_or(3).min(5);
    let mut provider = cloud_provider_factory::create_cloud_provider(&config).await?;
    provider
        .connect()
        .await
        .map_err(|e| format!("Connect failed: {}", e))?;

    // Crypt-overlay chokepoint (fail-closed). A crypt-bound AeroCloud profile
    // must show DECRYPTED folder names in the selective-sync tree (and match
    // `excluded_folders`, stored as plaintext paths, against them); without the
    // wrap the tree would list ciphertext directory names and exclusions would
    // never match. Mirrors the background-sync chokepoint.
    if let Some(store) = credential_store::CredentialStore::from_cache() {
        provider = crypt_overlay_provider::wrap_connected_provider_for_profile_named(
            provider,
            &config.server_profile,
            &store,
        )
        .await?;
    }

    let base = &config.remote_folder;
    let mut folders = Vec::new();
    let mut stack: Vec<(String, String, u32)> = vec![(base.clone(), String::new(), 0)];

    while let Some((path, rel, depth)) = stack.pop() {
        if depth > max_d {
            continue;
        }
        if provider.cd(&path).await.is_err() {
            continue;
        }
        let entries = provider
            .list(".")
            .await
            .map_err(|e| format!("List failed: {}", e))?;
        for entry in entries {
            if !entry.is_dir {
                continue;
            }
            let rel_path = if rel.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", rel, entry.name)
            };
            let excluded = config
                .excluded_folders
                .iter()
                .any(|ef| ef.trim_matches('/') == rel_path);
            folders.push(serde_json::json!({
                "path": rel_path,
                "name": entry.name,
                "depth": depth,
                "excluded": excluded,
            }));
            if !excluded {
                let child = format!("{}/{}", path.trim_end_matches('/'), entry.name);
                stack.push((child, rel_path, depth + 1));
            }
        }
    }

    let _ = provider.disconnect().await;
    Ok(folders)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn setup_aerocloud(
    cloud_name: String,
    local_folder: String,
    remote_folder: String,
    server_profile: String,
    sync_on_change: bool,
    sync_interval_secs: u64,
    protocol_type: Option<String>,
    connection_params: Option<serde_json::Value>,
) -> Result<CloudConfig, String> {
    let config = CloudConfig {
        enabled: true,
        cloud_name,
        local_folder: std::path::PathBuf::from(&local_folder),
        remote_folder: remote_folder.clone(),
        server_profile,
        sync_on_change,
        sync_interval_secs,
        protocol_type: protocol_type.unwrap_or_else(|| "ftp".to_string()),
        connection_params: connection_params.unwrap_or(serde_json::Value::Null),
        ..CloudConfig::default()
    };

    // Validate configuration
    cloud_config::validate_config(&config)?;

    // Ensure local cloud folder exists
    cloud_config::ensure_cloud_folder(&config)?;

    // Create default .aeroignore if it doesn't exist
    let aeroignore_path = config.local_folder.join(".aeroignore");
    if !aeroignore_path.exists() {
        let _ = std::fs::write(&aeroignore_path, sync_ignore::DEFAULT_AEROIGNORE_TEMPLATE);
    }

    // Save configuration
    cloud_config::save_cloud_config(&config)?;

    info!(
        "AeroCloud setup complete: protocol={}, local={}, remote={}",
        config.protocol_type, local_folder, remote_folder
    );

    Ok(config)
}

#[tauri::command]
fn get_cloud_status() -> CloudSyncStatus {
    let config = cloud_config::load_cloud_config();

    if !config.enabled {
        return CloudSyncStatus::NotConfigured;
    }

    if config.paused {
        return CloudSyncStatus::Paused;
    }

    CloudSyncStatus::Idle {
        last_sync: config.last_sync,
        next_sync: None, // Will be calculated by sync service
    }
}

#[tauri::command]
fn enable_aerocloud(enabled: bool) -> Result<CloudConfig, String> {
    let mut config = cloud_config::load_cloud_config();

    if enabled {
        // Validate before enabling
        cloud_config::validate_config(&config)?;
        cloud_config::ensure_cloud_folder(&config)?;
    }

    config.enabled = enabled;
    // Always clear the paused flag on explicit enable/disable so the two
    // state machines (enable vs pause) never conflict on re-enable.
    config.paused = false;
    cloud_config::save_cloud_config(&config)?;

    info!("AeroCloud {}", if enabled { "enabled" } else { "disabled" });

    Ok(config)
}

/// Pause AeroCloud without removing its configuration.
/// Stops the background sync worker and persists `paused = true`.
/// The auto-start effect in the frontend respects this flag on next launch.
#[tauri::command]
async fn pause_aerocloud(app: AppHandle) -> Result<CloudConfig, String> {
    let mut config = cloud_config::load_cloud_config();
    if !config.enabled {
        return Err("AeroCloud is not configured".to_string());
    }

    // Stop the worker if running. stop_background_sync emits "idle"; we override
    // with "paused" below so the frontend can distinguish the two states.
    if BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
        let _ = stop_background_sync(app.clone()).await;
    }

    config.paused = true;
    cloud_config::save_cloud_config(&config)?;

    let _ = app.emit(
        "cloud-sync-status",
        serde_json::json!({
            "status": "paused",
            "message": "AeroCloud paused"
        }),
    );

    info!("AeroCloud paused");
    Ok(config)
}

/// Resume AeroCloud after a pause. Clears the `paused` flag and starts the
/// background sync worker if the config is enabled.
#[tauri::command]
async fn resume_aerocloud(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<CloudConfig, String> {
    let mut config = cloud_config::load_cloud_config();
    if !config.enabled {
        return Err("AeroCloud is not configured".to_string());
    }

    config.paused = false;
    cloud_config::save_cloud_config(&config)?;

    // Best-effort start. If a worker is already running start_background_sync
    // returns Ok early. If the config is invalid we surface the error so the
    // caller can show it to the user.
    start_background_sync(app.clone(), state).await?;

    info!("AeroCloud resumed");
    Ok(config)
}

/// Fully disable AeroCloud: stop the worker and reset the configuration to
/// defaults. Re-enabling requires going through the setup wizard again.
#[tauri::command]
async fn disable_aerocloud(app: AppHandle) -> Result<(), String> {
    if BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
        let _ = stop_background_sync(app.clone()).await;
    }

    cloud_config::save_cloud_config(&CloudConfig::default())?;

    let _ = app.emit(
        "cloud-sync-status",
        serde_json::json!({
            "status": "disabled",
            "message": "AeroCloud disabled"
        }),
    );

    info!("AeroCloud disabled and configuration reset");
    Ok(())
}

/// Generate a shareable link for a file in AeroCloud
/// Returns the public URL if public_url_base is configured
#[tauri::command]
fn generate_share_link(local_path: String) -> Result<String, String> {
    let config = cloud_config::load_cloud_config();

    if !config.enabled {
        return Err("AeroCloud is not enabled".to_string());
    }

    let public_base = config.public_url_base.as_ref().ok_or_else(|| {
        "Public URL not configured. Go to AeroCloud Settings to set your public URL base."
            .to_string()
    })?;

    let local_folder = config.local_folder.to_string_lossy();
    let local_path_str = local_path.clone();

    // Check if file is within AeroCloud folder
    let local_folder_str: &str = local_folder.as_ref();
    if !local_path_str.starts_with(local_folder_str) {
        return Err("File is not in AeroCloud folder".to_string());
    }

    // Get relative path from AeroCloud folder
    let relative_path = local_path_str
        .strip_prefix(local_folder_str)
        .unwrap_or(&local_path_str)
        .trim_start_matches('/');

    // Construct public URL
    let base = public_base.trim_end_matches('/');
    let url = format!("{}/{}", base, relative_path);

    info!("Generated share link: {}", url);

    Ok(url)
}

/// Generate share link from remote path (when browsing remote files)
#[tauri::command]
fn generate_share_link_remote(remote_path: String) -> Result<String, String> {
    let config = cloud_config::load_cloud_config();

    if !config.enabled {
        return Err("AeroCloud is not enabled".to_string());
    }

    let public_base = config.public_url_base.as_ref().ok_or_else(|| {
        "Public URL not configured. Go to AeroCloud Settings to set your public URL base."
            .to_string()
    })?;

    // Check if path is within AeroCloud remote folder
    let remote_folder = config.remote_folder.trim_end_matches('/');
    if !remote_path.starts_with(remote_folder) {
        return Err("File is not in AeroCloud remote folder".to_string());
    }

    // Get relative path from remote folder
    let relative_path = remote_path
        .strip_prefix(remote_folder)
        .unwrap_or(&remote_path)
        .trim_start_matches('/');

    // Construct public URL
    let base = public_base.trim_end_matches('/');
    let url = format!("{}/{}", base, relative_path);

    info!("Generated share link (remote): {}", url);

    Ok(url)
}

/// Generate share link for any server with a configured public URL base.
/// Works for FTP/FTPS/SFTP/WebDAV: maps remote path to HTTP URL.
#[tauri::command]
fn generate_server_share_link(
    public_url_base: String,
    initial_path: String,
    remote_path: String,
) -> Result<String, String> {
    if public_url_base.is_empty() {
        return Err("Public URL base not configured for this server".to_string());
    }

    // SL-H01: Only allow http/https schemes
    if !public_url_base.starts_with("http://") && !public_url_base.starts_with("https://") {
        return Err("Public URL base must start with http:// or https://".to_string());
    }

    let root = initial_path.trim_end_matches('/');
    let base = public_url_base.trim_end_matches('/');

    // Strip server root from remote path to get relative path
    let relative = if !root.is_empty() && remote_path.starts_with(root) {
        remote_path
            .strip_prefix(root)
            .unwrap_or(&remote_path)
            .trim_start_matches('/')
    } else {
        // No initial path or path doesn't match: use full remote path
        remote_path.trim_start_matches('/')
    };

    if relative.is_empty() {
        return Err("Cannot generate share link for root directory".to_string());
    }

    // URL-encode path segments (spaces, special chars) but preserve /
    let encoded = relative
        .split('/')
        .map(|seg| urlencoding::encode(seg))
        .collect::<Vec<_>>()
        .join("/");

    let url = format!("{}/{}", base, encoded);
    debug!("Generated server share link: {}", url);
    Ok(url)
}

#[tauri::command]
fn get_default_cloud_folder() -> String {
    let default_config = CloudConfig::default();
    default_config.local_folder.to_string_lossy().to_string()
}

#[tauri::command]
fn update_conflict_strategy(strategy: ConflictStrategy) -> Result<(), String> {
    let mut config = cloud_config::load_cloud_config();
    config.conflict_strategy = strategy;
    cloud_config::save_cloud_config(&config)
}

#[tauri::command]
async fn trigger_cloud_sync(
    app: AppHandle,
    _state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let config = cloud_config::load_cloud_config();

    info!("AeroCloud: manual sync started");
    info!(
        "Config - enabled: {}, local: {:?}, remote: {}, protocol: {}",
        config.enabled, config.local_folder, config.remote_folder, config.protocol_type
    );

    if !config.enabled {
        return Err("AeroCloud is not configured. Please set it up first.".to_string());
    }

    // Use multi-protocol factory (same as background sync): supports FTP, SFTP, S3, etc.
    let result = perform_background_sync_with_app(&config, Some(&app)).await;

    match result {
        Ok(result) => {
            // Update global last sync timestamp so watcher cooldown applies
            LAST_SYNC_EPOCH.store(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                Ordering::SeqCst,
            );
            let summary = format!(
                "Sync complete: {} uploaded, {} downloaded, {} conflicts, {} skipped, {} errors",
                result.uploaded,
                result.downloaded,
                result.conflicts,
                result.skipped,
                result.errors.len()
            );
            info!("{}", summary);
            if !result.errors.is_empty() {
                for err in &result.errors {
                    warn!("Sync error: {}", err);
                }
            }
            Ok(summary)
        }
        Err(e) => {
            error!("Sync failed: {}", e);
            Err(format!("Sync failed: {}", e))
        }
    }
}
// ============ Background Sync & Tray Commands ============

use std::time::Duration;

/// Prevents concurrent syncs (manual + watcher firing at the same time)
static SYNC_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

// Global flag to control background sync
pub(crate) static BACKGROUND_SYNC_RUNNING: AtomicBool = AtomicBool::new(false);
static BACKGROUND_SYNC_CANCEL: std::sync::LazyLock<std::sync::Mutex<CancellationToken>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(CancellationToken::new()));
/// JoinHandle of the currently running background sync worker, so
/// `stop_background_sync` can await a graceful exit rather than fire-and-forget.
/// Previously the handle was dropped (detached), meaning the UI showed "stopped"
/// while the worker could still emit `cloud-sync-status` events.
static BACKGROUND_SYNC_HANDLE: std::sync::LazyLock<
    tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));
/// Epoch seconds of last completed sync (shared between manual + background)
static LAST_SYNC_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn reset_background_sync_cancel_token() -> CancellationToken {
    let token = CancellationToken::new();
    let mut guard = BACKGROUND_SYNC_CANCEL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = token.clone();
    token
}

fn background_sync_cancel_token() -> CancellationToken {
    BACKGROUND_SYNC_CANCEL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn cancel_background_sync_waits() {
    BACKGROUND_SYNC_CANCEL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .cancel();
}

/// Background sync worker: `tokio::select!` event loop
///
/// Listens for three trigger sources:
/// 1. **Scheduler timer**: fires based on `SyncSchedule` (interval + time window)
/// 2. **Filesystem watcher**: fires when files change in the local sync folder
/// 3. **Manual trigger**: fires when user clicks "Sync Now" via mpsc channel
///
/// Creates its own FTP connection per cycle to avoid conflicts with main UI.
async fn background_sync_worker(app: AppHandle) {
    info!("Background sync worker started (Phase 3A+ engine)");
    let stop_token = background_sync_cancel_token();

    // --- Setup filesystem watcher (Dropbox-style real-time sync) ---
    let (watcher_tx, mut watcher_rx) = tokio::sync::mpsc::channel::<file_watcher::WatcherEvent>(64);
    let mut watcher: Option<file_watcher::FileWatcher> = None;

    {
        let config = cloud_config::load_cloud_config();
        if config.sync_on_change {
            let local_path = config.local_folder.clone();
            let mut fw = file_watcher::FileWatcher::new(watcher_tx.clone());
            match fw.start(&local_path, file_watcher::WatcherMode::Auto) {
                Ok(()) => {
                    info!("Filesystem watcher active on {}", local_path.display());
                    let _ = app.emit(
                        "cloud-watcher-status",
                        serde_json::json!({
                            "active": true,
                            "path": local_path.to_string_lossy(),
                        }),
                    );
                    watcher = Some(fw);
                }
                Err(e) => {
                    warn!("Failed to start filesystem watcher: {}", e);
                }
            }
        }
    }

    // --- Main event loop ---
    let mut is_first_run = true;
    let mut last_sync_completed = tokio::time::Instant::now() - Duration::from_secs(120); // allow first sync immediately
    const WATCHER_COOLDOWN_SECS: u64 = 30; // min seconds between watcher-triggered syncs

    loop {
        // Check global stop flag
        if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
            info!("Background sync worker stopping (flag set to false)");
            break;
        }

        // Load fresh config and schedule each cycle
        let config = cloud_config::load_cloud_config();
        if !config.enabled {
            info!("AeroCloud disabled, stopping background sync");
            BACKGROUND_SYNC_RUNNING.store(false, Ordering::SeqCst);
            tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Default);
            let _ = app.emit(
                "cloud-sync-status",
                serde_json::json!({
                    "status": "disabled",
                    "message": "AeroCloud is disabled"
                }),
            );
            break;
        }

        // Determine trigger source for this cycle
        let trigger: transfer_pool::SyncTrigger = if is_first_run {
            is_first_run = false;
            // Only sync on startup if explicitly configured
            if config.sync_on_startup {
                transfer_pool::SyncTrigger::Manual
            } else {
                continue; // Skip first run, wait for normal interval
            }
        } else {
            // Load scheduler state
            let schedule = sync_scheduler::load_sync_schedule();

            // Emit schedule countdown to frontend
            if let Some(next_secs) = schedule.next_sync_in() {
                let _ = app.emit(
                    "cloud-sync-schedule",
                    serde_json::json!({
                        "next_sync_in_secs": next_secs,
                        "enabled": schedule.enabled,
                        "paused": schedule.paused,
                        "in_time_window": schedule.is_in_time_window(),
                    }),
                );
            }

            // Compute sleep duration: min of scheduler interval and 5s poll
            let sleep_secs = if schedule.enabled && !schedule.paused {
                schedule.next_sync_in().unwrap_or(30).min(30)
            } else {
                config.sync_interval_secs.max(30)
            };

            // Wait using tokio::select!: first event wins
            tokio::select! {
                _ = stop_token.cancelled() => {
                    transfer_pool::SyncTrigger::Stop
                }
                // Timer tick (scheduler interval or config interval)
                _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {
                    // Check if schedule allows sync now
                    let schedule = sync_scheduler::load_sync_schedule();
                    if schedule.enabled && schedule.should_sync_now() {
                        transfer_pool::SyncTrigger::Scheduled
                    } else if !schedule.enabled {
                        // Fallback to legacy interval logic when scheduler is disabled
                        transfer_pool::SyncTrigger::Scheduled
                    } else {
                        continue; // Not time yet, loop again
                    }
                }
                // Filesystem watcher event
                Some(event) = watcher_rx.recv() => {
                    // Suppress watcher during active folder download/upload
                    if crate::provider_commands::TRANSFER_IN_PROGRESS.load(std::sync::atomic::Ordering::SeqCst) {
                        info!("Watcher trigger suppressed: folder transfer in progress");
                        while watcher_rx.try_recv().is_ok() {}
                        continue;
                    }
                    // Cooldown: skip watcher triggers too close to last sync
                    // (prevents loop: sync writes files → watcher detects → re-sync)
                    // Check both local elapsed AND global epoch (covers manual sync)
                    let elapsed = last_sync_completed.elapsed().as_secs();
                    let global_elapsed = {
                        let last_epoch = LAST_SYNC_EPOCH.load(Ordering::SeqCst);
                        if last_epoch > 0 {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                .saturating_sub(last_epoch)
                        } else {
                            u64::MAX // no sync yet
                        }
                    };
                    let effective_elapsed = elapsed.min(global_elapsed);
                    if effective_elapsed < WATCHER_COOLDOWN_SECS {
                        info!("Watcher trigger suppressed: {}s since last sync (cooldown {}s)",
                            effective_elapsed, WATCHER_COOLDOWN_SECS);
                        // Drain any queued watcher events
                        while watcher_rx.try_recv().is_ok() {}
                        continue;
                    }
                    info!("Watcher trigger: {} paths changed", event.paths.len());
                    transfer_pool::SyncTrigger::FileChanged(event.paths)
                }
            }
        };

        // Check stop flag after wait
        if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
            break;
        }

        // --- Execute sync cycle ---
        let trigger_label = match &trigger {
            transfer_pool::SyncTrigger::Scheduled => "scheduled",
            transfer_pool::SyncTrigger::FileChanged(paths) => {
                info!("Watcher-triggered sync for {} changed paths", paths.len());
                "watcher"
            }
            transfer_pool::SyncTrigger::Manual => "manual",
            transfer_pool::SyncTrigger::Stop => break,
        };

        info!(
            "Background sync: starting cycle (trigger: {})",
            trigger_label
        );

        // Update tray badge and emit status
        tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Syncing);
        let _ = app.emit(
            "cloud-sync-status",
            serde_json::json!({
                "status": "syncing",
                "message": "Syncing...",
                "trigger": trigger_label,
            }),
        );

        {
            let local_folder = std::path::Path::new(&config.local_folder);
            sync_badge::update_directory_state(local_folder, sync_badge::SyncBadgeState::Syncing)
                .await;
        }

        match perform_background_sync(&config).await {
            Ok(result) => {
                info!(
                    "Background sync completed: {} uploaded, {} downloaded, {} errors",
                    result.uploaded,
                    result.downloaded,
                    result.errors.len()
                );

                // Mark sync completed and drain watcher events generated by the sync itself
                last_sync_completed = tokio::time::Instant::now();
                LAST_SYNC_EPOCH.store(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    Ordering::SeqCst,
                );
                let drained = {
                    let mut count = 0u32;
                    while watcher_rx.try_recv().is_ok() {
                        count += 1;
                    }
                    count
                };
                if drained > 0 {
                    info!("Drained {} watcher events generated during sync", drained);
                }

                {
                    let local_folder = std::path::Path::new(&config.local_folder);
                    sync_badge::update_directory_state(
                        local_folder,
                        sync_badge::SyncBadgeState::Synced,
                    )
                    .await;
                }

                tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Default);

                // Update scheduler last_sync timestamp
                let mut schedule = sync_scheduler::load_sync_schedule();
                schedule.last_sync = Some(chrono::Utc::now());
                let _ = sync_scheduler::save_sync_schedule(&schedule);

                let _ = app.emit(
                    "cloud-sync-status",
                    serde_json::json!({
                        "status": "active",
                        "message": format!("Synced: ↑{} ↓{}", result.uploaded, result.downloaded)
                    }),
                );
                let _ = app.emit("cloud_sync_complete", &result);
            }
            Err(e) => {
                warn!("Background sync failed: {}", e);

                // Mark sync completed (even on error) and drain watcher events
                last_sync_completed = tokio::time::Instant::now();
                while watcher_rx.try_recv().is_ok() {}

                {
                    let local_folder = std::path::Path::new(&config.local_folder);
                    sync_badge::update_directory_state(
                        local_folder,
                        sync_badge::SyncBadgeState::Error,
                    )
                    .await;
                }

                tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Error);

                let _ = app.emit(
                    "cloud-sync-status",
                    serde_json::json!({
                        "status": "error",
                        "message": format!("Sync failed: {}", e)
                    }),
                );

                // On error, wait before retrying
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                    _ = stop_token.cancelled() => break,
                }
            }
        }
    }

    // --- Cleanup ---
    if let Some(mut fw) = watcher {
        fw.stop();
        info!("Filesystem watcher stopped");
    }
    let _ = app.emit(
        "cloud-watcher-status",
        serde_json::json!({
            "active": false,
        }),
    );

    info!("Background sync worker exited");
}

/// Perform a sync cycle with a dedicated provider connection.
/// Creates the appropriate provider based on config.protocol_type (FTP, SFTP, S3, Google Drive, etc.)
/// and uses the generic perform_full_sync_with_provider method.
async fn perform_background_sync(
    config: &cloud_config::CloudConfig,
) -> Result<cloud_service::SyncOperationResult, String> {
    perform_background_sync_with_app(config, None).await
}

async fn perform_background_sync_with_app(
    config: &cloud_config::CloudConfig,
    app: Option<&AppHandle>,
) -> Result<cloud_service::SyncOperationResult, String> {
    // Prevent concurrent syncs: if one is already running, skip
    if SYNC_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        info!("Sync skipped: another sync is already in progress");
        return Ok(cloud_service::SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            conflicts: 0,
            errors: Vec::new(),
            duration_secs: 0,
            file_details: Vec::new(),
        });
    }

    let result = perform_background_sync_inner(config, app).await;
    SYNC_IN_PROGRESS.store(false, Ordering::SeqCst);
    result
}

async fn perform_background_sync_inner(
    config: &cloud_config::CloudConfig,
    app: Option<&AppHandle>,
) -> Result<cloud_service::SyncOperationResult, String> {
    info!(
        "Background sync: creating {} provider for profile '{}'",
        config.protocol_type, config.server_profile
    );

    // Create and connect the provider via multi-protocol factory
    let mut provider = cloud_provider_factory::create_cloud_provider(config).await?;

    info!("Background sync: connected via {}", config.protocol_type);

    // Crypt-overlay chokepoint (fail-closed). If the AeroCloud server profile is
    // crypt-bound, wrap the freshly-connected provider so background sync speaks
    // plaintext while the wire stays fully encrypted, exactly like the CLI / MCP /
    // cross-profile resolvers. Without this a scheduled sync of a crypt-bound
    // profile would read ciphertext and inject plaintext into the encrypted
    // remote. Wrapping BEFORE the cd below routes every path (cd/mkdir/list/
    // upload/download) through the decorator. A non-crypt profile is returned
    // byte-identical; a bound-but-locked vault refuses the sync rather than
    // running it raw.
    if let Some(store) = credential_store::CredentialStore::from_cache() {
        provider = crypt_overlay_provider::wrap_connected_provider_for_profile_named(
            provider,
            &config.server_profile,
            &store,
        )
        .await?;
    }

    // Navigate to remote folder
    if provider.cd(&config.remote_folder).await.is_err() {
        // Try to create it
        let _ = provider.mkdir(&config.remote_folder).await;
        provider
            .cd(&config.remote_folder)
            .await
            .map_err(|e| format!("Failed to navigate to remote folder: {}", e))?;
    }

    // Create cloud service and perform sync using the generic provider method
    let mut cloud_service = cloud_service::CloudService::new();
    if let Some(handle) = app {
        cloud_service.set_app_handle(handle.clone());
    }
    cloud_service.init(config.clone()).await;

    let result = cloud_service
        .perform_full_sync_with_provider(provider.as_mut())
        .await?;

    // Disconnect
    let _ = provider.disconnect().await;

    Ok(result)
}

#[tauri::command]
async fn start_background_sync(
    app: AppHandle,
    _state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
        return Ok("Background sync already running".to_string());
    }

    let config = cloud_config::load_cloud_config();
    if !config.enabled {
        return Err("AeroCloud not configured".to_string());
    }
    if config.paused {
        return Ok("Background sync paused".to_string());
    }

    reset_background_sync_cancel_token();

    // Set flag before spawning
    BACKGROUND_SYNC_RUNNING.store(true, Ordering::SeqCst);

    // Start badge server for file manager integration (Nautilus/Nemo)
    if let Err(e) = sync_badge::start_badge_server(app.clone()).await {
        warn!("Badge server failed to start (non-fatal): {}", e);
    }

    // Register sync root so files in local folder show green badge
    let local_folder = std::path::PathBuf::from(&config.local_folder);
    sync_badge::register_sync_root(local_folder).await;

    // Clone app handle for the spawned task
    let app_clone = app.clone();

    // Spawn background worker and retain the JoinHandle so stop_background_sync
    // can await a clean exit. If a previous handle is still around (e.g. the
    // worker was mid-shutdown), abort it before overwriting.
    let handle = tokio::spawn(async move {
        background_sync_worker(app_clone).await;
    });
    {
        let mut guard = BACKGROUND_SYNC_HANDLE.lock().await;
        if let Some(prev) = guard.take() {
            prev.abort();
        }
        *guard = Some(handle);
    }

    // Emit status
    let _ = app.emit(
        "cloud-sync-status",
        serde_json::json!({
            "status": "active",
            "message": "Background sync started"
        }),
    );

    info!(
        "Background sync started with interval: {}s",
        config.sync_interval_secs
    );

    Ok(format!(
        "Background sync started (interval: {}s)",
        config.sync_interval_secs
    ))
}

#[tauri::command]
async fn stop_background_sync(app: AppHandle) -> Result<String, String> {
    if !BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst) {
        return Ok("Background sync not running".to_string());
    }

    BACKGROUND_SYNC_RUNNING.store(false, Ordering::SeqCst);
    cancel_background_sync_waits();

    // Await the worker with a bounded grace window. If it finishes cleanly
    // no events will fire after the status emit below. If it takes too long
    // (stuck provider I/O) we abort so the command returns deterministically.
    let maybe_handle = {
        let mut guard = BACKGROUND_SYNC_HANDLE.lock().await;
        guard.take()
    };
    if let Some(handle) = maybe_handle {
        match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.is_cancelled() => {}
            Ok(Err(e)) => warn!("background sync worker join error: {}", e),
            Err(_) => {
                warn!("background sync worker did not exit within 5s, aborting");
                // Re-fetch via a short-lived guard to abort if still present;
                // the worker may race us by completing naturally.
                // (handle was already taken above; nothing to abort here.)
            }
        }
    }

    // Stop badge server and clear sync roots
    sync_badge::stop_badge_server().await;
    sync_badge::clear_all_states().await;

    // Reset tray badge to default (no badge)
    tray_badge::update_tray_badge(&app, tray_badge::TrayBadgeState::Default);

    // Emit status
    let _ = app.emit(
        "cloud-sync-status",
        serde_json::json!({
            "status": "idle",
            "message": "Background sync stopped"
        }),
    );

    info!("Background sync stopped");

    Ok("Background sync stopped".to_string())
}

#[tauri::command]
fn is_background_sync_running() -> bool {
    BACKGROUND_SYNC_RUNNING.load(Ordering::SeqCst)
}

#[tauri::command]
async fn set_tray_status(
    app: AppHandle,
    status: String,
    tooltip: Option<String>,
) -> Result<(), String> {
    let _ = app.emit(
        "tray-status-update",
        serde_json::json!({
            "status": status,
            "tooltip": tooltip.unwrap_or_else(|| "AeroCloud".to_string())
        }),
    );

    info!("Tray status updated: {}", status);
    Ok(())
}

#[tauri::command]
async fn update_tray_badge_cmd(app: AppHandle, state: String) -> Result<(), String> {
    let badge_state = tray_badge::TrayBadgeState::from_str(&state);
    tray_badge::update_tray_badge(&app, badge_state);
    Ok(())
}

/// Save server credentials for background sync use
#[tauri::command]
async fn save_server_credentials(
    profile_name: String,
    server: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;

    let value = serde_json::json!({
        "server": server,
        "username": username,
        "password": password,
    });

    // MUV-3: dual-write the per-profile credential blob (vault + active user's
    // partition).
    user_partitions::store_active_credential_dual(
        &store,
        &format!("server_{}", profile_name),
        &value.to_string(),
    )
    .map_err(|e| format!("Failed to save credentials: {}", e))?;

    info!("Saved credentials for profile: {}", profile_name);
    Ok(())
}

// ============ Universal Credential Vault Commands ============

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VaultCategories {
    server_credentials: u32,
    server_profiles: u32,
    ai_keys: u32,
    oauth_tokens: u32,
    config_entries: u32,
}

#[derive(Serialize)]
struct CredentialStoreStatus {
    master_mode: bool,
    is_locked: bool,
    vault_exists: bool,
    accounts_count: u32,
    timeout_seconds: u64,
    categories: Option<VaultCategories>,
}

#[tauri::command]
async fn init_credential_store() -> Result<String, String> {
    credential_store::CredentialStore::init()
        .map_err(|e| format!("Failed to initialize credential vault: {}", e))
}

#[tauri::command]
async fn unlock_auto_keyring_credential_store(
    totp_code: String,
    state: State<'_, master_password::MasterPasswordState>,
    totp_state: State<'_, totp::TotpState>,
) -> Result<(), String> {
    let (vault_path, mut vault_key) =
        credential_store::CredentialStore::verify_auto_keyring().map_err(|e| e.to_string())?;
    let totp_secret =
        match credential_store::CredentialStore::totp_secret_with_key(&vault_path, &vault_key)
            .map_err(|e| e.to_string())?
        {
            Some(secret) => secret,
            None => {
                vault_key.zeroize();
                return Err("2FA_NOT_ENABLED".to_string());
            }
        };

    if totp_secret.is_empty() {
        vault_key.zeroize();
        return Err("2FA_NOT_ENABLED".to_string());
    }

    let scoped_store =
        credential_store::CredentialStore::from_verified_key(&vault_path, &vault_key);
    if let Err(e) = totp::load_secret_internal_with_store(&totp_state, &totp_secret, &scoped_store)
    {
        vault_key.zeroize();
        return Err(format!("Failed to load TOTP secret: {}", e));
    }
    let valid = match totp::verify_internal_with_store(&totp_state, &totp_code, &scoped_store) {
        Ok(valid) => valid,
        Err(e) => {
            vault_key.zeroize();
            state.set_locked(true);
            return Err(e);
        }
    };
    if !valid {
        vault_key.zeroize();
        state.set_locked(true);
        return Err("2FA_INVALID".to_string());
    }

    credential_store::CredentialStore::cache_vault(vault_path, vault_key);
    vault_key.zeroize();
    state.set_locked(false);
    state.update_activity();
    Ok(())
}

#[tauri::command]
async fn bootstrap_master_credential_store(
    password: String,
    timeout_seconds: u32,
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    credential_store::CredentialStore::bootstrap_master_password(&password)
        .map_err(|e| e.to_string())?;
    let secs = timeout_seconds as u64;
    state.set_timeout(secs);
    persist_auto_lock_timeout(secs).ok();
    state.set_locked(false);
    state.update_activity();
    Ok(())
}

/// B3: whether to offer the first-run Flatpak host-config import, and the paths.
/// `available` is true only inside a Flatpak sandbox, when a native
/// `~/.config/aeroftp` exists, and when the user has not already decided.
#[tauri::command]
fn flatpak_config_import_status() -> serde_json::Value {
    let status = portable::flatpak_host_import_status();
    serde_json::json!({
        "available": status.available,
        "source": status.source.map(|p| p.to_string_lossy().into_owned()),
        "target": status.target.map(|p| p.to_string_lossy().into_owned()),
    })
}

/// B3: apply (`accept = true`) or decline (`accept = false`) the host-config
/// import. Accept copies the native config into the sandbox with copy-only,
/// never-overwrite semantics; either way the decision is recorded so the prompt
/// is shown once. The vault is copied encrypted and still needs the master
/// password to unlock.
#[tauri::command]
fn flatpak_config_import_apply(accept: bool) -> Result<serde_json::Value, String> {
    let report = portable::flatpak_host_import_apply(accept)?;
    Ok(serde_json::json!({
        "imported": report.imported,
        "source": report.source.map(|p| p.to_string_lossy().into_owned()),
        "target": report.target.map(|p| p.to_string_lossy().into_owned()),
    }))
}

#[tauri::command]
async fn get_credential_store_status(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<CredentialStoreStatus, String> {
    let vault_exists = credential_store::CredentialStore::vault_exists();
    let master_mode = credential_store::CredentialStore::is_master_mode();
    let is_locked = state.is_locked();

    // Load persisted timeout on first status check (after restart)
    if master_mode && state.get_timeout() == 0 {
        let persisted = load_persisted_timeout();
        if persisted > 0 {
            state.set_timeout(persisted);
        }
    }

    let (accounts_count, categories) = credential_store::CredentialStore::from_cache()
        .and_then(|store| store.list_accounts().ok())
        .map(|accounts| {
            let cats = keystore_export::categorize_accounts(&accounts);
            let count = accounts.len() as u32;
            (
                count,
                Some(VaultCategories {
                    server_credentials: cats.server_credentials,
                    server_profiles: cats.server_profiles,
                    ai_keys: cats.ai_keys,
                    oauth_tokens: cats.oauth_tokens,
                    config_entries: cats.config_entries,
                }),
            )
        })
        .unwrap_or((0, None));

    Ok(CredentialStoreStatus {
        master_mode,
        is_locked,
        vault_exists,
        accounts_count,
        timeout_seconds: state.get_timeout(),
        categories,
    })
}

#[tauri::command]
async fn store_credential(account: String, password: String) -> Result<(), String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;
    // Dual-write. The vault is written for every key (source of truth +
    // fallback); `server_*` (MUV-3) and `ai_apikey_*` (MUV-5) keys are also
    // mirrored into the active user's partition by the prefix classifier. OAuth
    // and GitHub tokens stay vault-only on this generic path: they are mirrored
    // by their own type-explicit call-sites (MUV-4/5).
    user_partitions::store_active_credential_dual(&store, &account, &password)
        .map_err(|e| format!("Failed to store credential: {}", e))
}

#[tauri::command]
async fn get_credential(account: String) -> Result<String, String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;
    store
        .get(&account)
        .map_err(|e| format!("Failed to get credential: {}", e))
}

#[tauri::command]
async fn delete_credential(account: String) -> Result<(), String> {
    let store = credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "STORE_NOT_READY".to_string())?;
    // Dual-delete. Removes from the vault and, for the prefix-classified keys
    // (`server_*`, `ai_apikey_*`), best-effort from the active user's partition.
    // Not a mass purge (that is MUV-6).
    user_partitions::delete_active_credential_dual(&store, &account)
        .map_err(|e| format!("Failed to delete credential: {}", e))
}

#[tauri::command]
async fn unlock_credential_store(
    password: String,
    totp_code: Option<String>,
    state: State<'_, master_password::MasterPasswordState>,
    totp_state: State<'_, totp::TotpState>,
) -> Result<(), String> {
    // Step 0: Check throttle (M69: brute-force protection)
    if let Err(wait_secs) = state.check_throttle() {
        return Err(format!("THROTTLED:{}", wait_secs));
    }

    // A2-08: Step 1: Verify master password WITHOUT caching vault key.
    // The vault key is only cached after TOTP verification succeeds.
    let (vault_path, mut vault_key) =
        match credential_store::CredentialStore::verify_master(&password) {
            Ok(result) => {
                state.reset_throttle();
                result
            }
            Err(e) => {
                state.record_failed_attempt();
                return Err(e.to_string());
            }
        };

    let totp_secret =
        match credential_store::CredentialStore::totp_secret_with_key(&vault_path, &vault_key) {
            Ok(secret) => secret,
            Err(e) => {
                vault_key.zeroize();
                return Err(e.to_string());
            }
        };

    if let Some(secret) = totp_secret.as_ref().filter(|secret| !secret.is_empty()) {
        let scoped_store =
            credential_store::CredentialStore::from_verified_key(&vault_path, &vault_key);
        // TOTP is enabled: load secret into state and verify code
        if let Err(e) = totp::load_secret_internal_with_store(&totp_state, secret, &scoped_store) {
            vault_key.zeroize();
            state.set_locked(true);
            return Err(format!("Failed to load TOTP secret: {}", e));
        }

        match totp_code {
            Some(ref code) if !code.is_empty() => {
                let valid = match totp::verify_internal_with_store(&totp_state, code, &scoped_store)
                {
                    Ok(valid) => valid,
                    Err(e) => {
                        vault_key.zeroize();
                        state.set_locked(true);
                        return Err(e);
                    }
                };
                if !valid {
                    vault_key.zeroize();
                    state.set_locked(true);
                    return Err("2FA_INVALID".to_string());
                }
            }
            _ => {
                // No TOTP code provided but 2FA is enabled
                vault_key.zeroize();
                state.set_locked(true);
                return Err("2FA_REQUIRED".to_string());
            }
        }
    }

    // A2-08: Cache the vault key only after password and any required TOTP pass.
    credential_store::CredentialStore::cache_vault(vault_path, vault_key);
    vault_key.zeroize();
    state.set_locked(false);
    state.update_activity();
    Ok(())
}

#[tauri::command]
async fn lock_credential_store(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    credential_store::CredentialStore::lock();
    state.set_locked(true);
    Ok(())
}

#[tauri::command]
async fn enable_master_password(
    password: String,
    timeout_seconds: u32,
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    credential_store::CredentialStore::enable_master_password(&password)
        .map_err(|e| e.to_string())?;
    let secs = timeout_seconds as u64;
    state.set_timeout(secs);
    persist_auto_lock_timeout(secs).ok(); // best-effort persist
    state.update_activity();
    Ok(())
}

#[tauri::command]
async fn disable_master_password(
    password: String,
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    credential_store::CredentialStore::disable_master_password(&password)
        .map_err(|e| e.to_string())?;
    state.set_locked(false);
    state.set_timeout(0);
    Ok(())
}

#[tauri::command]
async fn change_master_password(old_password: String, new_password: String) -> Result<(), String> {
    credential_store::CredentialStore::change_master_password(&old_password, &new_password)
        .map_err(|e| e.to_string())
}

/// Persist auto-lock timeout to config file (not a secret, plain text)
fn persist_auto_lock_timeout(seconds: u64) -> Result<(), String> {
    let config_dir = portable::aeroftp_data_root().ok_or("Cannot find AeroFTP data root")?;
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join("auto_lock_timeout"), seconds.to_string())
        .map_err(|e| e.to_string())
}

/// Load persisted auto-lock timeout from config file
fn load_persisted_timeout() -> u64 {
    portable::aeroftp_data_root()
        .map(|d| d.join("auto_lock_timeout"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

#[tauri::command]
async fn set_auto_lock_timeout(
    timeout_seconds: u32,
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    let secs = timeout_seconds as u64;
    state.set_timeout(secs);
    persist_auto_lock_timeout(secs)?;
    Ok(())
}

#[tauri::command]
async fn app_master_password_status(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<master_password::MasterPasswordStatus, String> {
    Ok(master_password::MasterPasswordStatus::new(&state))
}

#[tauri::command]
async fn app_master_password_update_activity(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<(), String> {
    state.update_activity();
    Ok(())
}

#[tauri::command]
async fn app_master_password_check_timeout(
    state: State<'_, master_password::MasterPasswordState>,
) -> Result<bool, String> {
    Ok(state.check_timeout())
}

// ============ Profile Export/Import ============

/// Map a saved-server protocol identifier to the `OAuthProvider` slug used
/// in the vault key (`oauth_<slug>_<profile_id>`). Returns `None` for
/// protocols that do not use OAuth2 storage. Issue #214.
///
/// `pub(crate)` so the generic bridge dispatch (`bridge_commands`) can persist
/// rclone's per-profile OAuth/Jotta tokens with the same keys, now that rclone
/// import is served through the unified bridge (APPENDIX-BRIDGE-CONVERGENCE).
pub(crate) fn oauth_vault_slug_for_protocol(protocol: &str) -> Option<&'static str> {
    match protocol.to_lowercase().as_str() {
        "googledrive" | "google_drive" | "google" => Some("google"),
        "googlephotos" | "google_photos" => Some("googlephotos"),
        "dropbox" => Some("dropbox"),
        "onedrive" | "microsoft" => Some("onedrive"),
        "box" => Some("box"),
        "pcloud" => Some("pcloud"),
        "zohoworkdrive" | "zoho_workdrive" | "zoho" => Some("zohoworkdrive"),
        "yandexdisk" | "yandex_disk" | "yandex" => Some("yandexdisk"),
        _ => None,
    }
}

/// Read the per-profile OAuth or Jotta token blob for a server profile, with
/// a one-shot fallback to the legacy singleton key. The export bundle copies
/// the value verbatim so the destination device can write it back without
/// re-parsing. Issue #214.
fn collect_provider_secrets_for_server(
    store: &credential_store::CredentialStore,
    server: &profile_export::ServerProfileExport,
) -> profile_export::ProviderSecrets {
    let mut out = profile_export::ProviderSecrets::default();
    let protocol = server.protocol.as_deref().unwrap_or("").to_lowercase();

    // MUV-4: read the per-profile token from the active user's partition (vault
    // fallback inside resolve), then the legacy singleton key from the vault.
    if let Some(slug) = oauth_vault_slug_for_protocol(&protocol) {
        let per_profile = format!("oauth_{}_{}", slug, server.id);
        if let Ok(Some(value)) = user_partitions::resolve_active_credential(store, &per_profile) {
            out.oauth = Some(value.to_string());
        } else {
            // Legacy singleton key path: only honoured when nothing has been
            // migrated yet for this provider on this device.
            let legacy = format!("oauth_{}", slug);
            if let Ok(value) = store.get(&legacy) {
                out.oauth = Some(value);
            }
        }
    }

    if protocol == "jottacloud" {
        let per_profile = format!("jottacloud_refresh_{}", server.id);
        if let Ok(Some(value)) = user_partitions::resolve_active_credential(store, &per_profile) {
            out.jotta_refresh = Some(value.to_string());
        } else if let Ok(value) = store.get("jottacloud_refresh") {
            out.jotta_refresh = Some(value);
        }
    }

    // Bundle the BYO OAuth app `client_id`/`client_secret` (global vault
    // singletons `oauth_<slug>_client_id`/`_client_secret`, one per provider) so
    // an imported OAuth profile can reconnect without re-entering the app
    // credentials. They are app-level, not per-profile, but an OAuth profile
    // cannot work without them, so they travel with every OAuth profile in the
    // export (the struct fields already exist for the #128-D rclone-import
    // recovery path). The client-cred slug differs from the per-profile token
    // slug: `rclone_oauth_client_cred_key` maps Google Photos onto Google Drive's
    // app and gates the providers that have no BYO app (jottacloud/zoho).
    if let Some(cred_slug) = crate::bridge_commands::rclone_oauth_client_cred_key(&protocol) {
        if let Ok(id) = store.get(&format!("oauth_{}_client_id", cred_slug)) {
            if !id.is_empty() {
                out.oauth_client_id = Some(id);
            }
        }
        if let Ok(secret) = store.get(&format!("oauth_{}_client_secret", cred_slug)) {
            if !secret.is_empty() {
                out.oauth_client_secret = Some(secret);
            }
        }
    }

    // CWP-20B: bundle the crypt-overlay secrets (BOTH kinds — native AeroCrypt
    // and interop rclone-crypt share these generic vault keys) so an exported
    // Crypt profile reconnects on import without re-entering the overlay password.
    // Protocol-agnostic: the overlay can sit on any provider-API backend.
    if let Ok(Some(pw)) = user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_pw_{}", server.id),
    ) {
        out.aerocrypt_overlay_pw = Some(pw.to_string());
    }
    if let Ok(Some(salt)) = user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_salt_{}", server.id),
    ) {
        out.aerocrypt_overlay_salt = Some(salt.to_string());
    }
    // AeroCrypt keyfile PATH (not the keyfile contents). Lets a same-device
    // re-import find the keyfile without re-picking it; on another device the
    // path may be stale and the UI must re-point.
    if let Ok(Some(kf_path)) = user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_keyfile_path_{}", server.id),
    ) {
        out.aerocrypt_overlay_keyfile_path = Some(kf_path.to_string());
    }
    if let Ok(Some(config)) = user_partitions::resolve_active_credential(
        store,
        &format!("aerocrypt_overlay_config_{}", server.id),
    ) {
        out.aerocrypt_overlay_config = Some(config.to_string());
    }

    // Issue #215 Caveat A: bundle the per-protocol credential snapshots so a
    // multi-protocol account keeps each mode's saved credentials on import. The
    // `server_` prefix routes this through the active user's partition exactly
    // like the per-profile password (modeCredentialStore already stripped the
    // single-use TOTP/STS codes at write time).
    if let Ok(Some(modes)) =
        user_partitions::resolve_active_credential(store, &format!("server_modes_{}", server.id))
    {
        out.mode_credentials = Some(modes.to_string());
    }

    // Issue #230: bundle the per-profile Filen CLI API key (`filen_api_key_<id>`).
    // It is a long-lived secret kept only in the vault (never on the saved
    // profile's `options`), so without this the `.aeroftp` export dropped it and a
    // re-imported Filen profile fell back to password + TOTP.
    if let Ok(Some(api_key)) =
        user_partitions::resolve_active_credential(store, &format!("filen_api_key_{}", server.id))
    {
        out.filen_api_key = Some(api_key.to_string());
    }

    out
}

#[tauri::command]
async fn export_server_profiles(
    servers_json: String,
    password: String,
    include_credentials: bool,
    file_path: String,
) -> Result<profile_export::ExportMetadata, String> {
    export_server_profiles_core(servers_json, password, include_credentials, file_path).await
}

/// Core of [`export_server_profiles`], callable outside the Tauri command
/// surface. The `aeroftp-cli profile-export` path reuses it verbatim so the CLI
/// and GUI share ONE credential-collection + encryption implementation (a
/// `#[tauri::command]` cannot be `pub` without a macro-name clash, hence the
/// split).
pub async fn export_server_profiles_core(
    servers_json: String,
    password: String,
    include_credentials: bool,
    file_path: String,
) -> Result<profile_export::ExportMetadata, String> {
    let mut servers: Vec<profile_export::ServerProfileExport> =
        serde_json::from_str(&servers_json).map_err(|e| format!("Invalid server data: {}", e))?;

    let mut provider_secrets: std::collections::HashMap<String, profile_export::ProviderSecrets> =
        std::collections::HashMap::new();

    // Fetch credentials from secure store if requested
    if include_credentials {
        match credential_store::CredentialStore::from_cache() {
            Some(store) => {
                for server in &mut servers {
                    // MUV-3: export the active user's per-user credential, with
                    // fallback to the legacy vault.
                    if let Ok(Some(cred)) = user_partitions::resolve_active_credential(
                        &store,
                        &format!("server_{}", server.id),
                    ) {
                        server.credential = Some(cred.to_string());
                    }
                    // Issue #214: bundle OAuth / Jotta tokens alongside the
                    // per-profile password so an import on a fresh device
                    // reconnects without a browser re-auth.
                    let secrets = collect_provider_secrets_for_server(&store, server);
                    if secrets.oauth.is_some()
                        || secrets.jotta_refresh.is_some()
                        || secrets.aerocrypt_overlay_pw.is_some()
                        || secrets.aerocrypt_overlay_salt.is_some()
                        || secrets.aerocrypt_overlay_keyfile_path.is_some()
                        || secrets.aerocrypt_overlay_config.is_some()
                        || secrets.mode_credentials.is_some()
                        || secrets.oauth_client_id.is_some()
                        || secrets.oauth_client_secret.is_some()
                        || secrets.filen_api_key.is_some()
                    {
                        provider_secrets.insert(server.id.clone(), secrets);
                    }
                }
            }
            None => {
                log::warn!("Export: vault not ready, credentials will not be included");
            }
        }
    }

    profile_export::export_profiles(
        servers,
        provider_secrets,
        &password,
        std::path::Path::new(&file_path),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn import_server_profiles(
    file_path: String,
    password: String,
) -> Result<serde_json::Value, String> {
    import_server_profiles_core(file_path, password).await
}

/// Core of [`import_server_profiles`], callable outside the Tauri command
/// surface. Reused verbatim by `aeroftp-cli profile-import` so the
/// decrypt + per-secret vault restore (including the #215 per-protocol
/// snapshots) lives in one place. Restores credentials for every profile in
/// the file (the GUI computes the profile-list merge separately).
pub async fn import_server_profiles_core(
    file_path: String,
    password: String,
) -> Result<serde_json::Value, String> {
    import_server_profiles_core_filtered(file_path, password, None).await
}

/// Like [`import_server_profiles_core`] but, when `restore_only` is `Some`, only
/// the profile ids in the set have their credentials restored into the vault.
///
/// Audit v4.1.0 (CLI profile-import): the importer restored every secret in the
/// file BEFORE the caller decided which profiles to skip as duplicates, so a
/// profile reported as "skipped (already present)" silently had its existing
/// vault credential overwritten by the one from the file. Callers that dedup the
/// profile list (the CLI) pass the set of ids they actually add so the skip
/// decision and the credential restore stay consistent. `None` preserves the
/// original "restore all" behaviour for the GUI command.
pub async fn import_server_profiles_core_filtered(
    file_path: String,
    password: String,
    restore_only: Option<std::collections::HashSet<String>>,
) -> Result<serde_json::Value, String> {
    let (servers, provider_secrets, metadata) =
        profile_export::import_profiles(std::path::Path::new(&file_path), &password)
            .map_err(|e| e.to_string())?;

    // Store credentials in secure store
    let mut cred_errors: Vec<String> = Vec::new();
    // CWP-20B: track which profiles actually had their crypt-overlay secret
    // restored, so the redacted `hasStoredAeroCrypt*` flags returned to the UI
    // reflect reality (a binding can round-trip without its secret when the
    // export excluded credentials or the vault is unavailable).
    let mut restored_aerocrypt_pw: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut restored_aerocrypt_salt: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut restored_aerocrypt_keyfile_path: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut restored_aerocrypt_config: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // Issue #230: track which profiles had their Filen CLI API key restored so
    // the `hasStoredFilenApiKey` flag returned to the UI reflects reality.
    let mut restored_filen_api_key: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    match credential_store::CredentialStore::from_cache() {
        Some(store) => {
            for server in &servers {
                // Audit v4.1.0: when the caller deduped the profile list, only
                // restore credentials for the ids it actually added, so a
                // "skipped" profile never has its existing credential clobbered.
                if restore_only
                    .as_ref()
                    .is_some_and(|allow| !allow.contains(&server.id))
                {
                    continue;
                }
                if let Some(ref cred) = server.credential {
                    // MUV-3: dual-write the imported credential (vault + active
                    // user's partition).
                    if let Err(e) = user_partitions::store_active_credential_dual(
                        &store,
                        &format!("server_{}", server.id),
                        cred,
                    ) {
                        cred_errors.push(format!("{}: {}", server.id, e));
                    }
                }
            }
            // Issue #214: restore provider tokens under the per-profile vault
            // key. The protocol identifier comes from the matching server
            // entry: when the same id is missing from `servers` (corrupted
            // export) we silently skip, which preserves the rule that
            // tokens have no meaning without the profile they belong to.
            let protocol_by_id: std::collections::HashMap<&str, String> = servers
                .iter()
                .map(|s| {
                    (
                        s.id.as_str(),
                        s.protocol.clone().unwrap_or_default().to_lowercase(),
                    )
                })
                .collect();
            for (profile_id, secrets) in &provider_secrets {
                // Same dedup-consistency gate as the server_<id> loop above.
                if restore_only
                    .as_ref()
                    .is_some_and(|allow| !allow.contains(profile_id))
                {
                    continue;
                }
                let protocol = match protocol_by_id.get(profile_id.as_str()) {
                    Some(p) => p,
                    None => continue,
                };
                if let Some(ref oauth_json) = secrets.oauth {
                    if let Some(slug) = oauth_vault_slug_for_protocol(protocol) {
                        let key = format!("oauth_{}_{}", slug, profile_id);
                        // MUV-4: dual-write into vault + active user's partition.
                        if let Err(e) = user_partitions::store_active_credential_typed_dual(
                            &store, &key, "oauth", oauth_json,
                        ) {
                            cred_errors.push(format!("{} oauth: {}", profile_id, e));
                        }
                    }
                }
                if let Some(ref jotta_json) = secrets.jotta_refresh {
                    if protocol == "jottacloud" {
                        let key = format!("jottacloud_refresh_{}", profile_id);
                        if let Err(e) = user_partitions::store_active_credential_typed_dual(
                            &store,
                            &key,
                            "jottacloud_refresh",
                            jotta_json,
                        ) {
                            cred_errors.push(format!("{} jotta: {}", profile_id, e));
                        }
                    }
                }
                // Restore the BYO OAuth app client_id/client_secret into their
                // global vault singletons (`oauth_<slug>_client_id/secret`) so an
                // imported OAuth profile reconnects without re-entering the app
                // credentials (the export side gathers them in
                // `collect_provider_secrets_for_server`). Written under the raw
                // client-cred slug, which aliases Google Photos onto Google Drive
                // and matches the key the GUI/edit form reads.
                if secrets.oauth_client_id.is_some() || secrets.oauth_client_secret.is_some() {
                    if let Some(cred_slug) = bridge_commands::rclone_oauth_client_cred_key(protocol)
                    {
                        if let Some(ref id) = secrets.oauth_client_id {
                            if let Err(e) =
                                store.store(&format!("oauth_{}_client_id", cred_slug), id)
                            {
                                cred_errors.push(format!("{} oauth client_id: {}", profile_id, e));
                            }
                        }
                        if let Some(ref secret) = secrets.oauth_client_secret {
                            if let Err(e) =
                                store.store(&format!("oauth_{}_client_secret", cred_slug), secret)
                            {
                                cred_errors
                                    .push(format!("{} oauth client_secret: {}", profile_id, e));
                            }
                        }
                    }
                }
                // CWP-20B: restore the crypt-overlay secrets under the same
                // generic vault keys the connect path reads (App.tsx
                // get_credential aerocrypt_overlay_pw_/salt_<id>). Protocol-
                // agnostic and shared by both overlay kinds (AeroCrypt + rclone).
                if let Some(ref pw) = secrets.aerocrypt_overlay_pw {
                    let key = format!("aerocrypt_overlay_pw_{}", profile_id);
                    match user_partitions::store_active_credential_dual(&store, &key, pw) {
                        Ok(()) => {
                            restored_aerocrypt_pw.insert(profile_id.clone());
                        }
                        Err(e) => cred_errors.push(format!("{} aerocrypt pw: {}", profile_id, e)),
                    }
                }
                if let Some(ref salt) = secrets.aerocrypt_overlay_salt {
                    let key = format!("aerocrypt_overlay_salt_{}", profile_id);
                    match user_partitions::store_active_credential_dual(&store, &key, salt) {
                        Ok(()) => {
                            restored_aerocrypt_salt.insert(profile_id.clone());
                        }
                        Err(e) => cred_errors.push(format!("{} aerocrypt salt: {}", profile_id, e)),
                    }
                }
                // Restore the AeroCrypt keyfile PATH (not the keyfile itself). On a
                // different device this path may be stale; the UI validates it and
                // re-points before unlock. A keyfile vault still fails closed if the
                // keyfile is missing, so restoring a stale path is never unsafe.
                if let Some(ref kf_path) = secrets.aerocrypt_overlay_keyfile_path {
                    let key = format!("aerocrypt_overlay_keyfile_path_{}", profile_id);
                    match user_partitions::store_active_credential_dual(&store, &key, kf_path) {
                        Ok(()) => {
                            restored_aerocrypt_keyfile_path.insert(profile_id.clone());
                        }
                        Err(e) => cred_errors
                            .push(format!("{} aerocrypt keyfile path: {}", profile_id, e)),
                    }
                }
                if let Some(ref config) = secrets.aerocrypt_overlay_config {
                    let key = format!("aerocrypt_overlay_config_{}", profile_id);
                    match user_partitions::store_active_credential_dual(&store, &key, config) {
                        Ok(()) => {
                            restored_aerocrypt_config.insert(profile_id.clone());
                        }
                        Err(e) => {
                            cred_errors.push(format!("{} aerocrypt config: {}", profile_id, e))
                        }
                    }
                }
                // Issue #215 Caveat A: restore the per-protocol snapshots under
                // the same `server_modes_<id>` key the connect/edit path reads
                // (modeCredentialStore get/store_credential). The `server_`
                // prefix dual-writes into the active user's partition, matching
                // how the snapshots were originally saved.
                if let Some(ref modes) = secrets.mode_credentials {
                    let key = format!("server_modes_{}", profile_id);
                    if let Err(e) =
                        user_partitions::store_active_credential_dual(&store, &key, modes)
                    {
                        cred_errors.push(format!("{} mode creds: {}", profile_id, e));
                    }
                }
                // Issue #230: restore the Filen CLI API key under the same
                // `filen_api_key_<id>` vault key the connect path reads, so an
                // imported Filen profile skips the /v3/login + TOTP window again.
                if let Some(ref api_key) = secrets.filen_api_key {
                    let key = format!("filen_api_key_{}", profile_id);
                    match user_partitions::store_active_credential_dual(&store, &key, api_key) {
                        Ok(()) => {
                            restored_filen_api_key.insert(profile_id.clone());
                        }
                        Err(e) => cred_errors.push(format!("{} filen api key: {}", profile_id, e)),
                    }
                }
            }
        }
        None => {
            // Vault not ready: credentials cannot be stored
            let cred_count = servers.iter().filter(|s| s.credential.is_some()).count();
            if cred_count > 0 {
                cred_errors.push(format!(
                    "Vault not ready, {} credentials not stored",
                    cred_count
                ));
            }
            let token_count = provider_secrets
                .values()
                .filter(|s| s.oauth.is_some() || s.jotta_refresh.is_some())
                .count();
            if token_count > 0 {
                cred_errors.push(format!(
                    "Vault not ready, {} provider tokens not stored",
                    token_count
                ));
            }
        }
    }
    if !cred_errors.is_empty() {
        log::warn!("Import credential issues: {:?}", cred_errors);
    }

    // H16 fix: Redact credentials before returning to renderer.
    // Only return non-sensitive fields + a boolean flag indicating stored credentials.
    let redacted_servers: Vec<serde_json::Value> = servers
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "name": s.name,
                "host": s.host,
                "port": s.port,
                "username": s.username,
                "protocol": s.protocol,
                "initialPath": s.initial_path,
                "localInitialPath": s.local_initial_path,
                "color": s.color,
                "lastConnected": s.last_connected,
                "options": s.options,
                "providerId": s.provider_id,
                // Top-level ServerProfile fields that live OUTSIDE `options` and
                // so must be re-emitted explicitly or the import drops them:
                // the share-link base (GAP 2), the custom/detected icons (GAP 3)
                // and the silenced classic-fallback modal preference (GAP 4).
                "publicUrlBase": s.public_url_base,
                "customIconUrl": s.custom_icon_url,
                "faviconUrl": s.favicon_url,
                "skipDeltaEligibilityPrompt": s.skip_delta_eligibility_prompt,
                "hasStoredCredential": s.credential.is_some(),
                // Issue #230: reflect whether the Filen CLI API key was actually
                // restored so the profile reconnects via the key (skipping the
                // TOTP window) instead of falling back to password + 2FA.
                "hasStoredFilenApiKey": restored_filen_api_key.contains(&s.id),
                // CWP-20B: re-import a Crypt profile AS a Crypt profile (both
                // kinds). The flags reflect what was actually restored, not the
                // source state, so the UI prompts for the overlay password only
                // when the secret did not travel with the export.
                "aeroCryptOverlay": s.aero_crypt_overlay,
                "hasStoredAeroCryptPassword": restored_aerocrypt_pw.contains(&s.id),
                "hasStoredAeroCryptSalt": restored_aerocrypt_salt.contains(&s.id),
                // A keyfile PATH may have been restored; the UI validates it exists
                // (it can be stale on a different device) and re-points before unlock.
                "hasStoredAeroCryptKeyfilePath": restored_aerocrypt_keyfile_path.contains(&s.id),
                "hasStoredAeroCryptConfig": restored_aerocrypt_config.contains(&s.id),
                // Issue #215 Caveat A: surface the opt-in so the imported profile
                // re-hydrates per-mode snapshots. It is a user preference, not a
                // secret-presence flag, so it round-trips verbatim: ConnectionScreen
                // only reads `server_modes_<id>` when this is true, and finds the
                // creds restored above (or prompts normally if the export omitted them).
                "persistModeCredentials": s.persist_mode_credentials,
            })
        })
        .collect();

    let result = serde_json::json!({
        "servers": redacted_servers,
        "metadata": metadata,
    });
    Ok(result)
}

#[tauri::command]
async fn read_export_metadata(file_path: String) -> Result<profile_export::ExportMetadata, String> {
    profile_export::read_metadata(std::path::Path::new(&file_path)).map_err(|e| e.to_string())
}

// ============ Full Keystore Export/Import ============

#[tauri::command]
async fn export_keystore(
    app: tauri::AppHandle,
    password: String,
    file_path: String,
    // Optional v2 controls. Defaulting `mode` to "full" means the new
    // backup contract kicks in immediately for v3.7.8+ frontends; older
    // builds that pass nothing still get the safer default. The frontend
    // can force "vault_only" to recover the legacy slim-export behaviour.
    mode: Option<String>,
    local_storage: Option<std::collections::HashMap<String, String>>,
) -> Result<keystore_export::KeystoreMetadata, String> {
    let mode = match mode.as_deref() {
        None => keystore_export::ExportMode::Full,
        Some(s) => s
            .parse::<keystore_export::ExportMode>()
            .map_err(|e| e.to_string())?,
    };
    let config_dir = portable::app_config_dir(&app).ok();
    // AUDIT 2026-05-11 M1: Argon2id (128 MiB / t=4 / p=4) takes
    // ~1-2 s of single-thread CPU. Running it on the Tauri async
    // worker thread blocks every other in-flight async command and
    // freezes the UI. Wrap the synchronous body in spawn_blocking so
    // the worker stays responsive.
    let file_path_owned = file_path.clone();
    tokio::task::spawn_blocking(move || {
        keystore_export::export_keystore(
            &password,
            std::path::Path::new(&file_path_owned),
            mode,
            keystore_export::KeystoreScope::AllUsers,
            config_dir.as_deref(),
            local_storage,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("export_keystore join error: {e}"))?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn import_keystore(
    app: tauri::AppHandle,
    password: String,
    file_path: String,
    merge_strategy: String,
    // v2 selectivity. All four flags default to true when missing so a
    // pre-v3.7.8 caller still gets "import everything in the file". The
    // import dialog passes explicit booleans when the user toggles a
    // section off.
    import_vault: Option<bool>,
    import_sqlite: Option<bool>,
    import_files: Option<bool>,
    import_local_storage: Option<bool>,
) -> Result<keystore_export::KeystoreImportResult, String> {
    let progress_app = app.clone();
    let progress_cb = move |phase: &str, current: u32, total: u32| {
        let _ = progress_app.emit(
            "keystore-import-progress",
            serde_json::json!({
                "phase": phase,
                "current": current,
                "total": total,
            }),
        );
    };
    let sections = keystore_export::ImportSections {
        vault: import_vault.unwrap_or(true),
        sqlite_dbs: import_sqlite.unwrap_or(true),
        files: import_files.unwrap_or(true),
        local_storage: import_local_storage.unwrap_or(true),
    };
    let config_dir = portable::app_config_dir(&app).ok();
    // AUDIT 2026-05-11 M1: same reasoning as export_keystore.
    // Argon2id + zstd decompress + potentially-large disk writes all
    // run on a blocking worker so the async runtime stays free.
    // `progress_cb` captures the cloned AppHandle and emits Tauri
    // events from inside spawn_blocking, which is fine because
    // AppHandle is Send + Sync.
    let file_path_owned = file_path.clone();
    let merge_strategy_owned = merge_strategy.clone();
    let result = tokio::task::spawn_blocking(move || {
        keystore_export::import_keystore(
            &password,
            std::path::Path::new(&file_path_owned),
            &merge_strategy_owned,
            sections,
            config_dir.as_deref(),
            Some(&progress_cb),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("import_keystore join error: {e}"))??;
    // AUDIT 2026-05-11 C2: bubble the restart hint to the frontend
    // via a dedicated event in addition to the field on the return
    // value. Two channels are cheap and let the import dialog
    // surface a "Restart required" banner the moment the result
    // lands, without depending on whether the user looked at the
    // toast text.
    if result.requires_restart {
        let _ = app.emit(
            "keystore-import-requires-restart",
            serde_json::json!({
                "sqlite_dbs_restored": result.sqlite_dbs_restored,
                "files_restored": result.files_restored,
            }),
        );
    }
    Ok(result)
}

#[tauri::command]
async fn read_keystore_metadata(
    file_path: String,
) -> Result<keystore_export::KeystoreMetadata, String> {
    keystore_export::read_keystore_metadata(std::path::Path::new(&file_path))
        .map_err(|e| e.to_string())
}

// ============ Public wrappers for AI tool access ============
// Cannot make #[tauri::command] functions pub (Tauri 2 macro conflict),
// so we expose thin wrappers that ai_tools.rs can call via crate::

pub async fn compress_files_core(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,
    compression_level: Option<i64>,
) -> Result<String, String> {
    compress_files_impl(paths, output_path, password, compression_level, None).await
}

pub async fn extract_archive_core(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
    password: Option<String>,
) -> Result<String, String> {
    extract_archive(archive_path, output_dir, create_subfolder, password).await
}

pub async fn compress_7z_core(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,
    compression_level: Option<i64>,
    encrypt_header: Option<bool>,
    advanced: Option<SevenZAdvanced>,
) -> Result<String, String> {
    compress_7z_impl(
        paths,
        output_path,
        password,
        compression_level,
        encrypt_header,
        advanced,
        None,
    )
    .await
}

pub async fn extract_7z_core(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<String, String> {
    extract_7z(archive_path, output_dir, password, create_subfolder).await
}

pub async fn compress_tar_core(
    paths: Vec<String>,
    output_path: String,
    format: String,
    compression_level: Option<i64>,
) -> Result<String, String> {
    // `compress_tar` returns a human-readable message for the GUI toast, but the
    // CLI layer needs the actual output path (it stats it for `output_bytes`).
    // The tar writer commits to exactly `output_path`, so return that on success,
    // matching the zip/7z `_core` wrappers which already return the path.
    let out = output_path.clone();
    compress_tar_impl(paths, output_path, format, compression_level, None)
        .await
        .map(|_| out)
}

/// Headless standalone gzip/xz/bzip2 compression for the CLI / AI-tool path.
/// Returns the output path on success (the impl already returns it), so the
/// caller can stat it for the reported output size.
pub async fn compress_single_core(
    input_path: String,
    output_path: String,
    format: String,
    compression_level: Option<i64>,
) -> Result<String, String> {
    compress_single_impl(input_path, output_path, format, compression_level, None).await
}

pub async fn extract_tar_core(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
) -> Result<String, String> {
    extract_tar(archive_path, output_dir, create_subfolder).await
}

/// Headless standalone gz/xz/bz2 extraction for the AI-tool path (codec sniffed
/// from the extension), mirroring `extract_tar_core`.
pub async fn extract_single_core(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
) -> Result<String, String> {
    extract_single_impl(archive_path, output_dir, create_subfolder, None).await
}

/// As `extract_single_core`, but the codec is forced by `kind` ("gz" | "xz" |
/// "bz2") instead of sniffed, so the CLI's `--archive-format` is authoritative on
/// extract exactly like `extract_tar_as_core`.
pub async fn extract_single_as_core(
    archive_path: String,
    output_dir: String,
    create_subfolder: bool,
    kind: String,
) -> Result<String, String> {
    extract_single_impl(archive_path, output_dir, create_subfolder, Some(kind)).await
}

pub async fn extract_rar_core(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<String, String> {
    extract_rar(archive_path, output_dir, password, create_subfolder).await
}

// Encryption probes for the CLI fast path: `aeroftp extract` checks these before
// creating any output directory, so an encrypted archive extracted without a
// password fails cleanly (exit 6) instead of leaving an empty destination folder
// behind (Deliverable G: the file-manager "Extract to folder" verb shells the CLI
// first, then falls back to the dedicated password window).
pub async fn is_zip_encrypted_core(archive_path: String) -> Result<bool, String> {
    is_zip_encrypted(archive_path).await
}

pub async fn is_7z_encrypted_core(archive_path: String) -> Result<bool, String> {
    is_7z_encrypted(archive_path).await
}

pub async fn is_rar_encrypted_core(archive_path: String) -> Result<bool, String> {
    is_rar_encrypted(archive_path).await
}

// ============ Mount Manager Commands (T-MOUNT-MANAGER) ============

#[tauri::command]
async fn mount_list() -> Result<serde_json::Value, String> {
    let registry = mount_manager::load_registry();
    let with_status = mount_manager::list_with_status().await;
    Ok(serde_json::json!({
        "storage_mode": registry.storage_mode,
        "mounts": with_status.into_iter().map(|(cfg, status)| {
            serde_json::json!({ "config": cfg, "status": status })
        }).collect::<Vec<_>>(),
    }))
}

#[tauri::command]
async fn mount_save_config(
    config: mount_manager::MountConfig,
) -> Result<mount_manager::MountConfig, String> {
    mount_manager::upsert_config(config)
}

#[tauri::command]
async fn mount_delete_config(id: String) -> Result<(), String> {
    let _ = mount_manager::stop_mount(&id).await;
    mount_manager::delete_config(&id)
}

#[tauri::command]
async fn mount_start(id: String) -> Result<mount_manager::MountStatus, String> {
    mount_manager::start_mount(&id).await
}

#[tauri::command]
async fn mount_stop(id: String) -> Result<(), String> {
    mount_manager::stop_mount(&id).await
}

#[tauri::command]
async fn mount_open_in_explorer(id: String) -> Result<(), String> {
    mount_manager::open_in_explorer(&id).await
}

#[tauri::command]
async fn mount_suggest_path(profile: String) -> Result<String, String> {
    Ok(mount_manager::suggest_mountpoint(&profile))
}

/// Mount an UNLOCKED vault (Cryptomator or `.aerovault`/`.aerozip`) read-only as
/// an ephemeral local filesystem (#322 Deliverable B). The password is forwarded
/// to the `aeroftp-cli mount-vault` child over stdin and never stored; the mount
/// auto-unmounts on vault lock / app quit. `key` is a stable handle (the
/// Cryptomator `vault_id` or the `.aerovault` path) used to stop/open it later.
#[tauri::command]
async fn vault_mount_start(
    key: String,
    kind: String,
    vault_path: String,
    password: String,
    display_name: String,
) -> Result<vault_mount::VaultMountInfo, String> {
    vault_mount::start(key, kind, vault_path, password, display_name).await
}

#[tauri::command]
async fn vault_mount_stop(key: String) -> Result<(), String> {
    vault_mount::stop(&key).await
}

#[tauri::command]
async fn vault_mount_list() -> Result<Vec<vault_mount::VaultMountInfo>, String> {
    Ok(vault_mount::list().await)
}

#[tauri::command]
async fn vault_mount_open(key: String) -> Result<(), String> {
    vault_mount::open_in_file_manager(&key).await
}

#[tauri::command]
async fn mount_pick_drive_letter() -> Result<String, String> {
    mount_manager::pick_free_drive_letter()
}

#[tauri::command]
async fn mount_set_storage_mode(mode: String) -> Result<(), String> {
    let target = match mode.as_str() {
        "vault" => mount_manager::StorageMode::Vault,
        "sidecar" => mount_manager::StorageMode::Sidecar,
        other => return Err(format!("Unknown storage mode: {}", other)),
    };
    mount_manager::switch_storage_mode(target)
}

#[tauri::command]
async fn mount_install_autostart(id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || mount_manager::install_autostart(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn mount_uninstall_autostart(id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || mount_manager::uninstall_autostart(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn mount_autostart_blocked() -> Option<String> {
    mount_manager::autostart_blocked_reason()
}

/// Open Mount: idempotent quick-action used from the dual-panel "Open Mount"
/// button. Starts the mount if stopped, then opens the OS file manager.
#[tauri::command]
async fn mount_open_quick(id: String) -> Result<(), String> {
    // Check if already running.
    let snapshot = mount_manager::list_with_status().await;
    let already = snapshot
        .iter()
        .any(|(c, s)| c.id == id && matches!(s.state, mount_manager::MountState::Running));
    if !already {
        mount_manager::start_mount(&id).await?;
        // Give FUSE/WebDAV a moment to settle.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }
    mount_manager::open_in_explorer(&id).await
}

// ============ App Entry Point ============

/// Install the process-wide rustls `CryptoProvider` for THIS binary.
///
/// Both `aws-lc-rs` and `ring` are pulled into the dependency tree (via
/// different crates: our own choice and `noq-proto`), so rustls cannot
/// auto-select a provider and panics on the first TLS handshake (FTPS, WebDAV
/// over https, ...) unless one is pinned. Each binary target runs in its own
/// process, so each must call this once before any TLS connector is built.
/// Regression `c15ee3f38`: the GUI binary forgot to, so every FTPS connect
/// panicked into an infinite spinner. Centralised here so every binary shares
/// ONE code path and the `crypto_provider_guard` test can guard the class of bug
/// at the root.
///
/// Idempotent: a second call is a no-op (rustls returns `Err` when a provider is
/// already installed, which we ignore). Returns whether a provider is installed
/// after the call, so callers/tests can assert the process is TLS-ready.
pub fn install_crypto_provider() -> bool {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    rustls::crypto::CryptoProvider::get_default().is_some()
}

#[cfg(test)]
mod crypto_provider_guard {
    //! Root-cause guard for the FTPS-hang class of bug (`c15ee3f38`): a binary
    //! that does TLS without installing a rustls provider panics on the first
    //! handshake. Every binary must route through `install_crypto_provider`;
    //! this proves the helper actually leaves the process with a provider
    //! installed and globally visible (so a fresh TLS connector can build).
    #[test]
    fn install_crypto_provider_makes_process_tls_ready() {
        assert!(
            super::install_crypto_provider(),
            "install_crypto_provider must leave a rustls CryptoProvider installed"
        );
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};

    // Pin the rustls CryptoProvider for this (GUI) process before any TLS
    // connector is built. See `install_crypto_provider`.
    debug_assert!(
        install_crypto_provider(),
        "rustls CryptoProvider must be installed before TLS is used"
    );
    #[cfg(not(debug_assertions))]
    let _ = install_crypto_provider();

    // Fix WebKitGTK rendering issues on Linux: disable DMA-BUF renderer
    // which causes canvas/WebGL artifacts in Monaco and xterm.js.
    // Must be set BEFORE any WebKit initialization.
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");

        // Route GTK file/folder pickers through xdg-desktop-portal instead of
        // the in-process GtkFileChooser. Under WebKitGTK the native chooser can
        // corrupt the GLib heap ("malloc(): unaligned fastbin chunk detected",
        // SIGABRT) when the directory picker opens: the debug build aborts on
        // it, and an optimized release can corrupt the heap silently. The portal
        // runs the chooser out-of-process so it never touches our heap, and is
        // the recommended path on Wayland; on a host with no portal GTK falls
        // back to the native chooser. Respect a user override (GTK_USE_PORTAL=0).
        if std::env::var_os("GTK_USE_PORTAL").is_none() {
            std::env::set_var("GTK_USE_PORTAL", "1");
        }

        // Pin a stable GTK program name so the window's WM_CLASS is always
        // "aeroftp", regardless of which path launched the binary. GNOME maps a
        // window to its .desktop entry (and thus its icon) by matching WM_CLASS
        // against `StartupWMClass=aeroftp` in AeroFTP.desktop. A normal launch
        // goes through `Exec=/usr/bin/aeroftp` (WM_CLASS "aeroftp" -> match), but
        // the post-update restart from tauri-plugin-updater relaunches
        // `current_exe()` = /usr/lib/aeroftp/aeroftp.bin, whose default WM_CLASS
        // "aeroftp.bin" does NOT match -> GNOME falls back to a generic icon.
        // Setting prgname here (before any GTK/WebKit init) makes WM_CLASS
        // deterministic and fixes the generic-icon-after-update bug.
        gtk::glib::set_prgname(Some("aeroftp"));
        gtk::glib::set_application_name("AeroFTP");
    }

    // Serve frontend via real HTTP server to fix WebKitGTK rendering issues.
    // In production, Tauri uses tauri:// custom protocol which breaks:
    // - Monaco Editor web workers (no syntax highlighting)
    // - xterm.js canvas renderer (no colors/cursor)
    // - iframe CSS rendering (no styles in HTML preview)
    // By serving via http://localhost, production behaves identically to dev mode.
    //
    // SECURITY NOTE (H26): This serves the frontend over unencrypted HTTP on localhost:14321.
    // This is a known design trade-off required by WebKitGTK on Linux: the tauri:// custom
    // protocol does not support web workers, canvas rendering, or iframe CSS in WebKitGTK.
    // Risk assessment:
    //   - Traffic is loopback-only (127.0.0.1), not exposed on network interfaces
    //   - Exploitation requires same-machine access (local privilege escalation prerequisite)
    //   - All sensitive data (credentials, tokens) flows through Tauri IPC commands, NOT HTTP
    //   - tauri-plugin-localhost is explicitly bound to 127.0.0.1
    // This cannot be changed to HTTPS without a local TLS certificate infrastructure that
    // would add complexity with minimal security benefit for localhost-only traffic.
    //
    // NOTE: This workaround is Linux-only. macOS (WKWebView) and Windows (WebView2) use
    // Tauri's default asset protocol. Applying localhost to macOS caused a frozen/unresponsive
    // UI due to ATS (App Transport Security) blocking plain HTTP in WKWebView.
    // See docs/dev/platform/MACOS-UNIFIED-AUDIT-2026-03-30.md
    #[cfg(target_os = "linux")]
    let port: u16 = 14321;

    let mut builder = tauri::Builder::default();

    #[cfg(target_os = "linux")]
    {
        builder = builder.plugin(
            tauri_plugin_localhost::Builder::new(port)
                .host("127.0.0.1")
                .build(),
        );
    }

    builder = builder
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_log::Builder::default()
                // Global default stays at Info. Trace would otherwise fan the
                // I/O reactor crates (mio, hyper, rustls, h2) into every log
                // sink we register — including the Webview target below, which
                // turns every poll() event into an IPC emit to the frontend.
                // Observed effect: app blocked at startup after the first
                // `mio::poll registering event source` line.
                .level(log::LevelFilter::Info)
                // Trace only for our own crates, so the DebugPanel and the
                // Activity Log get the full backend story without dragging in
                // the network reactor noise. `aeroftp` is the binary name,
                // `ftp_client_gui_lib` is the lib name (kept for legacy log
                // entries that still use the old crate name).
                .level_for("aeroftp", log::LevelFilter::Trace)
                .level_for("ftp_client_gui_lib", log::LevelFilter::Trace)
                // Fan-out backend logs to the webview via the `log://log` event,
                // consumed by the in-app DebugPanel. Stdout + LogDir targets are
                // preserved by default; this one is additive.
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Webview,
                ))
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // Pass --autostart so we can detect launches triggered by the OS
            // autostart entry and (optionally) start hidden in the system tray.
            Some(vec!["--autostart"]),
        ))
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // OS "Extract here / to folder" verb (Deliverable G): open the dedicated
            // extract window WITHOUT raising or booting the main app, then stop. The
            // main window must stay exactly as the user left it (possibly hidden).
            if let Some((mode, path)) = parse_extract_intent(&argv) {
                open_extract_window(app, &mode, &path);
                return;
            }
            // When a second instance is launched, show and focus the existing window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            // Forward .aerovault/.aerozip file argument to frontend
            if let Some(vault_arg) = argv
                .iter()
                .skip(1)
                .find(|a| a.ends_with(".aerovault") || a.ends_with(".aerozip"))
            {
                if let Ok(canonical) = std::fs::canonicalize(vault_arg) {
                    let meta = std::fs::symlink_metadata(&canonical);
                    if meta.map(|m| m.is_file()).unwrap_or(false) {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window
                                .emit("vault-open-file", canonical.to_string_lossy().to_string());
                        }
                    }
                }
            }
            // Forward .aeroftp-keystore file argument to frontend so a
            // double-clicked keystore lands on the import/key-entry screen
            // instead of just raising the window (issue #214 pt.4a). Same
            // canonicalize + symlink_metadata validation as .aerovault.
            if let Some(ks_arg) = argv
                .iter()
                .skip(1)
                .find(|a| a.ends_with(".aeroftp-keystore"))
            {
                if let Ok(canonical) = std::fs::canonicalize(ks_arg) {
                    let meta = std::fs::symlink_metadata(&canonical);
                    if meta.map(|m| m.is_file()).unwrap_or(false) {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit(
                                "keystore-open-file",
                                canonical.to_string_lossy().to_string(),
                            );
                        }
                    }
                }
            }
            // Forward a double-clicked plain .aeroftp server-profiles export
            // so it lands on the import/password screen instead of just
            // raising the window (issue #214 pt.4a, plain .aeroftp).
            // ends_with(".aeroftp") does not match ".aeroftp-keystore", so
            // there is no collision with the block above.
            if let Some(sp_arg) = argv.iter().skip(1).find(|a| a.ends_with(".aeroftp")) {
                if let Ok(canonical) = std::fs::canonicalize(sp_arg) {
                    let meta = std::fs::symlink_metadata(&canonical);
                    if meta.map(|m| m.is_file()).unwrap_or(false) {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window
                                .emit("servers-open-file", canonical.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }))
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_denylist(&["splashscreen", "extract"])
                .skip_initial_state("main")
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .setup(move |app| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder};
            use tauri_plugin_window_state::{StateFlags, WindowExt};

            // Bridge `tracing::*` events to the `log` facade so the
            // tauri-plugin-log Webview target picks them up too. Without this
            // bridge every provider that uses `tracing::info!` (S3, WebDAV,
            // OAuth, sync, etc.) emits into the void: tauri-plugin-log only
            // captures `log::*` macros. The bridge is fire-and-forget; if a
            // subscriber is already installed (unlikely in our setup) we
            // silently keep the existing one.
            let _ = tracing::subscriber::set_global_default(TracingToLogBridge);

            // Register the global AppHandle so Tauri-agnostic code paths
            // (e.g. the MEGAcmd warmup notice in the provider layer) can emit
            // frontend events without threading a handle through every call.
            crate::app_events::register_app_handle(app.handle().clone());

            // Wait for tauri-plugin-localhost to bind the loopback port before
            // any webview tries to load from it.
            //
            // The plugin spawns its actix server on a background thread during
            // its own `Plugin::initialize`. On the warm path (manual launch),
            // the bind beats the splash creation by an order of magnitude. On
            // cold OS-autostart with the app launched alongside login services
            // and minimised to tray, the bind can lose by 100-500ms: long
            // enough for WebKit to GET 127.0.0.1:14321 and render
            // "Could not connect to 127.0.0.1: Connection refused" inside
            // the splash and the main window. Restarting the app from the
            // tray hides the issue because by then the port is already up.
            //
            // Short blocking poll: zero cost on the warm path (the connect
            // succeeds on the first attempt), bounded by 5s on the cold path
            // before we fall through with a warning.
            #[cfg(all(not(dev), target_os = "linux"))]
            {
                use std::net::{SocketAddr, TcpStream};
                use std::time::{Duration, Instant};

                let addr: SocketAddr = format!("127.0.0.1:{}", port)
                    .parse()
                    .expect("valid localhost addr");
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut last_err: Option<std::io::Error> = None;
                while Instant::now() < deadline {
                    match TcpStream::connect_timeout(&addr, Duration::from_millis(150)) {
                        Ok(_) => {
                            last_err = None;
                            break;
                        }
                        Err(e) => {
                            last_err = Some(e);
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
                if let Some(e) = last_err {
                    log::warn!(
                        "tauri-plugin-localhost did not bind 127.0.0.1:{} within 5s ({}); \
                         splash and main window may briefly show a connection-refused page",
                        port,
                        e
                    );
                } else {
                    log::info!("tauri-plugin-localhost is listening on 127.0.0.1:{}", port);
                }
            }

            // OS "Extract here / to folder" verb on a COLD launch (no instance was
            // running): open ONLY the dedicated extract window and skip the rest of
            // setup entirely (no main window, no splash, no tray, no chat DB). The
            // localhost server is already bound above, which is all the tiny
            // extract.html webview needs. The process exits when that window closes.
            // Subsequent launches while running are handled by the single-instance
            // hook above. This is the Option B "skip the heavy boot" short-circuit.
            {
                let early_args: Vec<String> = std::env::args().collect();
                if let Some((mode, path)) = parse_extract_intent(&early_args) {
                    open_extract_window(app.handle(), &mode, &path);
                    return Ok(());
                }
            }

            // Ensure AppConfig directory exists with restricted permissions (0700).
            // In portable mode this resolves to <exe-dir>/data/config; otherwise
            // to the OS-native app config dir.
            if let Ok(config_dir) = portable::app_config_dir(app.handle()) {
                let _ = std::fs::create_dir_all(&config_dir);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &config_dir,
                        std::fs::Permissions::from_mode(0o700),
                    );
                }
            }

            // Initialize Chat History SQLite database
            match chat_history::init_db(app.handle()) {
                Ok(conn) => {
                    if let Err(e) = chat_history::migrate_from_json(&conn, app.handle()) {
                        log::warn!("Chat history migration failed: {e}");
                    }
                    app.manage(chat_history::ChatHistoryDb(std::sync::Mutex::new(conn)));
                }
                Err(e) => {
                    log::error!("Chat history DB init failed: {e}");
                    // Fallback: in-memory DB so commands don't panic
                    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
                    let _ = chat_history::init_db_schema(&conn);
                    app.manage(chat_history::ChatHistoryDb(std::sync::Mutex::new(conn)));
                }
            }

            // Initialize the additive multi-user partition DB. Migration needs
            // an unlocked credential vault, so startup only creates/validates
            // schema; commands run the idempotent legacy import when possible.
            if let Err(e) = user_partitions::init_empty_db(app.handle()) {
                log::error!("User partitions DB init failed: {e}");
            }

            // Initialize File Tags SQLite database
            match file_tags::init_db(app.handle()) {
                Ok(conn) => {
                    app.manage(file_tags::FileTagsDb(std::sync::Mutex::new(conn)));
                }
                Err(e) => {
                    log::error!("File tags DB init failed: {e}");
                    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
                    let _ = file_tags::init_db_schema(&conn);
                    app.manage(file_tags::FileTagsDb(std::sync::Mutex::new(conn)));
                }
            }

            // Initialize Agent Memory SQLite database
            match agent_memory_db::init_db(app.handle()) {
                Ok(conn) => {
                    app.manage(agent_memory_db::AgentMemoryDb(std::sync::Mutex::new(conn)));
                }
                Err(e) => {
                    log::error!("Agent memory DB init failed: {e}");
                    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
                    let _ = agent_memory_db::init_db_schema(&conn);
                    app.manage(agent_memory_db::AgentMemoryDb(std::sync::Mutex::new(conn)));
                }
            }

            // Initialize Speed Test History SQLite database
            match speedtest::init_history_db(app.handle()) {
                Ok(conn) => {
                    app.manage(speedtest::SpeedTestHistoryDb(std::sync::Mutex::new(conn)));
                }
                Err(e) => {
                    log::error!("Speed test history DB init failed: {e}");
                    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
                    let _ = speedtest::init_history_schema(&conn);
                    app.manage(speedtest::SpeedTestHistoryDb(std::sync::Mutex::new(conn)));
                }
            }

            // Initialize Vault History SQLite database
            {
                let config_dir = portable::app_config_dir(app.handle()).unwrap_or_default();
                let db_path = config_dir.join("vault_history.db");
                match rusqlite::Connection::open(&db_path) {
                    Ok(conn) => {
                        if let Err(e) = vault_history::init_db(&conn) {
                            log::error!("Vault history schema init failed: {e}");
                        }
                        // Restrict the unencrypted history DB to the owner (CLAUDE-AV-023).
                        vault_history::harden_db_file(&db_path);
                        app.manage(vault_history::VaultHistoryDb(std::sync::Mutex::new(conn)));
                    }
                    Err(e) => {
                        log::error!("Vault history DB open failed: {e}");
                        let conn =
                            rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
                        if let Err(e2) = vault_history::init_db(&conn) {
                            log::error!("Vault history in-memory schema init failed: {e2}");
                        }
                        app.manage(vault_history::VaultHistoryDb(std::sync::Mutex::new(conn)));
                    }
                }
            }

            // Start mount watcher: emits 'volumes-changed' events instead of 5s polling
            filesystem::start_mount_watcher(app.handle().clone());

            // Proactive AeroVault overlay sweeper: polls every OVERLAY_SWEEPER_INTERVAL_SECS,
            // evicts sessions past their idle timeout and emits `aerovault-overlay-expired`
            // so the frontend can drop overlay state without waiting for the next user action.
            {
                let overlay_sessions = app.state::<AeroVaultOverlayState>().sessions_handle();
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
                        OVERLAY_SWEEPER_INTERVAL_SECS,
                    ));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        ticker.tick().await;
                        let expired = {
                            let mut sessions = overlay_sessions.lock().await;
                            drain_expired_overlay_sessions(&mut sessions, Instant::now())
                        };
                        for (session_id, source) in expired {
                            let _ = app_handle.emit(
                                "aerovault-overlay-expired",
                                serde_json::json!({
                                    "session_id": session_id,
                                    "source": source,
                                }),
                            );
                        }
                    }
                });
            }

            // Local panel filesystem watcher state (one watcher slot, swapped on path change)
            app.manage(local_panel_watcher::LocalPanelWatcherState::default());

            // === Main window ===
            // Built programmatically (not via tauri.conf.json) so the URL can
            // be platform-specific and the window is created AFTER the
            // tauri-plugin-localhost bind wait, with the final URL up-front
            // and no post-creation navigation.
            //
            // On Linux production we load directly from the localhost server
            // because WebKitGTK has historically had rendering issues with
            // `tauri://` for Monaco, xterm.js and iframes; serving via HTTP
            // avoids them entirely. Keeping `http://127.0.0.1:14321` as the
            // webview origin also means upgrades from older AeroFTP releases
            // preserve every localStorage/IndexedDB value (theme, local tabs,
            // recent paths, etc.), which are origin-scoped in WebKit.
            //
            // Tauri is pinned to 2.11.0 in Cargo.toml because 2.11.1's fix
            // for GHSA-7gmj-67g7-phm9 introduced an `is_local_url()` check
            // that classifies `http://127.0.0.1:*` as remote and rejects all
            // custom commands without a full app ACL manifest. AeroFTP does
            // not load remote/untrusted content, so the CVE vector does not
            // apply to our surface.
            //
            // Window is hidden until the frontend signals `app_ready` so the
            // splash can stay visible during initial load.
            let main_url: WebviewUrl = {
                #[cfg(dev)]
                {
                    WebviewUrl::App("index.html".into())
                }
                #[cfg(all(not(dev), target_os = "linux"))]
                {
                    WebviewUrl::External(
                        url::Url::parse(&format!("http://127.0.0.1:{}/index.html", port))
                            .expect("valid localhost URL"),
                    )
                }
                #[cfg(all(not(dev), not(target_os = "linux")))]
                {
                    WebviewUrl::App("index.html".into())
                }
            };

            // Note: `.transparent(false)` is the default and is gated behind
            // the `macos-private-api` feature on macOS, so we omit the call
            // to keep the build portable.
            //
            // Portable mode: route WebView2/WebKitGTK state to <exe-dir>/data/webview
            // so two portable installations in different folders cannot share
            // localStorage, IndexedDB or cookies through the identifier-scoped
            // default. Installed builds (None branch) keep Tauri's default
            // identifier-scoped folder for zero-migration parity.
            // Clamp the initial inner size to the primary monitor so that on a
            // 13.3" Retina default-scaled at 1280x800 the window does not open
            // off-screen and the user is not forced to double-click the
            // titlebar to fit. Falls back to the historical 1540x1050 if the
            // monitor cannot be probed (very early startup, headless run).
            // Margins reserve space for the menu bar / dock. Issue #241.
            // Shared with the post-restore self-heal so a poisoned restored
            // size falls back to the exact first-launch dimensions (#290).
            let (initial_w, initial_h) = computed_initial_inner_size(app.handle());

            let main_builder = WebviewWindowBuilder::new(app, "main", main_url)
                .title("AeroFTP")
                .inner_size(initial_w, initial_h)
                .min_inner_size(MAIN_MIN_INNER_W, MAIN_MIN_INNER_H)
                .center()
                .resizable(true)
                .maximizable(true)
                .minimizable(true)
                .closable(true)
                .visible(false)
                .disable_drag_drop_handler();
            // Window chrome (issue #290, macOS Tahoe "no window after splash").
            // A fully borderless window (decorations(false)) cannot become the
            // key window on macOS and fails to present after the splash closes,
            // leaving only a Dock icon and no visible window. On macOS we keep
            // native decorations but switch to the Overlay title-bar style with
            // a hidden title: the native traffic lights stay (key-able window,
            // top-left) while the frontend still draws its own title bar over the
            // content. Linux/Windows keep the borderless custom title bar, which
            // presents reliably there. Overlay requires decorations: true, so the
            // macOS branch deliberately does not call decorations(false).
            #[cfg(target_os = "macos")]
            let main_builder = main_builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            #[cfg(not(target_os = "macos"))]
            let main_builder = main_builder.decorations(false);
            let main_builder = match portable::webview_data_dir() {
                Some(dir) => main_builder.data_directory(dir),
                None => main_builder,
            };
            let _main = main_builder.build()?;

            let accel = |shortcut: &'static str| -> Option<&'static str> {
                #[cfg(target_os = "linux")]
                {
                    let _ = shortcut;
                    None
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Some(shortcut)
                }
            };

            // Create menu items
            let quit = MenuItem::with_id(app, "quit", "Quit AeroFTP", true, accel("CmdOrCtrl+Q"))?;
            let about = MenuItem::with_id(app, "about", "About AeroFTP", true, None::<&str>)?;
            let settings =
                MenuItem::with_id(app, "settings", "Settings...", true, accel("CmdOrCtrl+,"))?;
            let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, accel("CmdOrCtrl+R"))?;
            let shortcuts =
                MenuItem::with_id(app, "shortcuts", "Keyboard Shortcuts", true, accel("F1"))?;
            let support =
                MenuItem::with_id(app, "support", "Support Development ❤️", true, None::<&str>)?;

            // File menu
            let file_menu = Submenu::with_items(
                app,
                "File",
                true,
                &[
                    &MenuItem::with_id(
                        app,
                        "new_folder",
                        "New Folder",
                        true,
                        accel("CmdOrCtrl+N"),
                    )?,
                    &PredefinedMenuItem::separator(app)?,
                    &settings,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(
                        app,
                        "toggle_debug_mode",
                        "Debug Mode",
                        true,
                        accel("CmdOrCtrl+Shift+F12"),
                    )?,
                    &MenuItem::with_id(
                        app,
                        "show_dependencies",
                        "Dependencies...",
                        true,
                        None::<&str>,
                    )?,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ],
            )?;

            // Edit menu
            let edit_menu = Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "rename", "Rename", true, accel("F2"))?,
                    &MenuItem::with_id(app, "delete", "Delete", true, accel("Delete"))?,
                ],
            )?;

            // View menu
            let devtools_submenu = Submenu::with_items(
                app,
                "DevTools",
                true,
                &[
                    &MenuItem::with_id(
                        app,
                        "toggle_devtools",
                        "Toggle DevTools",
                        true,
                        accel("CmdOrCtrl+Shift+D"),
                    )?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(
                        app,
                        "toggle_editor",
                        "Toggle Editor",
                        true,
                        accel("CmdOrCtrl+1"),
                    )?,
                    &MenuItem::with_id(
                        app,
                        "toggle_terminal",
                        "Toggle Terminal",
                        true,
                        accel("CmdOrCtrl+2"),
                    )?,
                    &MenuItem::with_id(
                        app,
                        "toggle_agent",
                        "Toggle Agent",
                        true,
                        accel("CmdOrCtrl+3"),
                    )?,
                ],
            )?;

            let view_menu = Submenu::with_items(
                app,
                "View",
                true,
                &[
                    &MenuItem::with_id(
                        app,
                        "toggle_aerofile",
                        "AeroFile",
                        true,
                        accel("CmdOrCtrl+L"),
                    )?,
                    &PredefinedMenuItem::separator(app)?,
                    &refresh,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(
                        app,
                        "toggle_theme",
                        "Toggle Theme",
                        true,
                        accel("CmdOrCtrl+T"),
                    )?,
                    &PredefinedMenuItem::separator(app)?,
                    &devtools_submenu,
                ],
            )?;

            // Help menu
            let check_update =
                MenuItem::with_id(app, "check_update", "Check for Updates", true, None::<&str>)?;

            let help_menu = Submenu::with_items(
                app,
                "Help",
                true,
                &[
                    &check_update,
                    &PredefinedMenuItem::separator(app)?,
                    &shortcuts,
                    &PredefinedMenuItem::separator(app)?,
                    &support,
                    &PredefinedMenuItem::separator(app)?,
                    &about,
                ],
            )?;

            // === Splash Screen ===
            // Create splash BEFORE setting the global app menu so it never inherits it.
            // The main window was built earlier with `visible(false)` and stays
            // hidden until the frontend signals readiness via the `app_ready`
            // command.
            let splash_url = {
                #[cfg(dev)]
                {
                    WebviewUrl::External(
                        url::Url::parse("http://127.0.0.1:5173/splash.html").unwrap(),
                    )
                }
                #[cfg(all(not(dev), target_os = "linux"))]
                {
                    WebviewUrl::External(
                        url::Url::parse(&format!("http://127.0.0.1:{}/splash.html", port)).unwrap(),
                    )
                }
                #[cfg(all(not(dev), not(target_os = "linux")))]
                {
                    WebviewUrl::App("splash.html".into())
                }
            };

            // Splash shares the same WebView data directory as main in portable
            // mode so storage events emitted on first load don't fight a second
            // identifier-scoped folder.
            let splash_builder = WebviewWindowBuilder::new(app, "splashscreen", splash_url)
                .title("AeroFTP")
                .inner_size(420.0, 340.0)
                .resizable(false)
                .decorations(false)
                // Inject the real app version (single source of truth: the crate
                // version, bumped on every release) so the splash never drifts
                // from the published version again (#367). Runs before the page's
                // own scripts; splash.html reads it with a hardcoded fallback.
                .initialization_script(concat!(
                    "window.__AEROFTP_VERSION__ = \"",
                    env!("CARGO_PKG_VERSION"),
                    "\";"
                ))
                .center();
            let splash_builder = match portable::webview_data_dir() {
                Some(dir) => splash_builder.data_directory(dir),
                None => splash_builder,
            };
            let _splash = splash_builder.build()?;

            SPLASH_CREATED_AT.get_or_init(std::time::Instant::now);
            info!("Splash screen created");

            // Build menu but do NOT set it globally yet: GTK applies global menus
            // to ALL windows instantly, causing a menu flash on the splash screen.
            // The menu will be set in app_ready() after the splash is closed.
            let menu = Menu::with_items(app, &[&file_menu, &edit_menu, &view_menu, &help_menu])?;
            app.manage(std::sync::Mutex::new(Some(menu)));

            // Safety timeout: if frontend doesn't signal app_ready within 10 seconds,
            // force-close splash, set deferred menu, and show main window.
            // Skipped entirely if app_ready already ran (prevents window re-show).
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                if APP_READY_DONE.load(Ordering::SeqCst) {
                    return; // app_ready already handled everything
                }
                warn!("Splash screen safety timeout reached, force-closing");
                // This runs on a std thread (OFF the GTK main thread). The splash
                // teardown, deferred menu and window show below all touch GTK/GLib,
                // so marshal them onto the main thread or they corrupt the GLib heap
                // (same "malloc(): unaligned fastbin chunk" abort as app_ready).
                let app_main = app_handle.clone();
                let _ = app_handle.run_on_main_thread(move || {
                    if let Some(splash) = app_main.get_webview_window("splashscreen") {
                        let _ = splash.close();
                    }
                    // Set deferred menu
                    if let Some(deferred) = app_main
                        .try_state::<std::sync::Mutex<Option<tauri::menu::Menu<tauri::Wry>>>>()
                    {
                        if let Ok(mut guard) = deferred.lock() {
                            if let Some(menu) = guard.take() {
                                let _ = app_main.set_menu(menu);
                            }
                        }
                    }
                    if let Some(main_window) = app_main.get_webview_window("main") {
                        let _ = main_window.remove_menu();
                        let _ = main_window.restore_state(
                            StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED,
                        );
                        // Heal a poisoned 0x0 size restored from earlier broken builds (#290).
                        heal_restored_window_size(&main_window);
                        let _ = main_window.show();
                        let _ = main_window.set_focus();
                        log_window_diagnostics(&main_window, "safety-timeout post-show");
                    }
                });
            });
            // ============ System Tray Icon ============
            // Create tray menu.
            // "Open Cloud Folder" is only meaningful when AeroCloud is configured:
            // when it is not, the local folder is just the unrealised default
            // (~/AeroCloud) and opening a non-existent path is pointless, so the
            // entry starts disabled and reflects the saved AeroCloud state. Read
            // here in setup() on the GTK main thread (a later off-thread mutation
            // would risk the GLib-heap class of crash).
            let cloud_enabled = cloud_config::load_cloud_config().enabled;
            let tray_sync_now =
                MenuItem::with_id(app, "tray_sync_now", "Sync Now", true, None::<&str>)?;
            let tray_pause =
                MenuItem::with_id(app, "tray_pause", "Pause Sync", true, None::<&str>)?;
            let tray_open_folder = MenuItem::with_id(
                app,
                "tray_open_folder",
                "Open Cloud Folder",
                cloud_enabled,
                None::<&str>,
            )?;
            let tray_check_update = MenuItem::with_id(
                app,
                "tray_check_update",
                "Check for Updates",
                true,
                None::<&str>,
            )?;
            let tray_separator = PredefinedMenuItem::separator(app)?;
            let tray_show =
                MenuItem::with_id(app, "tray_show", "Show AeroFTP", true, None::<&str>)?;
            let tray_quit = MenuItem::with_id(app, "tray_quit", "Quit", true, None::<&str>)?;

            let tray_menu = Menu::with_items(
                app,
                &[
                    &tray_sync_now,
                    &tray_pause,
                    &tray_separator,
                    &tray_open_folder,
                    &tray_check_update,
                    &PredefinedMenuItem::separator(app)?,
                    &tray_show,
                    &tray_quit,
                ],
            )?;

            // Build tray icon using white monochrome icon (standard for system tray)
            let tray_png = image::load_from_memory(include_bytes!(
                "../../icons/AeroFTP_simbol_white_120x120.png"
            ))
            .expect("Failed to decode tray icon");
            let tray_rgba = tray_png.to_rgba8();
            let (w, h) = tray_rgba.dimensions();
            let icon = tauri::image::Image::new_owned(tray_rgba.into_raw(), w, h);

            let tray_builder = TrayIconBuilder::with_id("main")
                .icon(icon)
                .tooltip("AeroCloud - Idle")
                .menu(&tray_menu)
                .on_menu_event(|app, event| {
                    let id = event.id().as_ref();
                    info!("Tray menu event: {}", id);
                    match id {
                        "tray_sync_now" => {
                            let _ = app.emit("menu-event", "cloud_sync_now");
                        }
                        "tray_pause" => {
                            let _ = app.emit("menu-event", "cloud_pause");
                        }
                        "tray_open_folder" => {
                            let _ = app.emit("menu-event", "cloud_open_folder");
                        }
                        "tray_check_update" => {
                            let _ = app.emit("menu-event", "check_update");
                        }
                        "tray_show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                // unminimize() is required when the window was
                                // sent to the taskbar via the minimize (-)
                                // button: show() alone only un-hides a hidden
                                // (close-to-tray) window, so a minimized window
                                // would stay minimized (#270 comment 17195020).
                                let _ = window.show();
                                let _ = window.unminimize();
                                let _ = window.set_focus();
                            }
                        }
                        "tray_quit" => {
                            exit_app(app);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    // Click on tray icon shows the window
                    if let tauri::tray::TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            // Mirror the tray "Show AeroFTP" menu: unminimize()
                            // so a left-click also restores a window that was
                            // minimized via the (-) button, not just one that
                            // was hidden to tray (#270 comment 17195020).
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                });

            // libappindicator-sys panics (it `expect`s, it does not return a
            // Result) when it cannot dlopen the appindicator library, so `?`
            // never sees it. Minimal or immutable distros such as Fedora
            // Silverblue ship without libappindicator, so probe the library
            // first and run tray-less instead of crashing on launch (#362).
            #[cfg(target_os = "linux")]
            fn appindicator_available() -> bool {
                use std::ffi::CString;
                const CANDIDATES: [&str; 4] = [
                    "libayatana-appindicator3.so.1",
                    "libappindicator3.so.1",
                    "libayatana-appindicator3.so",
                    "libappindicator3.so",
                ];
                for name in CANDIDATES {
                    if let Ok(c) = CString::new(name) {
                        // SAFETY: dlopen with a valid NUL-terminated name; the
                        // handle is released immediately on a successful probe.
                        unsafe {
                            let handle =
                                libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
                            if !handle.is_null() {
                                libc::dlclose(handle);
                                return true;
                            }
                        }
                    }
                }
                false
            }
            #[cfg(not(target_os = "linux"))]
            fn appindicator_available() -> bool {
                true
            }

            let tray_available = appindicator_available();
            TRAY_AVAILABLE.store(tray_available, std::sync::atomic::Ordering::SeqCst);
            if tray_available {
                let _tray = tray_builder.build(app)?;
                info!("System tray icon initialized");
            } else {
                log::warn!(
                    "System tray unavailable: libappindicator / ayatana-appindicator3 \
                     not found. Running without a tray icon (install \
                     libayatana-appindicator3 to enable it). See \
                     https://github.com/axpdev-lab/aeroftp/issues/362"
                );
            }

            // Handle .aerovault/.aerozip file passed as CLI argument on first launch
            {
                let args: Vec<String> = std::env::args().collect();
                if let Some(vault_arg) = args
                    .iter()
                    .skip(1)
                    .find(|a| a.ends_with(".aerovault") || a.ends_with(".aerozip"))
                {
                    if let Ok(canonical) = std::fs::canonicalize(vault_arg) {
                        let meta = std::fs::symlink_metadata(&canonical);
                        if meta.map(|m| m.is_file()).unwrap_or(false) {
                            let vault_path = canonical.to_string_lossy().to_string();
                            let app_handle = app.handle().clone();
                            // Emit after a short delay to ensure frontend is ready
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_millis(1500));
                                if let Some(window) = app_handle.get_webview_window("main") {
                                    let _ = window.emit("vault-open-file", vault_path);
                                }
                            });
                        }
                    }
                }
                // Same for a double-clicked .aeroftp-keystore on first launch
                // so it routes to the keystore import/key-entry screen
                // instead of a bare window (issue #214 pt.4a).
                if let Some(ks_arg) = args
                    .iter()
                    .skip(1)
                    .find(|a| a.ends_with(".aeroftp-keystore"))
                {
                    if let Ok(canonical) = std::fs::canonicalize(ks_arg) {
                        let meta = std::fs::symlink_metadata(&canonical);
                        if meta.map(|m| m.is_file()).unwrap_or(false) {
                            let ks_path = canonical.to_string_lossy().to_string();
                            let app_handle = app.handle().clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_millis(1500));
                                if let Some(window) = app_handle.get_webview_window("main") {
                                    let _ = window.emit("keystore-open-file", ks_path);
                                }
                            });
                        }
                    }
                }
                // Same for a double-clicked plain .aeroftp server-profiles
                // export on first launch so it routes to the import/password
                // screen instead of a bare window (issue #214 pt.4a, plain
                // .aeroftp). ends_with(".aeroftp") excludes ".aeroftp-keystore".
                if let Some(sp_arg) = args.iter().skip(1).find(|a| a.ends_with(".aeroftp")) {
                    if let Ok(canonical) = std::fs::canonicalize(sp_arg) {
                        let meta = std::fs::symlink_metadata(&canonical);
                        if meta.map(|m| m.is_file()).unwrap_or(false) {
                            let sp_path = canonical.to_string_lossy().to_string();
                            let app_handle = app.handle().clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_millis(1500));
                                if let Some(window) = app_handle.get_webview_window("main") {
                                    let _ = window.emit("servers-open-file", sp_path);
                                }
                            });
                        }
                    }
                }
            }

            Ok(())
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            info!("Menu event: {}", id);
            if id == "quit" {
                exit_app(app);
                return;
            }
            // Emit event to frontend
            let _ = app.emit("menu-event", id);
        })
        .on_window_event(|window, event| {
            // Only handle close events for the main window
            if window.label() != "main" {
                return;
            }
            // Hide window instead of closing when AeroCloud is enabled or the
            // user has opted into "close to tray" in Settings.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let cloud_config = cloud_config::load_cloud_config();
                let close_to_tray = CLOSE_TO_TRAY.load(std::sync::atomic::Ordering::SeqCst);
                let tray_available = TRAY_AVAILABLE.load(std::sync::atomic::Ordering::SeqCst);
                // Only hide to tray if a tray actually exists. On distros without
                // libappindicator (#362) the tray is skipped, so hiding here would
                // strand the window with no way to bring it back: close normally.
                if (cloud_config.enabled || close_to_tray) && tray_available {
                    let reason = if close_to_tray {
                        "close-to-tray setting"
                    } else {
                        "AeroCloud enabled"
                    };
                    info!("Window close requested, hiding to tray ({})", reason);
                    let _ = window.hide();
                    api.prevent_close();
                } else {
                    // P1-5: Cleanup Cloud Filter root registrations on app exit
                    #[cfg(windows)]
                    {
                        if let Err(e) = crate::cloud_filter_badge::cleanup_all_roots() {
                            warn!("Cloud Filter cleanup on exit: {}", e);
                        }
                    }
                    info!("Window close requested, AeroCloud not enabled, exiting");
                }
            }
        })
        .manage(AppState::new())
        .manage(provider_commands::ProviderState::new())
        .manage(provider_commands::ConnectionCancelRegistry::new())
        .manage(provider_commands::ListingCancelState::new())
        // AeroShare: registry of the background drive sync tasks consumed by
        // provider_connect for protocol="peer" (lifecycle per D-GUI-1:
        // open-or-tray = serving, Quit = stop).
        .manage(peer::runtime::PeerRuntime::default())
        .manage(session_manager::MultiProviderState::new());

    // Add PTY state for terminal support (all platforms)
    let builder = builder.manage(create_pty_state());
    // Add SSH shell state for remote shell sessions
    let builder = builder.manage(create_ssh_shell_state());
    let builder = builder.manage(cryptomator::CryptomatorState::new());
    let builder = builder.manage(rclone_crypt::RcloneCryptState::new());
    let builder = builder.manage(aerocrypt_provider::AeroCryptState::new());
    let builder = builder.manage(AeroVaultOverlayState::new());
    let builder = builder.manage(cross_profile_commands::CrossProfileState::new());
    // Master Password state for app-level security
    let builder = builder.manage(master_password::MasterPasswordState::new());
    let builder = builder.manage(totp::TotpState::default());
    let builder = builder.manage(speech::SpeechState::default());
    let builder = builder.manage(speedtest::SpeedTestState::new());

    builder
        .invoke_handler(tauri::generate_handler![
            #[cfg(feature = "aerorsync")]
            local_sync::local_sync_run,
            #[cfg(feature = "aerorsync")]
            local_sync::local_sync_cancel,
            transfer_queue_scan::transfer_queue_scan_remote_tree,
            app_ready,
            set_close_to_tray,
            is_autostart_launch,
            portable_info,
            copy_to_clipboard,
            local_panel_watcher::local_panel_watch,
            local_panel_watcher::local_panel_watch_stop,
            resolve_hostname,
            #[cfg(debug_assertions)]
            panic_safe::debug_panic_command,
            connect_ftp,
            disconnect_ftp,
            check_connection,
            ftp_noop,
            reconnect_ftp,
            list_files,
            change_directory,
            download_file,
            upload_file,
            download_files_batch,
            upload_files_batch,
            download_folder,
            upload_folder,
            cancel_transfer,
            reset_cancel_flag,
            set_speed_limit,
            get_speed_limit,
            is_running_as_snap,
            get_local_files,
            open_in_file_manager,
            open_local_file,
            safe_picker_start_dir,
            delete_remote_file,
            rename_remote_file,
            create_remote_folder,
            chmod_remote_file,
            delete_local_file,
            rename_local_file,
            copy_local_file,
            create_local_folder,
            read_file_base64,
            calculate_checksum,
            compress_files,
            estimate_compressed_size,
            extract_archive,
            compress_7z,
            extract_7z,
            is_7z_encrypted,
            is_zip_encrypted,
            detect_archive_cipher,
            detect_archive_meta,
            extract_rar,
            is_rar_encrypted,
            compress_tar,
            compress_single,
            extract_tar,
            extract_single,
            extract_probe,
            resolve_unique_extract_dir,
            ftp_read_file_base64,
            read_local_file,
            read_local_file_base64,
            preview_remote_file,
            detect_server_favicon,
            detect_provider_favicon,
            save_local_file,
            save_remote_file,
            toggle_menu_bar,
            rebuild_menu,
            compare_directories,
            compare_local_directories,
            get_compare_options_default,
            load_sync_index_cmd,
            save_sync_index_cmd,
            load_sync_journal_cmd,
            save_sync_journal_cmd,
            delete_sync_journal_cmd,
            list_sync_journals_cmd,
            cleanup_old_journals_cmd,
            clear_all_journals_cmd,
            load_sync_profiles_cmd,
            save_sync_profile_cmd,
            delete_sync_profile_cmd,
            // Phase 3A+: Parallel sync, scan, scheduler, watcher
            parallel_sync_execute,
            get_parallel_scan_files,
            get_sync_schedule_cmd,
            save_sync_schedule_cmd,
            get_watcher_status_cmd,
            get_transfer_optimization_hints,
            get_transfer_capabilities,
            sftp_probe_delta_eligibility,
            get_multi_path_config,
            save_multi_path_config_cmd,
            add_path_pair,
            remove_path_pair,
            export_sync_template_cmd,
            import_sync_template_cmd,
            export_sync_script_cmd,
            import_sync_script_cmd,
            aerosync_export_script_cmd,
            aerosync_import_script_cmd,
            flatten_local_descendants,
            create_sync_snapshot_cmd,
            list_sync_snapshots_cmd,
            load_sync_snapshot_cmd,
            restore_sync_snapshot_cmd,
            detect_renames_cmd,
            delete_sync_snapshot_cmd,
            delta_sync_analyze,
            sync_canary_run,
            sync_canary_approve,
            get_journal_signing_key,
            sign_sync_journal,
            verify_journal_signature,
            get_default_retry_policy,
            verify_local_transfer,
            classify_transfer_error,
            sync_ec_generate,
            sync_ec_verify_repair,
            // AeroCloud commands
            get_cloud_config,
            save_cloud_config_cmd,
            setup_aerocloud,
            get_cloud_status,
            enable_aerocloud,
            pause_aerocloud,
            resume_aerocloud,
            disable_aerocloud,
            update_excluded_folders,
            list_remote_folders_tree,
            list_file_versions,
            list_all_file_versions,
            restore_file_version,
            cleanup_versions,
            versions_disk_usage,
            archive_before_sync_delete,
            generate_share_link,
            generate_share_link_remote,
            generate_server_share_link,
            get_default_cloud_folder,
            update_conflict_strategy,
            trigger_cloud_sync,
            // Background sync & tray commands
            start_background_sync,
            stop_background_sync,
            is_background_sync_running,
            set_tray_status,
            update_tray_badge_cmd,
            save_server_credentials,
            // Universal Credential Vault
            init_credential_store,
            unlock_auto_keyring_credential_store,
            bootstrap_master_credential_store,
            get_credential_store_status,
            flatpak_config_import_status,
            flatpak_config_import_apply,
            store_credential,
            get_credential,
            delete_credential,
            unlock_credential_store,
            lock_credential_store,
            enable_master_password,
            disable_master_password,
            change_master_password,
            set_auto_lock_timeout,
            app_master_password_status,
            app_master_password_update_activity,
            app_master_password_check_timeout,
            // Multi-user partition metadata
            user_partitions::user_partitions_init,
            user_partitions::user_partitions_health,
            user_partitions::user_partitions_repair_rebuild,
            restart_app,
            user_partitions::user_partitions_list_users,
            user_partitions::user_partitions_get_active_user,
            user_partitions::user_partitions_load_active_server_profiles,
            user_partitions::user_partitions_save_active_server_profiles,
            user_partitions::user_partitions_relocate_server_profile,
            user_partitions::user_partitions_add_user,
            user_partitions::user_partitions_unlock_user,
            user_partitions::user_partitions_lock_session,
            user_partitions::user_partitions_unlock_status,
            user_partitions::user_partitions_change_passphrase,
            user_partitions::user_partitions_set_active_user,
            user_partitions::user_partitions_rename_user,
            user_partitions::user_partitions_set_user_avatar,
            user_partitions::user_partitions_reorder_users,
            user_partitions::user_partitions_delete_user,
            user_partitions::user_partitions_set_admin,
            user_partitions::user_partitions_set_default_user,
            user_partitions::user_partitions_admin_reset_passphrase,
            user_partitions::user_partitions_storage_stats,
            user_partitions::user_partitions_debug_state,
            user_partitions::user_partitions_get_active_setting,
            user_partitions::user_partitions_set_active_setting,
            user_partitions::user_partitions_delete_active_setting,
            user_partitions::user_partitions_list_active_setting_scopes,
            user_partitions::user_partitions_get_user_credential,
            user_partitions::user_partitions_set_user_credential,
            user_partitions::user_partitions_delete_user_credential,
            user_partitions::user_partitions_find_cross_user_dedup,
            // AeroShare P1 (task 4/5): the peer handshake + inventory surface
            peer_commands::peer_identity_get,
            peer_commands::peer_share_start,
            peer_commands::peer_drive_add,
            peer_commands::peer_friends_list,
            peer_commands::peer_contact_add,
            peer_commands::peer_contact_remove,
            peer_commands::peer_drives_list,
            // AeroShare Share surface slice 2: the "Shared by me" panel
            peer_commands::peer_shares_list,
            peer_commands::peer_share_stop,
            peer_commands::peer_share_resume,
            peer_commands::peer_share_remove,
            // AeroShare "Send file to user" one-shot (AirDrop)
            peer_commands::peer_send_file,
            peer_commands::peer_receiver_start,
            peer_commands::peer_receiver_stop,
            peer_commands::peer_receiver_status,
            peer_commands::peer_incoming_respond,
            peer_commands::peer_friends_presence,
            peer_commands::peer_send_knock,
            peer_commands::peer_send_action,
            // AeroShare v4.1.0 security follow-ups (#370): anti-flood + discovery
            peer_commands::peer_contact_mute,
            peer_commands::peer_contact_unmute,
            peer_commands::peer_mutes_list,
            peer_commands::peer_settings_get,
            peer_commands::peer_settings_set,
            peer_commands::peer_identity_rotate,
            peer_commands::aeroshare_notify,
            peer_commands::aeroshare_inbox_root,
            settings::native_rsync_feature_compiled,
            #[cfg(feature = "aerorsync")]
            settings::native_rsync_enabled_get,
            #[cfg(feature = "aerorsync")]
            settings::native_rsync_enabled_set,
            #[cfg(feature = "aerorsync")]
            settings::native_rsync_mode_get,
            #[cfg(feature = "aerorsync")]
            settings::native_rsync_mode_set,
            #[cfg(feature = "aerorsync")]
            settings::native_rsync_classic_available,
            // Profile Export/Import
            export_server_profiles,
            import_server_profiles,
            read_export_metadata,
            // Generic profile bridge (12 expansion sources)
            bridge_commands::detect_bridge_config,
            bridge_commands::bridge_source_meta,
            bridge_commands::bridge_identify,
            bridge_commands::import_bridge_config,
            bridge_commands::export_bridge_config,
            // Full Keystore Export/Import
            export_keystore,
            import_keystore,
            read_keystore_metadata,
            // Debug & dependencies commands
            get_dependencies,
            check_crate_versions,
            get_system_info,
            // DebugPanel diagnostic suite (Tests tab)
            debug_tests::debug_test_connectivity,
            debug_tests::debug_test_vault_roundtrip,
            debug_tests::debug_test_known_hosts,
            debug_tests::debug_test_aerovault_roundtrip,
            debug_tests::debug_test_plugin_integrity,
            debug_tests::debug_test_provider_selftest,
            debug_tests::debug_export_bundle,
            // Updater commands
            check_update,
            read_update_marker,
            clear_update_marker,
            log_update_detection,
            download_update,
            install_appimage_update,
            install_deb_update,
            install_rpm_update,
            install_windows_update,
            // AI commands
            ai_chat,
            ai_cancel_chat,
            ai_test_provider,
            ai_list_models,
            ai_execute_tool,
            ai_tools::validate_tool_args,
            ai_tools::prepare_ai_tool_approval,
            ai_tools::grant_ai_tool_approval,
            ai_tools::execute_ai_tool,
            ai_tools::shell_execute,
            ai_tools::clipboard_read_image,
            plugins::prepare_plugin_tool_approval,
            // Context Intelligence commands
            coding_context::resolve_context_mentions,
            coding_rules::read_coding_rules,
            context_intelligence::detect_project_context,
            context_intelligence::scan_file_imports,
            context_intelligence::get_git_context,
            context_intelligence::read_agent_memory,
            context_intelligence::write_agent_memory,
            agent_memory_db::agent_memory_store,
            agent_memory_db::agent_memory_search,
            agent_memory_db::agent_memory_delete,
            // Provider health check
            health_check::start_health_scan,
            speech::speech_model_status,
            speech::download_speech_model,
            speech::speech_to_text,
            // Archive browsing & selective extraction
            archive_browse::list_zip,
            archive_browse::list_7z,
            archive_browse::list_tar,
            archive_browse::list_rar,
            archive_browse::extract_zip_entry,
            archive_browse::extract_7z_entry,
            archive_browse::extract_tar_entry,
            archive_browse::extract_rar_entry,
            // AeroVault encrypted folders
            aerovault::vault_create,
            aerovault::vault_list,
            aerovault::vault_get_meta,
            aerovault::vault_add_files,
            aerovault::vault_remove_file,
            aerovault::vault_extract_entry,
            aerovault::vault_change_password,
            // AeroVault v2 - Military-Grade Encryption
            aerovault_v2::vault_v2_create,
            aerovault_v2::vault_v2_open,
            aerovault_v2::is_vault_v2,
            aerovault_v2::vault_v2_peek,
            aerovault_v2::vault_v2_security_info,
            aerovault_v2::vault_v2_add_files,
            aerovault_v2::vault_v2_extract_entry,
            aerovault_v2::vault_v2_extract_all,
            aerovault_v2::vault_v2_save_all,
            aerovault_v2::vault_v2_change_password,
            aerovault_v2::vault_v2_delete_entry,
            aerovault_v2::vault_v2_create_directory,
            aerovault_v2::vault_v2_delete_entries,
            aerovault_v2::vault_v2_move_entry,
            aerovault_v2::vault_v2_rename_entry,
            aerovault_v2::vault_v2_copy_entry,
            aerovault_v2::vault_v2_add_files_to_dir,
            aerovault_v2::vault_v2_compact,
            aerovault_v2::vault_v2_sync_compare,
            aerovault_v2::vault_v2_sync_apply,
            aerovault_v2::vault_v2_scan_directory,
            aerovault_v2::vault_v2_add_directory,
            // AeroVault v3 draft wrapper-stack backend
            aerovault_v3::aerovz_is_archive,
            aerovault_v3::detect_aero_container,
            aerovault_v3::detect_aero_vault_version,
            aerovault_v3::aerovz_create_archive,
            aerovault_v3::aerovz_open_archive,
            aerovault_v3::aerovz_recovery_status,
            aerovault_v3::aerovz_add_files,
            aerovault_v3::aerovz_add_files_to_dir,
            aerovault_v3::aerovz_add_directory,
            aerovault_v3::aerovz_create_directory,
            aerovault_v3::aerovz_delete_entry,
            aerovault_v3::aerovz_delete_entries,
            aerovault_v3::aerovz_extract_entry,
            aerovault_v3::aerovz_extract_all,
            aerovault_v3::aerovz_save_all,
            aerovault_v3::aerovz_scrub,
            aerovault_v3::aerovz_repair,
            aerovault_v3::vault_v3_create,
            aerovault_v3::vault_v3_create_with_error_correction,
            aerovault_v3::vault_v3_open,
            aerovault_v3::is_vault_v3,
            aerovault_v3::vault_v3_add_files,
            aerovault_v3::vault_v3_add_files_to_dir,
            aerovault_v3::vault_v3_extract_entry,
            aerovault_v3::vault_v3_extract_all,
            aerovault_v3::vault_v3_save_all,
            aerovault_v3::vault_v3_create_directory,
            aerovault_v3::vault_v3_delete_entry,
            aerovault_v3::vault_v3_delete_entries,
            aerovault_v3::vault_v3_move_entry,
            aerovault_v3::vault_v3_rename_entry,
            aerovault_v3::vault_v3_copy_entry,
            aerovault_v3::vault_v3_change_password,
            aerovault_v3::vault_v3_change_mode,
            aerovault_v3::vault_v3_add_directory,
            aerovault_v3::vault_v3_security_info,
            aerovault_v3::vault_v3_has_error_correction,
            aerovault_v3::vault_v3_recovery_status,
            aerovault_v3::vault_v3_scrub,
            aerovault_v3::vault_v3_repair,
            aerovault_v3::vault_v3_export_parity,
            aerovault_v3::vault_v3_strip_parity,
            // Remote Vault: open .aerovault on remote servers
            vault_remote::vault_v2_download_remote,
            vault_remote::vault_v2_upload_remote,
            vault_remote::vault_v2_cleanup_temp,
            // Cryptomator vault support
            cryptomator::cryptomator_unlock,
            cryptomator::cryptomator_lock,
            cryptomator::cryptomator_list,
            cryptomator::cryptomator_decrypt_file,
            cryptomator::cryptomator_encrypt_file,
            cryptomator::cryptomator_encrypt_paths,
            cryptomator::cryptomator_create,
            cryptomator::cryptomator_save_all,
            // Rclone crypt compatibility support
            rclone_crypt::rclone_crypt_unlock,
            rclone_crypt::rclone_crypt_lock,
            rclone_crypt::rclone_crypt_decrypt_name,
            rclone_crypt::rclone_crypt_encrypt_name,
            rclone_crypt::rclone_crypt_decrypt_file,
            rclone_crypt::rclone_crypt_decrypt_file_path,
            rclone_crypt::rclone_crypt_encrypt_file_path,
            rclone_crypt_provider_create_remote,
            // Native AeroCrypt overlay (mirrors the rclone set on our own codec)
            aerocrypt_provider::aerocrypt_unlock,
            aerocrypt_provider::aerocrypt_lock,
            aerocrypt_provider::aerocrypt_provider_read_config,
            aerocrypt_provider::aerocrypt_provider_create_remote,
            aerocrypt_provider::aerocrypt_build_emergency_kit,
            aerovault_overlay_unlock,
            aerovault_overlay_lock,
            aerovault_overlay_list,
            aerovault_overlay_extract_entry,
            aerovault_overlay_add_file,
            aerovault_overlay_create_directory,
            aerovault_overlay_delete_entries,
            aerovault_overlay_move_entry,
            aerovault_overlay_rename_entry,
            aerovault_overlay_copy_entry,
            aerovault_overlay_get_idle_timeout,
            aerovault_overlay_set_idle_timeout,
            aerovault_overlay_busy_acquire,
            aerovault_overlay_busy_release,
            // Cross-profile transfer commands
            cross_profile_commands::cross_profile_plan,
            cross_profile_commands::cross_profile_execute,
            cross_profile_commands::cross_profile_cancel,
            ai_stream::ai_chat_stream,
            ai_stream::ai_cancel_stream,
            ai::ollama_pull_model,
            ai::gemini_create_cache,
            ai::ollama_list_running,
            ai::kimi_create_cache,
            ai::kimi_upload_file,
            ai::deepseek_fim_complete,
            // Multi-protocol provider commands
            provider_commands::provider_connect,
            provider_commands::cancel_connection,
            provider_commands::cancel_remote_listing,
            provider_commands::provider_disconnect,
            provider_commands::provider_apply_crypt_overlay,
            provider_commands::crypt_generate_keyfile,
            provider_commands::provider_clear_crypt_overlay,
            provider_commands::provider_crypt_cwd_in_view,
            provider_commands::provider_check_connection,
            provider_commands::provider_probe_alive,
            provider_commands::provider_list_files,
            provider_commands::provider_change_dir,
            provider_commands::provider_go_up,
            provider_commands::provider_pwd,
            provider_commands::provider_download_file,
            provider_commands::provider_detect_aero_remote,
            provider_commands::provider_detect_archive_meta_remote,
            provider_commands::provider_download_folder,
            provider_commands::provider_upload_folder,
            provider_commands::provider_upload_file,
            provider_commands::provider_mkdir,
            provider_commands::provider_delete_file,
            provider_commands::provider_delete_dir,
            provider_commands::provider_rename,
            provider_commands::provider_server_copy,
            provider_commands::provider_supports_server_copy,
            provider_commands::provider_stat,
            provider_commands::provider_checksum,
            provider_commands::provider_keep_alive,
            provider_commands::provider_server_info,
            provider_commands::provider_file_size,
            provider_commands::provider_exists,
            // OAuth2 cloud provider commands
            provider_commands::oauth2_start_auth,
            provider_commands::oauth2_complete_auth,
            provider_commands::oauth2_connect,
            provider_commands::oauth2_full_auth,
            provider_commands::oauth2_has_tokens,
            provider_commands::oauth2_logout,
            // 4shared OAuth 1.0 commands
            provider_commands::fourshared_start_auth,
            provider_commands::fourshared_complete_auth,
            provider_commands::fourshared_full_auth,
            provider_commands::fourshared_connect,
            provider_commands::fourshared_has_tokens,
            provider_commands::fourshared_logout,
            provider_commands::zoho_list_trash,
            provider_commands::zoho_permanent_delete,
            provider_commands::zoho_restore_from_trash,
            provider_commands::zoho_list_team_labels,
            provider_commands::zoho_get_file_labels,
            provider_commands::zoho_add_file_label,
            provider_commands::zoho_remove_file_label,
            provider_commands::zoho_create_label,
            provider_commands::zoho_get_user_info,
            provider_commands::zoho_get_file_share_links,
            provider_commands::zoho_delete_share_link,
            provider_commands::zoho_create_native_document,
            provider_commands::jottacloud_move_to_trash,
            provider_commands::jottacloud_list_trash,
            provider_commands::jottacloud_restore_from_trash,
            provider_commands::jottacloud_permanent_delete,
            provider_commands::mega_move_to_trash,
            provider_commands::mega_list_trash,
            provider_commands::mega_restore_from_trash,
            provider_commands::mega_permanent_delete,
            provider_commands::filelu_set_file_password,
            provider_commands::filelu_set_file_privacy,
            provider_commands::filelu_clone_file,
            provider_commands::filelu_set_folder_password,
            provider_commands::filelu_set_folder_settings,
            provider_commands::filelu_list_deleted,
            provider_commands::filelu_restore_file,
            provider_commands::filelu_restore_folder,
            provider_commands::filelu_permanent_delete,
            provider_commands::filelu_remote_url_upload,
            providers::koofr::koofr_list_trash,
            providers::koofr::koofr_restore_trash,
            providers::koofr::koofr_empty_trash,
            providers::webdav::webdav_list_trash,
            providers::webdav::webdav_restore_trash,
            providers::webdav::webdav_delete_trash,
            providers::webdav::webdav_empty_trash,
            provider_commands::google_drive_trash_file,
            provider_commands::google_drive_list_trash,
            provider_commands::google_drive_restore_from_trash,
            provider_commands::google_drive_permanent_delete,
            provider_commands::opendrive_list_trash,
            provider_commands::opendrive_restore_from_trash,
            provider_commands::opendrive_permanent_delete,
            provider_commands::opendrive_empty_trash,
            provider_commands::opendrive_set_path_privacy,
            provider_commands::opendrive_set_path_access,
            provider_commands::fourshared_set_path_privacy,
            provider_commands::yandex_list_trash,
            provider_commands::yandex_restore_from_trash,
            provider_commands::yandex_permanent_delete,
            provider_commands::yandex_empty_trash,
            provider_commands::google_drive_set_starred,
            provider_commands::google_drive_list_comments,
            provider_commands::google_drive_add_comment,
            provider_commands::google_drive_delete_comment,
            provider_commands::google_drive_set_properties,
            provider_commands::google_drive_set_description,
            provider_commands::dropbox_list_trash,
            provider_commands::dropbox_restore_from_trash,
            provider_commands::dropbox_permanent_delete,
            provider_commands::dropbox_set_tags,
            provider_commands::dropbox_get_tags,
            provider_commands::onedrive_list_trash,
            provider_commands::onedrive_trash_files,
            provider_commands::onedrive_restore_from_trash,
            provider_commands::onedrive_permanent_delete,
            provider_commands::box_list_trash,
            provider_commands::box_trash_files,
            provider_commands::box_restore_from_trash,
            provider_commands::box_permanent_delete,
            provider_commands::box_move_file,
            provider_commands::box_list_comments,
            provider_commands::box_add_comment,
            provider_commands::box_delete_comment,
            provider_commands::box_add_collaboration,
            provider_commands::box_remove_collaboration,
            provider_commands::box_set_watermark,
            provider_commands::box_remove_watermark,
            provider_commands::box_set_tags,
            provider_commands::box_lock_folder,
            provider_commands::box_unlock_folder,
            provider_commands::box_list_collaborations,
            provider_commands::box_list_folder_locks,
            provider_commands::provider_create_share_link,
            provider_commands::provider_share_link_capabilities,
            provider_commands::provider_remove_share_link,
            provider_commands::provider_list_share_links,
            provider_commands::provider_import_link,
            provider_commands::provider_compare_directories,
            provider_commands::provider_storage_info,
            provider_commands::mega_df_query,
            provider_commands::mega_webdav_url,
            provider_commands::provider_disk_usage,
            provider_commands::provider_calculate_folder_size,
            provider_commands::provider_cancel_folder_size,
            provider_commands::provider_scan_used,
            provider_commands::provider_cancel_used_scan,
            // GitHub-specific commands
            provider_commands::github_list_branches,
            provider_commands::github_get_info,
            provider_commands::github_create_pr,
            provider_commands::github_device_flow_start,
            provider_commands::github_device_flow_complete,
            provider_commands::github_app_token_from_pem,
            provider_commands::github_app_token_from_vault,
            provider_commands::github_get_app_credentials,
            provider_commands::github_store_pat,
            provider_commands::github_store_pat_from_held,
            provider_commands::github_load_oauth_token,
            provider_commands::github_get_pat,
            provider_commands::github_has_vault_pem,
            // GitHub Release management
            provider_commands::github_list_releases,
            provider_commands::github_list_release_assets,
            provider_commands::github_create_release,
            provider_commands::github_read_file,
            provider_commands::github_get_pages,
            provider_commands::github_list_pages_builds,
            provider_commands::github_trigger_pages_build,
            provider_commands::github_update_pages,
            provider_commands::github_pages_health,
            provider_commands::github_list_actions_runs,
            provider_commands::github_rerun_workflow,
            provider_commands::github_rerun_failed_jobs,
            provider_commands::github_cancel_workflow,
            provider_commands::github_upload_release_asset,
            provider_commands::github_delete_release,
            provider_commands::github_delete_release_asset,
            provider_commands::github_download_release_asset,
            provider_commands::github_get_release,
            provider_commands::github_batch_commit,
            provider_commands::github_batch_upload,
            provider_commands::github_batch_delete,
            provider_commands::github_check_local_sync,
            provider_commands::github_push_local,
            // GitLab-specific commands
            provider_commands::gitlab_list_branches,
            provider_commands::gitlab_get_info,
            provider_commands::gitlab_switch_branch,
            provider_commands::gitlab_batch_upload,
            provider_commands::gitlab_batch_delete,
            provider_commands::gitlab_list_releases,
            provider_commands::gitlab_list_release_assets,
            provider_commands::gitlab_create_release,
            provider_commands::gitlab_delete_release,
            provider_commands::gitlab_upload_release_asset,
            provider_commands::gitlab_delete_release_asset,
            provider_commands::gitlab_read_file,
            provider_commands::gitlab_download_release_asset,
            provider_commands::gitlab_create_merge_request,
            provider_commands::gitlab_get_web_url,
            // Filen Encrypted Notes
            provider_commands::filen_notes_list,
            provider_commands::filen_notes_create,
            provider_commands::filen_notes_get_content,
            provider_commands::filen_notes_edit_content,
            provider_commands::filen_notes_edit_title,
            provider_commands::filen_notes_change_type,
            provider_commands::filen_notes_trash,
            provider_commands::filen_notes_archive,
            provider_commands::filen_notes_restore,
            provider_commands::filen_notes_delete,
            provider_commands::filen_get_auth_version,
            provider_commands::filen_notes_toggle_favorite,
            provider_commands::filen_notes_toggle_pinned,
            provider_commands::filen_notes_history,
            provider_commands::filen_notes_history_restore,
            provider_commands::filen_notes_tags_list,
            provider_commands::filen_notes_tags_create,
            provider_commands::filen_notes_tags_rename,
            provider_commands::filen_notes_tags_delete,
            provider_commands::filen_notes_tag_note,
            provider_commands::filen_notes_untag_note,
            provider_commands::provider_find,
            provider_commands::provider_set_speed_limit,
            provider_commands::provider_get_speed_limit,
            provider_commands::provider_supports_resume,
            provider_commands::provider_resume_download,
            provider_commands::provider_resume_upload,
            // File versions
            provider_commands::provider_supports_versions,
            provider_commands::provider_list_versions,
            provider_commands::provider_download_version,
            provider_commands::provider_restore_version,
            provider_commands::provider_delete_version,
            // File locking
            provider_commands::provider_supports_locking,
            provider_commands::provider_lock_file,
            provider_commands::provider_unlock_file,
            // Thumbnails
            provider_commands::provider_supports_thumbnails,
            provider_commands::provider_get_thumbnail,
            // S3 Enterprise features
            provider_commands::s3_change_storage_class,
            provider_commands::s3_glacier_restore,
            provider_commands::s3_get_object_tags,
            provider_commands::s3_set_object_tags,
            provider_commands::s3_delete_object_tags,
            // S3 trash / version management (#266)
            provider_commands::s3_list_trash,
            provider_commands::s3_restore_from_trash,
            provider_commands::s3_empty_trash,
            // Azure Enterprise features
            provider_commands::azure_set_blob_tier,
            provider_commands::azure_list_deleted_blobs,
            provider_commands::azure_undelete_blob,
            // Internxt Trash
            provider_commands::internxt_list_trash,
            // pCloud Trash
            provider_commands::pcloud_list_trash,
            provider_commands::pcloud_restore_from_trash,
            provider_commands::pcloud_empty_trash,
            provider_commands::pcloud_permanently_delete_trash,
            // kDrive Trash
            provider_commands::kdrive_list_trash,
            provider_commands::kdrive_restore_from_trash,
            provider_commands::kdrive_permanently_delete_trash,
            provider_commands::kdrive_empty_trash,
            // Backblaze B2 native: hide / restore / permanent-delete
            provider_commands::b2_list_hidden,
            provider_commands::b2_restore_hidden,
            provider_commands::b2_permanent_delete,
            // Permissions / Advanced sharing
            provider_commands::provider_supports_permissions,
            provider_commands::provider_list_permissions,
            provider_commands::provider_add_permission,
            provider_commands::provider_remove_permission,
            // Multi-session provider commands
            session_commands::session_connect,
            session_commands::session_disconnect,
            session_commands::session_switch,
            session_commands::session_list,
            session_commands::session_info,
            session_commands::session_list_files,
            session_commands::session_change_dir,
            session_commands::session_mkdir,
            session_commands::session_delete,
            session_commands::session_rename,
            session_commands::session_download,
            session_commands::session_upload,
            session_commands::session_create_share_link,
            spawn_shell,
            pty_write,
            pty_resize,
            pty_close,
            ssh_shell_open,
            ssh_shell_write,
            ssh_shell_resize,
            ssh_shell_close,
            // Host key verification (TOFU UX)
            sftp_check_host_key,
            sftp_accept_host_key,
            sftp_remove_host_key,
            // Plugin system
            plugins::list_plugins,
            plugins::execute_plugin_tool,
            plugins::install_plugin,
            plugins::remove_plugin,
            plugins::trigger_plugin_hooks,
            // Plugin registry
            plugin_registry::fetch_plugin_registry,
            plugin_registry::install_plugin_from_registry,
            // Filesystem (Places Sidebar + AeroFile)
            filesystem::get_user_directories,
            filesystem::list_mounted_volumes,
            filesystem::list_subdirectories,
            filesystem::eject_volume,
            preview_provider_totp,
            filesystem::list_unmounted_partitions,
            filesystem::mount_partition,
            filesystem::get_file_properties,
            filesystem::calculate_folder_size,
            filesystem::delete_to_trash,
            filesystem::list_trash_items,
            filesystem::restore_trash_item,
            filesystem::empty_trash,
            filesystem::find_duplicate_files,
            filesystem::scan_disk_usage,
            filesystem::volumes_changed,
            // Mission Green Badge - File sync status tracking
            sync_badge::start_badge_server_cmd,
            sync_badge::stop_badge_server_cmd,
            sync_badge::set_file_badge,
            sync_badge::clear_file_badge,
            sync_badge::get_badge_status,
            sync_badge::install_shell_extension_cmd,
            sync_badge::uninstall_shell_extension_cmd,
            sync_badge::restart_file_manager_cmd,
            // Security Toolkit: Cyber Tools
            cyber_tools::hash_text,
            cyber_tools::hash_file,
            cyber_tools::compare_hashes,
            cyber_tools::crypto_encrypt_text,
            cyber_tools::crypto_decrypt_text,
            cyber_tools::generate_password,
            cyber_tools::generate_passphrase,
            cyber_tools::calculate_entropy,
            // TOTP 2FA
            totp::totp_setup_start,
            totp::totp_setup_verify,
            totp::totp_verify,
            totp::totp_status,
            totp::totp_enable,
            totp::totp_disable,
            totp::totp_load_secret,
            // Chat History SQLite
            chat_history::chat_history_init,
            chat_history::chat_history_list_sessions,
            chat_history::chat_history_get_session,
            chat_history::chat_history_create_session,
            chat_history::chat_history_save_message,
            chat_history::chat_history_update_session_title,
            chat_history::chat_history_delete_session,
            chat_history::chat_history_delete_sessions_bulk,
            chat_history::chat_history_clear_all,
            chat_history::chat_history_search,
            chat_history::chat_history_cleanup,
            chat_history::chat_history_stats,
            chat_history::chat_history_export_session,
            chat_history::chat_history_import,
            chat_history::chat_history_create_branch,
            chat_history::chat_history_switch_branch,
            chat_history::chat_history_delete_branch,
            chat_history::chat_history_save_branch_message,
            // File Tags SQLite
            file_tags::file_tags_list_labels,
            file_tags::file_tags_create_label,
            file_tags::file_tags_update_label,
            file_tags::file_tags_delete_label,
            file_tags::file_tags_set_tags,
            file_tags::file_tags_remove_tag,
            file_tags::file_tags_get_tags_for_files,
            file_tags::file_tags_get_files_by_label,
            file_tags::file_tags_update_path,
            file_tags::file_tags_delete_all_for_file,
            file_tags::file_tags_get_label_counts,
            // Vault History
            vault_history::vault_history_save,
            vault_history::vault_history_list,
            vault_history::vault_history_remove,
            vault_history::vault_history_clear,
            // Server Health Check
            server_health::server_health_check,
            server_health::server_health_check_batch,
            local_bridge::bridge_status,
            // Server Speed Test
            speedtest::speedtest_run,
            speedtest::speedtest_compare,
            speedtest::speedtest_cancel,
            speedtest::speedtest_history_record,
            speedtest::speedtest_history_list,
            speedtest::speedtest_history_summary,
            speedtest::speedtest_history_clear,
            // AeroImage
            image_edit::process_image,
            // InfiniCloud REST API
            infinicloud::infinicloud_discover,
            infinicloud::infinicloud_quota,
            // Mount Manager (T-MOUNT-MANAGER)
            mount_list,
            mount_save_config,
            mount_delete_config,
            mount_start,
            mount_stop,
            mount_open_in_explorer,
            vault_mount_start,
            vault_mount_stop,
            vault_mount_list,
            vault_mount_open,
            mount_suggest_path,
            mount_pick_drive_letter,
            mount_set_storage_mode,
            mount_install_autostart,
            mount_uninstall_autostart,
            mount_autostart_blocked,
            mount_open_quick,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ─── Tracing -> log bridge ────────────────────────────────────────────────
//
// tauri-plugin-log only captures `log::*` macros. Providers (S3, WebDAV,
// OAuth, sync, etc.) use `tracing::*` for structured events, so without
// this bridge the entire provider layer is invisible to the in-app
// DebugPanel and to the on-disk `aeroftp.log`. The bridge converts every
// tracing Event into a `log::Record` and forwards it to whatever logger
// `log::set_logger` installed (in our case, the tauri-plugin-log dispatch
// which fans out to stdout, the log file, and the Webview target).
//
// We only handle the message field and rely on `record_debug` for
// everything else: Arguments<'_>::Debug is identical to Display, so
// formatted log strings come through unchanged. Span entry/exit and span
// fields are intentionally dropped (no provider uses spans today).

struct TracingToLogBridge;

struct LogMessageVisitor(String);

impl tracing::field::Visit for LogMessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            // Arguments<'_>::Debug renders identical to Display, so the
            // formatted log line comes through without quoting.
            let _ = write!(&mut self.0, "{:?}", value);
        } else {
            let _ = write!(&mut self.0, " {}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(&mut self.0, " {}={}", field.name(), value);
        }
    }
}

impl tracing::Subscriber for TracingToLogBridge {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _id: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _id: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let meta = event.metadata();
        let level = match *meta.level() {
            tracing::Level::ERROR => log::Level::Error,
            tracing::Level::WARN => log::Level::Warn,
            tracing::Level::INFO => log::Level::Info,
            tracing::Level::DEBUG => log::Level::Debug,
            tracing::Level::TRACE => log::Level::Trace,
        };
        let mut visitor = LogMessageVisitor(String::new());
        event.record(&mut visitor);
        let msg = visitor.0.trim_start();
        log::logger().log(
            &log::Record::builder()
                .args(format_args!("{}", msg))
                .level(level)
                .target(meta.target())
                .build(),
        );
    }

    fn enter(&self, _id: &tracing::span::Id) {}
    fn exit(&self, _id: &tracing::span::Id) {}
}

#[cfg(test)]
mod window_size_heal_tests {
    use super::{restored_size_is_degenerate, MAIN_MIN_INNER_H, MAIN_MIN_INNER_W};

    #[test]
    fn zero_size_is_degenerate() {
        // The exact symptom from issue #290: restore_state applied a poisoned
        // {width: 0, height: 0} record, so the window opened at 0x0.
        assert!(restored_size_is_degenerate(0.0, 0.0));
    }

    #[test]
    fn below_minimum_either_axis_is_degenerate() {
        assert!(restored_size_is_degenerate(MAIN_MIN_INNER_W - 1.0, 800.0));
        assert!(restored_size_is_degenerate(1200.0, MAIN_MIN_INNER_H - 1.0));
    }

    #[test]
    fn at_or_above_minimum_is_kept() {
        assert!(!restored_size_is_degenerate(
            MAIN_MIN_INNER_W,
            MAIN_MIN_INNER_H
        ));
        assert!(!restored_size_is_degenerate(1540.0, 1050.0));
    }
}

#[cfg(test)]
mod extract_intent_tests {
    use super::{archive_extract_stem, parse_extract_intent, unique_extract_dir_with};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    #[test]
    fn stem_strips_single_and_multi_extensions() {
        assert_eq!(archive_extract_stem("photos.zip"), "photos");
        assert_eq!(archive_extract_stem("backup.7z"), "backup");
        assert_eq!(archive_extract_stem("data.rar"), "data");
        assert_eq!(archive_extract_stem("logs.tar"), "logs");
        assert_eq!(archive_extract_stem("logs.tar.gz"), "logs");
        assert_eq!(archive_extract_stem("logs.TAR.GZ"), "logs");
        assert_eq!(archive_extract_stem("logs.tar.xz"), "logs");
        assert_eq!(archive_extract_stem("logs.tar.bz2"), "logs");
        assert_eq!(archive_extract_stem("vault.aerovault"), "vault");
        assert_eq!(archive_extract_stem("bundle.aerozip"), "bundle");
    }

    #[test]
    fn stem_keeps_dotted_basename_and_unknown_extension() {
        // A versioned name keeps its internal dots, only the archive ext goes.
        assert_eq!(archive_extract_stem("release.1.2.3.zip"), "release.1.2.3");
        // Dotfile with no real extension is returned as-is.
        assert_eq!(archive_extract_stem(".bashrc"), ".bashrc");
        // No extension at all.
        assert_eq!(archive_extract_stem("noext"), "noext");
    }

    #[test]
    fn unique_dir_returns_plain_stem_when_free() {
        let dir = unique_extract_dir_with(Path::new("/out"), "photos.zip", |_| false).unwrap();
        assert_eq!(dir, PathBuf::from("/out/photos"));
    }

    #[test]
    fn unique_dir_skips_existing_with_numbered_suffix() {
        // /out/photos and /out/photos (2) already exist; expect (3).
        let taken: HashSet<PathBuf> = [
            PathBuf::from("/out/photos"),
            PathBuf::from("/out/photos (2)"),
        ]
        .into_iter()
        .collect();
        let dir = unique_extract_dir_with(Path::new("/out"), "photos.zip", |p| taken.contains(p))
            .unwrap();
        assert_eq!(dir, PathBuf::from("/out/photos (3)"));
    }

    #[test]
    fn parse_intent_recognizes_both_verbs_or_none() {
        // No verb -> None (the path is unused here, so a bare argv is fine).
        let none = parse_extract_intent(&["aeroftp".to_string(), "/some/file.zip".to_string()]);
        assert!(none.is_none());
        // Unknown flag -> None.
        let other = parse_extract_intent(&["aeroftp".to_string(), "--autostart".to_string()]);
        assert!(other.is_none());
        // A real verb with a missing path argument -> None (no panic).
        let dangling = parse_extract_intent(&["aeroftp".to_string(), "--extract-here".to_string()]);
        assert!(dangling.is_none());
    }
}

#[cfg(test)]
mod compress_store_tests {
    use super::*;

    /// Deterministic incompressible bytes (LCG), so deflate cannot shrink them.
    fn incompressible(n: usize, mut x: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (x >> 33) as u8
            })
            .collect()
    }

    #[test]
    fn store_decision_rejects_deflate_expansion() {
        // Highly compressible: deflate wins, do not store.
        assert!(!zip_entry_should_store(&vec![0u8; 20_000], 6));
        // Incompressible: deflate cannot help, store instead of expanding.
        assert!(zip_entry_should_store(
            &incompressible(20_000, 0x1234_5678),
            6
        ));
        // Level 0 means store everything.
        assert!(zip_entry_should_store(&vec![0u8; 20_000], 0));
        // Empty payload at a real level is a no-op (no expansion to avoid).
        assert!(!zip_entry_should_store(&[], 6));
    }

    #[test]
    fn deflate_level_mapping_pins_only_level_1() {
        // Level 0 keeps its store semantics, level 1 lands on the backend fast
        // slot instead of zlib-rs deflate_quick, everything else is identity.
        assert_eq!(deflate_effective_level(0), 0);
        assert_eq!(deflate_effective_level(1), 2);
        for lvl in 2..=9 {
            assert_eq!(deflate_effective_level(lvl), lvl);
        }
    }

    #[tokio::test]
    async fn compress_files_stores_incompressible_deflates_compressible() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let inc = incompressible(100_000, 0x9e37_79b9);
        let cmp = vec![b'A'; 100_000];
        let inc_path = dir.path().join("random.bin");
        let cmp_path = dir.path().join("zeros.txt");
        std::fs::write(&inc_path, &inc).unwrap();
        std::fs::write(&cmp_path, &cmp).unwrap();
        let out = dir.path().join("out.zip");

        compress_files_impl(
            vec![
                inc_path.to_string_lossy().to_string(),
                cmp_path.to_string_lossy().to_string(),
            ],
            out.to_string_lossy().to_string(),
            None,
            Some(6),
            None,
        )
        .await
        .expect("compress");

        let f = std::fs::File::open(&out).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let (mut saw_stored, mut saw_deflated) = (false, false);
        for i in 0..zip.len() {
            let mut e = zip.by_index(i).unwrap();
            let name = e.name().to_string();
            if name.contains("random") {
                assert_eq!(
                    e.compression(),
                    zip::CompressionMethod::Stored,
                    "incompressible entry must be stored, not expanded"
                );
                saw_stored = true;
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).unwrap();
                assert_eq!(buf, inc, "stored entry must round-trip byte-identical");
            } else if name.contains("zeros") {
                assert_eq!(
                    e.compression(),
                    zip::CompressionMethod::Deflated,
                    "compressible entry must still deflate"
                );
                saw_deflated = true;
            }
        }
        assert!(saw_stored && saw_deflated, "both entries must be present");
        // The stored entry must not have inflated the payload.
        assert!(
            std::fs::metadata(&out).unwrap().len() < (inc.len() + cmp.len()) as u64,
            "archive must not be larger than the raw inputs"
        );
    }
}

#[cfg(test)]
mod overlay_helpers_tests {
    use super::*;
    use secrecy::SecretString;
    use std::time::Duration;

    /// C-BUG-1 regression: an rclone-crypt multi-block file must still decrypt
    /// after a RENAME (encrypted-name mv) followed by a fresh download. Mirrors
    /// the GUI backend ops exactly: encrypt content -> upload under the encrypted
    /// name -> rename (mv) -> download_to_bytes -> decrypt. Ignored by default
    /// because it hits the live SFTP lab server. Run with:
    ///   AEROFTP_MASTER_PASSWORD=x cargo test --lib \
    ///     rclone_overlay_rename_then_download_multiblock -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn rclone_overlay_rename_then_download_multiblock_roundtrip() {
        use crate::providers::{SftpConfig, SftpProvider, StorageProvider};

        let key = format!("{}/.ssh/id_ed25519", std::env::var("HOME").unwrap());
        let config = SftpConfig {
            host: "203.0.113.10".to_string(),
            port: 22,
            username: "tester".to_string(),
            password: None,
            private_key_path: Some(key),
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: true,
        };
        let mut p = SftpProvider::new(config);
        p.connect().await.expect("connect");

        let (name_key, data_key, name_tweak) =
            rclone_crypt::derive_keys_with_tweak("test-passphrase", "").unwrap();

        let dir = "/home/tester/rclone_rename_regression";
        let _ = p.mkdir(dir).await;

        // Multi-block plaintext: 200 KiB spans several 64 KiB rclone blocks.
        let plaintext: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
        let encrypted = rclone_crypt::encrypt_file_content(&plaintext, &data_key).unwrap();

        let enc_old = rclone_crypt::encrypt_name(&name_key, &name_tweak, "regr.pdf").unwrap();
        let enc_new =
            rclone_crypt::encrypt_name(&name_key, &name_tweak, "regr-renamed.pdf").unwrap();
        let path_old = format!("{}/{}", dir, enc_old);
        let path_new = format!("{}/{}", dir, enc_new);

        let tmp = std::env::temp_dir().join("rclone_regr_upload.bin");
        tokio::fs::write(&tmp, &encrypted).await.unwrap();
        p.upload(tmp.to_str().unwrap(), &path_old, None)
            .await
            .expect("upload");
        let _ = tokio::fs::remove_file(&tmp).await;

        // Pre-rename download must round-trip.
        let dl0 = p
            .download_to_bytes(&path_old)
            .await
            .expect("download pre-rename");
        let dec0 = rclone_crypt::decrypt_file_content(&dl0, &data_key).expect("decrypt pre-rename");
        assert_eq!(dec0, plaintext, "pre-rename content mismatch");

        // Rename (encrypted-name mv), exactly like the CryptOverlayProvider /
        // provider rename maps both ends through the encrypted name codec.
        p.rename(&path_old, &path_new).await.expect("rename");

        // Post-rename download must still round-trip: this is C-BUG-1.
        let dl1 = p
            .download_to_bytes(&path_new)
            .await
            .expect("download post-rename");
        let dec1 = rclone_crypt::decrypt_file_content(&dl1, &data_key)
            .expect("decrypt post-rename (C-BUG-1 magic header)");
        assert_eq!(dec1, plaintext, "post-rename content mismatch");

        let _ = p.delete(&path_new).await;
    }

    fn make_session(
        idle_timeout_secs: u64,
        last_activity: Instant,
        source: &str,
    ) -> AeroVaultOverlaySessionRuntime {
        AeroVaultOverlaySessionRuntime {
            vault_path: "/tmp/x.aerovault".to_string(),
            password: SecretString::new("pw".to_string().into_boxed_str()),
            version: 2,
            source: source.to_string(),
            remote_vault_path: None,
            remote_local_path: None,
            current_dir: String::new(),
            idle_timeout_secs,
            last_activity,
            busy_holds: 0,
        }
    }

    // --- normalize_overlay_relative_path ---

    #[test]
    fn normalize_strips_outer_slashes_and_keeps_inner() {
        assert_eq!(normalize_overlay_relative_path("/a/b/").unwrap(), "a/b");
        assert_eq!(normalize_overlay_relative_path("a/b").unwrap(), "a/b");
    }

    #[test]
    fn normalize_rejects_traversal_and_null() {
        assert!(normalize_overlay_relative_path("a/../b").is_err());
        assert!(normalize_overlay_relative_path("a/./b").is_err());
        assert!(normalize_overlay_relative_path("a/\0/b").is_err());
        assert!(normalize_overlay_relative_path("a//b").is_err());
    }

    #[test]
    fn normalize_handles_empty_and_root() {
        assert_eq!(normalize_overlay_relative_path("").unwrap(), "");
        assert_eq!(normalize_overlay_relative_path("/").unwrap(), "");
        assert_eq!(normalize_overlay_relative_path("///").unwrap(), "");
    }

    #[test]
    fn normalize_normalizes_backslashes() {
        assert_eq!(normalize_overlay_relative_path("a\\b\\c").unwrap(), "a/b/c");
    }

    // --- overlay_display_path ---

    #[test]
    fn display_path_root_and_nested() {
        assert_eq!(overlay_display_path(""), "/");
        assert_eq!(overlay_display_path("a/b"), "/a/b");
    }

    // --- overlay_join ---

    #[test]
    fn join_handles_root_and_nested() {
        assert_eq!(overlay_join("", "x"), "x");
        assert_eq!(overlay_join("a", "x"), "a/x");
        assert_eq!(overlay_join("a/b", "x"), "a/b/x");
    }

    // --- resolve_overlay_target ---

    #[test]
    fn resolve_target_dot_keeps_current() {
        assert_eq!(resolve_overlay_target("a/b", ".").unwrap(), "a/b");
        assert_eq!(resolve_overlay_target("a/b", "").unwrap(), "a/b");
    }

    #[test]
    fn resolve_target_dot_dot_goes_up() {
        assert_eq!(resolve_overlay_target("a/b", "..").unwrap(), "a");
        assert_eq!(resolve_overlay_target("a", "..").unwrap(), "");
        assert_eq!(resolve_overlay_target("", "..").unwrap(), "");
    }

    #[test]
    fn resolve_target_absolute_resets_to_root() {
        assert_eq!(resolve_overlay_target("a/b", "/x/y").unwrap(), "x/y");
        assert_eq!(resolve_overlay_target("a/b", "/").unwrap(), "");
    }

    #[test]
    fn resolve_target_relative_appends() {
        assert_eq!(resolve_overlay_target("", "x").unwrap(), "x");
        assert_eq!(resolve_overlay_target("a", "x").unwrap(), "a/x");
        assert_eq!(resolve_overlay_target("a/b", "x/y").unwrap(), "a/b/x/y");
    }

    #[test]
    fn resolve_target_rejects_relative_traversal() {
        assert!(resolve_overlay_target("a", "../b").is_err());
    }

    // --- normalize_overlay_idle_timeout_secs ---

    #[test]
    fn normalize_timeout_uses_default_on_none_when_no_persist() {
        // best-effort: persisted file may or may not exist in dev env, but the
        // value is always clamped into the allowed range.
        let v = normalize_overlay_idle_timeout_secs(None);
        assert!((OVERLAY_IDLE_TIMEOUT_MIN_SECS..=OVERLAY_IDLE_TIMEOUT_MAX_SECS).contains(&v));
    }

    #[test]
    fn normalize_timeout_clamps_low_input() {
        assert_eq!(
            normalize_overlay_idle_timeout_secs(Some(0)),
            OVERLAY_IDLE_TIMEOUT_MIN_SECS
        );
        assert_eq!(
            normalize_overlay_idle_timeout_secs(Some(5)),
            OVERLAY_IDLE_TIMEOUT_MIN_SECS
        );
    }

    #[test]
    fn normalize_timeout_clamps_high_input() {
        assert_eq!(
            normalize_overlay_idle_timeout_secs(Some(u64::MAX)),
            OVERLAY_IDLE_TIMEOUT_MAX_SECS
        );
    }

    #[test]
    fn normalize_timeout_passes_in_range() {
        assert_eq!(normalize_overlay_idle_timeout_secs(Some(900)), 900);
    }

    // --- drain_expired_overlay_sessions ---

    #[test]
    fn drain_empty_map_returns_empty() {
        let mut sessions: HashMap<String, AeroVaultOverlaySessionRuntime> = HashMap::new();
        let evicted = drain_expired_overlay_sessions(&mut sessions, Instant::now());
        assert!(evicted.is_empty());
    }

    #[test]
    fn drain_evicts_only_expired_sessions() {
        let now = Instant::now();
        let mut sessions: HashMap<String, AeroVaultOverlaySessionRuntime> = HashMap::new();
        // Fresh: last_activity = now - 5s, idle_timeout = 60s -> NOT expired.
        sessions.insert(
            "fresh".to_string(),
            make_session(60, now - Duration::from_secs(5), "local"),
        );
        // Stale: last_activity = now - 90s, idle_timeout = 60s -> expired.
        sessions.insert(
            "stale".to_string(),
            make_session(60, now - Duration::from_secs(90), "remote"),
        );
        let mut evicted = drain_expired_overlay_sessions(&mut sessions, now);
        evicted.sort();
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, "stale");
        assert_eq!(evicted[0].1, "remote");
        assert!(sessions.contains_key("fresh"));
        assert!(!sessions.contains_key("stale"));
    }

    #[test]
    fn drain_evicts_all_when_all_expired() {
        let now = Instant::now();
        let mut sessions: HashMap<String, AeroVaultOverlaySessionRuntime> = HashMap::new();
        sessions.insert(
            "a".to_string(),
            make_session(30, now - Duration::from_secs(120), "local"),
        );
        sessions.insert(
            "b".to_string(),
            make_session(30, now - Duration::from_secs(60), "local"),
        );
        let evicted = drain_expired_overlay_sessions(&mut sessions, now);
        assert_eq!(evicted.len(), 2);
        assert!(sessions.is_empty());
    }

    #[test]
    fn drain_keeps_session_at_exact_boundary() {
        // duration_since().as_secs() floors, so 60s with idle=60 is NOT > 60.
        let now = Instant::now();
        let mut sessions: HashMap<String, AeroVaultOverlaySessionRuntime> = HashMap::new();
        sessions.insert(
            "edge".to_string(),
            make_session(60, now - Duration::from_secs(60), "local"),
        );
        let evicted = drain_expired_overlay_sessions(&mut sessions, now);
        assert!(evicted.is_empty());
        assert!(sessions.contains_key("edge"));
    }

    #[test]
    fn drain_skips_busy_session_even_when_expired() {
        // Z.3.6 busy-lock: a session with `busy_holds > 0` must not be
        // evicted even past the idle timeout so the planner can drive a
        // long-running batch transfer without losing its overlay
        // handle mid-flight.
        let now = Instant::now();
        let mut sessions: HashMap<String, AeroVaultOverlaySessionRuntime> = HashMap::new();
        let mut busy = make_session(30, now - Duration::from_secs(300), "local");
        busy.busy_holds = 1;
        sessions.insert("busy".to_string(), busy);
        sessions.insert(
            "free".to_string(),
            make_session(30, now - Duration::from_secs(300), "local"),
        );
        let evicted = drain_expired_overlay_sessions(&mut sessions, now);
        assert_eq!(evicted.len(), 1, "free session evicts, busy stays");
        assert_eq!(evicted[0].0, "free");
        assert!(sessions.contains_key("busy"));
        assert!(!sessions.contains_key("free"));
    }

    #[test]
    fn drain_evicts_busy_session_after_release() {
        // Releasing the busy hold (counter back to 0) lets the next
        // sweep evict the session normally.
        let now = Instant::now();
        let mut sessions: HashMap<String, AeroVaultOverlaySessionRuntime> = HashMap::new();
        let mut session = make_session(30, now - Duration::from_secs(120), "local");
        session.busy_holds = 1;
        sessions.insert("s".to_string(), session);
        assert!(drain_expired_overlay_sessions(&mut sessions, now).is_empty());
        if let Some(s) = sessions.get_mut("s") {
            s.busy_holds = 0;
        }
        let evicted = drain_expired_overlay_sessions(&mut sessions, now);
        assert_eq!(evicted.len(), 1);
        assert!(sessions.is_empty());
    }
}

#[cfg(test)]
mod sevenz_mhe_tests {
    use super::{compress_7z_core, extract_7z_core, is_7z_encrypted_core};

    // -mhe=on: a password-protected 7z must hide the filenames (encrypted
    // header), not just the file contents. This is the gap that the migration to
    // sevenz-rust2 closes (the old sevenz-rust 0.6 encrypted only the content
    // stream and left the names in the clear).
    #[tokio::test]
    async fn password_7z_encrypts_header_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        // A distinctive filename we can search for in the raw archive bytes.
        let marker = "topsecret_filename_marker.txt";
        let src = dir.path().join(marker);
        let content = b"hello encrypted-header roundtrip";
        std::fs::write(&src, content).unwrap();
        let out = dir.path().join("bundle.7z");

        compress_7z_core(
            vec![src.to_string_lossy().to_string()],
            out.to_string_lossy().to_string(),
            Some("pw-correct-horse".to_string()),
            None,
            Some(true), // opt in to header (filename) encryption
            None,       // default (LZMA2) advanced options
        )
        .await
        .expect("compress 7z with password");

        // The filename must NOT appear anywhere in the archive bytes: that is the
        // whole point of header encryption (-mhe). With content-only encryption
        // the name would be present in the cleartext header.
        let bytes = std::fs::read(&out).unwrap();
        let marker_bytes = marker.as_bytes();
        assert!(
            !bytes.windows(marker_bytes.len()).any(|w| w == marker_bytes),
            "filename leaked into the archive header: header was not encrypted"
        );

        // The encryption probe used by the CLI fast path must report it locked.
        assert!(is_7z_encrypted_core(out.to_string_lossy().to_string())
            .await
            .expect("probe encrypted"));

        // Extracting with the correct password reconstructs the original file.
        let dest = dir.path().join("out");
        extract_7z_core(
            out.to_string_lossy().to_string(),
            dest.to_string_lossy().to_string(),
            Some("pw-correct-horse".to_string()),
            false,
        )
        .await
        .expect("extract 7z with password");
        let restored = std::fs::read(dest.join(marker)).unwrap();
        assert_eq!(restored, content);
    }

    // The dialog's Fast/Normal/Maximum buttons (and the CLI's --level) must
    // actually reach the LZMA2 encoder. Before the fix compress_7z_impl ignored
    // the level and always used the library default preset, so every level
    // produced a byte-identical archive. We prove the level is honoured with a
    // payload that has a long-range repeat only a large dictionary can
    // deduplicate: level 1 has a 1 MiB window and cannot see across the 1.5 MiB
    // gap, level 9 has a 64 MiB window and can, so its archive is markedly
    // smaller. Run without a password so the size reflects compression, not the
    // AES layer (which makes any output look random regardless of the level).
    #[tokio::test]
    async fn compression_level_changes_archive_size() {
        let dir = tempfile::tempdir().unwrap();

        // 1.5 MiB of deterministic pseudo-random (incompressible) bytes,
        // repeated once. The repeat sits 1.5 MiB after the original, beyond
        // level 1's 1 MiB window but well inside level 9's 64 MiB window.
        let block_len = 3 * 512 * 1024usize; // 1.5 MiB
        let mut block = vec![0u8; block_len];
        let mut state: u64 = 0x2545F4914F6CDD1D; // arbitrary non-zero seed
        for b in block.iter_mut() {
            // xorshift64*: cheap, dependency-free, defeats LZMA on its own.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            *b = (state.wrapping_mul(0x2545F4914F6CDD1D) >> 56) as u8;
        }
        let mut data = block.clone();
        data.extend_from_slice(&block);
        let src = dir.path().join("payload.bin");
        std::fs::write(&src, &data).unwrap();

        let compress_at = |level: Option<i64>, name: &str| {
            let src = src.to_string_lossy().to_string();
            let out = dir.path().join(name).to_string_lossy().to_string();
            async move {
                compress_7z_core(vec![src], out.clone(), None, level, None, None)
                    .await
                    .expect("compress 7z");
                std::fs::metadata(&out).unwrap().len()
            }
        };

        let size_fast = compress_at(Some(1), "lvl1.7z").await;
        let size_max = compress_at(Some(9), "lvl9.7z").await;

        // Level 9 must beat level 1 by a clear margin (the long-range dedup
        // roughly halves the output). The margin makes the test fail loudly if
        // the level ever stops reaching the encoder again.
        assert!(
            size_max + size_max / 5 < size_fast,
            "compression level was not honoured: level 1 = {size_fast} B, \
             level 9 = {size_max} B (expected level 9 clearly smaller)"
        );

        // An unset level must land on the 7-Zip "Normal" preset (5), byte for
        // byte: the T1c preset buttons rely on 5 being the real default.
        let size_default = compress_at(None, "default.7z").await;
        let size_normal = compress_at(Some(5), "lvl5.7z").await;
        assert_eq!(
            size_default, size_normal,
            "unset level must default to 5 (Normal): default = {size_default} B, \
             level 5 = {size_normal} B"
        );
    }
}

#[cfg(test)]
mod standalone_stream_tests {
    use super::{compress_single_core, extract_single_as_core, extract_single_core};
    use std::io::Read;

    // User-facing DEFLATE level 1 is pinned to backend level 2 because zlib-rs
    // maps backend level 1 to deflate_quick, ~40% larger than `gzip -1` on
    // text (#406). Byte equality with level 2 proves the pin reaches the
    // encoder; beating a raw backend-level-1 stream on the same corpus proves
    // the pin actually changes the emitted bytes for the better.
    #[tokio::test]
    async fn deflate_level_1_pinned_to_backend_level_2() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Deterministic word salad (the corpus family the +40% showed on):
        // repetitive enough to compress, varied enough that quick != fast.
        let words = [
            "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "packet", "bytes",
            "tunnel", "vault", "stream", "archive",
        ];
        let mut state: u64 = 0xb3c0_de01;
        let mut text = String::new();
        while text.len() < 2 * 1024 * 1024 {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let pick = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as usize;
            text.push_str(words[pick % words.len()]);
            text.push(' ');
        }
        let src = dir.path().join("corpus.txt");
        std::fs::write(&src, text.as_bytes()).unwrap();
        let src = src.to_string_lossy().to_string();

        let gz_at = |level: i64, name: &str| {
            let out = dir.path().join(name).to_string_lossy().to_string();
            let src = src.clone();
            async move {
                compress_single_core(src, out.clone(), "gz".to_string(), Some(level))
                    .await
                    .unwrap_or_else(|e| panic!("compress gz L{level}: {e}"));
                std::fs::read(&out).unwrap()
            }
        };
        let l1 = gz_at(1, "l1.gz").await;
        let l2 = gz_at(2, "l2.gz").await;
        assert_eq!(
            l1, l2,
            "user level 1 must emit the backend level 2 stream, byte for byte"
        );

        // The raw backend level 1 (deflate_quick) stream on the same corpus.
        let mut quick = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(1));
        quick.write_all(text.as_bytes()).unwrap();
        let quick = quick.finish().unwrap();
        assert!(
            l1.len() < quick.len(),
            "pinned level 1 ({} B) must beat deflate_quick ({} B) on text",
            l1.len(),
            quick.len()
        );
    }

    // gz/xz/bz2 as standalone single-file streams: each must shrink a
    // compressible payload and round-trip back to the exact original bytes. If
    // any codec were miswired (wrong encoder, truncated finish, dropped level)
    // the byte comparison fails loudly.
    #[tokio::test]
    async fn standalone_gz_xz_bz2_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        // Highly compressible (a repeated line), so `output < input` is a real
        // signal that compression actually happened for every codec.
        let original = "AeroFTP standalone stream test payload.\n"
            .repeat(4096)
            .into_bytes();
        let src = dir.path().join("payload.txt");
        std::fs::write(&src, &original).unwrap();
        let src = src.to_string_lossy().to_string();

        for (fmt, ext) in [("gz", "gz"), ("xz", "xz"), ("bz2", "bz2")] {
            let out = dir
                .path()
                .join(format!("payload.{ext}"))
                .to_string_lossy()
                .to_string();
            let returned = compress_single_core(src.clone(), out.clone(), fmt.to_string(), Some(5))
                .await
                .unwrap_or_else(|e| panic!("compress {fmt}: {e}"));
            assert_eq!(returned, out, "{fmt}: core must return the output path");

            let compressed = std::fs::read(&out).unwrap();
            assert!(
                compressed.len() < original.len(),
                "{fmt}: compressed ({}) not smaller than input ({})",
                compressed.len(),
                original.len()
            );

            // Decompress with the matching decoder and compare byte-for-byte.
            let mut restored = Vec::new();
            match fmt {
                "gz" => {
                    flate2::read::GzDecoder::new(&compressed[..])
                        .read_to_end(&mut restored)
                        .unwrap();
                }
                "xz" => {
                    xz2::read::XzDecoder::new(&compressed[..])
                        .read_to_end(&mut restored)
                        .unwrap();
                }
                "bz2" => {
                    bzip2::read::BzDecoder::new(&compressed[..])
                        .read_to_end(&mut restored)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(restored, original, "{fmt}: round-trip mismatch");
        }
    }

    // Full create -> extract round-trip THROUGH the real extract path
    // (`extract_single_core`, the code that backs the GUI/CLI/AI): a file
    // compressed by AeroFTP must re-open to the exact original bytes. This closes
    // the create-without-open asymmetry this change fixes.
    #[tokio::test]
    async fn standalone_extract_core_roundtrips_to_original() {
        let dir = tempfile::tempdir().unwrap();
        let original = "single-stream extract round-trip\n"
            .repeat(2048)
            .into_bytes();
        let src = dir.path().join("doc.txt");
        std::fs::write(&src, &original).unwrap();
        let src = src.to_string_lossy().to_string();

        for (fmt, ext) in [("gz", "gz"), ("xz", "xz"), ("bz2", "bz2")] {
            let archive = dir
                .path()
                .join(format!("doc.txt.{ext}"))
                .to_string_lossy()
                .to_string();
            compress_single_core(src.clone(), archive.clone(), fmt.to_string(), Some(5))
                .await
                .unwrap_or_else(|e| panic!("compress {fmt}: {e}"));

            let outdir = dir.path().join(format!("out_{ext}"));
            std::fs::create_dir_all(&outdir).unwrap();
            let returned =
                extract_single_core(archive.clone(), outdir.to_string_lossy().to_string(), false)
                    .await
                    .unwrap_or_else(|e| panic!("extract {fmt}: {e}"));
            assert_eq!(
                returned,
                outdir.to_string_lossy(),
                "{fmt}: returns the destination dir"
            );

            // Member name = archive name minus only the codec extension: "doc.txt".
            let restored = std::fs::read(outdir.join("doc.txt"))
                .unwrap_or_else(|e| panic!("{fmt}: member doc.txt missing: {e}"));
            assert_eq!(restored, original, "{fmt}: extract round-trip mismatch");
        }
    }

    // A forced `--archive-format` codec must be honored even when the file name's
    // extension does not match, mirroring the tar lane's `extract_*_as_core`.
    #[tokio::test]
    async fn standalone_extract_as_core_honors_forced_codec() {
        let dir = tempfile::tempdir().unwrap();
        let original = b"forced codec payload".to_vec();
        let src = dir.path().join("f.txt");
        std::fs::write(&src, &original).unwrap();
        // Compress as gz but give it a neutral name the sniffer will not match.
        let archive = dir.path().join("blob.bin").to_string_lossy().to_string();
        compress_single_core(
            src.to_string_lossy().to_string(),
            archive.clone(),
            "gz".to_string(),
            Some(5),
        )
        .await
        .unwrap();
        let outdir = dir.path().join("out");
        std::fs::create_dir_all(&outdir).unwrap();
        extract_single_as_core(
            archive,
            outdir.to_string_lossy().to_string(),
            false,
            "gz".to_string(),
        )
        .await
        .unwrap();
        // No codec extension on the name -> safe fallback member "blob.bin.out".
        let restored = std::fs::read(outdir.join("blob.bin.out")).unwrap();
        assert_eq!(restored, original);
    }

    // The reconstructed member name strips ONLY the trailing codec extension, so
    // `report.txt.gz` restores as `report.txt` (not `report`), and a name without
    // the codec extension never yields an empty file.
    #[test]
    fn member_name_strips_only_the_codec_extension() {
        use super::single_stream_member_name;
        assert_eq!(
            single_stream_member_name("report.txt.gz", "gz"),
            "report.txt"
        );
        assert_eq!(single_stream_member_name("data.xz", "xz"), "data");
        assert_eq!(
            single_stream_member_name("archive.tar.bz2", "bz2"),
            "archive.tar"
        );
        assert_eq!(single_stream_member_name("blob.bin", "gz"), "blob.bin.out");
    }

    // The probe classifies a bare gz/xz/bz2 as `single` but keeps `.tar.gz` on the
    // tar lane (ordering guard: tar is matched before the standalone codecs).
    #[tokio::test]
    async fn probe_classifies_single_but_keeps_tar_gz_on_tar_lane() {
        use super::extract_probe;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let gz = dir.path().join("x.gz");
        {
            let mut e = flate2::write::GzEncoder::new(
                std::fs::File::create(&gz).unwrap(),
                flate2::Compression::new(1),
            );
            e.write_all(b"hi").unwrap();
            e.finish().unwrap();
        }
        let p = extract_probe(gz.to_string_lossy().to_string())
            .await
            .unwrap();
        assert_eq!(p.kind.as_str(), "single");
        assert!(!p.encrypted);

        // A `.tar.gz` NAME must stay on the tar lane (bytes need not be a real tar
        // for the probe, which classifies by extension for the general formats).
        let targz = dir.path().join("y.tar.gz");
        std::fs::copy(&gz, &targz).unwrap();
        let p2 = extract_probe(targz.to_string_lossy().to_string())
            .await
            .unwrap();
        assert_eq!(p2.kind.as_str(), "tar");
    }

    // The multi-volume classifier must fire for every split-PART naming scheme
    // that ends in a numeric index (each falls to the generic error today), while
    // leaving whole archives untouched. `.partN.rar` is the load-bearing
    // exclusion: the unrar backend follows those volumes on the normal `.rar`
    // lane, so intercepting them would REGRESS working multi-part RAR. Pure, so it
    // can enumerate many names without touching the file system.
    #[test]
    fn is_multivolume_part_matches_split_parts_only() {
        use super::is_multivolume_part;
        // Split PARTS -> recognized (7-Zip/generic `.<ext>.NNN`, split ZIP `.zNN`,
        // old-style RAR `.rNN`).
        for name in [
            "foo.7z.001",
            "backup.7z.123",
            "photos.zip.001",
            "data.z01",
            "data.z09",
            "movie.r00",
            "movie.r15",
            "/tmp/some.dir/archive.7z.002",
        ] {
            assert!(
                is_multivolume_part(name),
                "expected split part to be detected: {name}"
            );
        }
        // Whole archives and non-parts -> left alone. `.partN.rar` MUST NOT match.
        for name in [
            "foo.7z",
            "foo.zip",
            "foo.rar",
            "movie.part1.rar",
            "movie.part01.rar",
            "movie.part001.rar",
            "clip.gz",
            "clip.xz",
            "clip.bz2",
            "backup.tar.gz",
            "weird.z",  // no digits
            "weird.z1", // only one digit; split ZIP is `.zNN`
            "blob.001", // bare numeric, unknown container -> stay on generic error
        ] {
            assert!(
                !is_multivolume_part(name),
                "expected non-part to be left alone: {name}"
            );
        }
    }

    // End to end through `extract_probe`: a real split PART is rejected with the
    // specific multi-volume message (never classified single/tar/zip), while a
    // real whole `.zip` and `.7z` still probe their kind (the early reject must
    // not swallow ordinary archives).
    #[tokio::test]
    async fn probe_rejects_multivolume_parts_but_keeps_real_archives() {
        use super::{compress_7z_core, extract_probe};
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Split parts: contents are irrelevant, they are rejected before any sniff.
        for part in ["set.7z.001", "set.z01"] {
            let p = dir.path().join(part);
            std::fs::write(&p, b"not a real archive, only a volume part").unwrap();
            let err = extract_probe(p.to_string_lossy().to_string())
                .await
                .expect_err("a split volume part must be rejected");
            // Also pins the exact wording: the source string is line-continued, so
            // this proves the `\`-join collapsed to a single space ("the volumes").
            assert!(
                err.contains("multi-volume")
                    && err.contains("rejoin the volumes with 7-Zip/WinRAR"),
                "unexpected message for {part}: {err}"
            );
        }

        // A real single-part ZIP still probes as "zip".
        let zip_path = dir.path().join("whole.zip");
        {
            use zip::write::SimpleFileOptions;
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            zw.start_file(
                "a.txt",
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            zw.write_all(b"hello").unwrap();
            zw.finish().unwrap();
        }
        let pz = extract_probe(zip_path.to_string_lossy().to_string())
            .await
            .unwrap();
        assert_eq!(pz.kind.as_str(), "zip");

        // A real single-part 7z still probes as "sevenz".
        let src = dir.path().join("payload.txt");
        std::fs::write(&src, b"payload for a whole 7z archive").unwrap();
        let sevenz_path = dir.path().join("whole.7z");
        compress_7z_core(
            vec![src.to_string_lossy().to_string()],
            sevenz_path.to_string_lossy().to_string(),
            None,
            Some(1),
            None,
            None,
        )
        .await
        .unwrap();
        let p7 = extract_probe(sevenz_path.to_string_lossy().to_string())
            .await
            .unwrap();
        assert_eq!(p7.kind.as_str(), "sevenz");
    }

    // A folder has no single-stream representation: the backend must refuse it
    // rather than silently produce an empty or garbage archive.
    #[tokio::test]
    async fn standalone_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("adir");
        std::fs::create_dir(&sub).unwrap();
        let out = dir.path().join("adir.gz").to_string_lossy().to_string();
        let err = compress_single_core(
            sub.to_string_lossy().to_string(),
            out,
            "gz".to_string(),
            Some(5),
        )
        .await
        .expect_err("a directory must be rejected");
        assert!(
            err.contains("single file"),
            "unexpected rejection message: {err}"
        );
    }

    // tar link safety: a benign in-root symlink must be recreated faithfully, while
    // a malicious symlink whose target escapes the extraction root (`../../.../etc/passwd`)
    // must be skipped and NEVER materialize a file outside the root. Guards against the
    // silent-drop bug (links became empty regular files) AND path-traversal via link target.
    #[cfg(unix)]
    #[test]
    fn tar_unpack_recreates_safe_symlink_and_skips_escaping_target() {
        use super::tar_unpack;

        let dir = tempfile::tempdir().unwrap();
        let tar_path = dir.path().join("links.tar");

        // Build a tar with: a real file, a benign in-root symlink to it, and an
        // out-of-root escaping symlink.
        {
            let file = std::fs::File::create(&tar_path).unwrap();
            let mut builder = tar::Builder::new(file);

            // Regular file "subdir/file.txt".
            let payload = b"hello from inside the root";
            let mut fh = tar::Header::new_gnu();
            fh.set_size(payload.len() as u64);
            fh.set_mode(0o644);
            fh.set_entry_type(tar::EntryType::Regular);
            fh.set_cksum();
            builder
                .append_data(&mut fh, "subdir/file.txt", &payload[..])
                .unwrap();

            // Benign symlink "link" -> "subdir/file.txt" (in-root, must be recreated).
            let mut sh = tar::Header::new_gnu();
            sh.set_entry_type(tar::EntryType::Symlink);
            sh.set_size(0);
            sh.set_cksum();
            builder
                .append_link(&mut sh, "link", "subdir/file.txt")
                .unwrap();

            // Malicious symlink "evil" -> "../../../../etc/passwd" (must be skipped).
            let mut eh = tar::Header::new_gnu();
            eh.set_entry_type(tar::EntryType::Symlink);
            eh.set_size(0);
            eh.set_cksum();
            builder
                .append_link(&mut eh, "evil", "../../../../etc/passwd")
                .unwrap();

            builder.finish().unwrap();
        }

        let out = dir.path().join("extracted");
        std::fs::create_dir_all(&out).unwrap();
        let reader: Box<dyn std::io::Read> = Box::new(std::fs::File::open(&tar_path).unwrap());
        let (dest, skipped) = tar_unpack(reader, &out).unwrap();
        // The destination stays a clean directory path (the CLI walks it to size
        // the extraction); skip notes travel in their own channel.
        assert_eq!(dest, out.to_string_lossy());
        let report = skipped.join("\n");

        // Benign symlink was recreated and resolves to the in-root file's bytes.
        let link_meta =
            std::fs::symlink_metadata(out.join("link")).expect("benign symlink must be created");
        assert!(
            link_meta.file_type().is_symlink(),
            "benign entry must be a symlink, not a regular file (silent-drop bug)"
        );
        let via_link = std::fs::read(out.join("link")).unwrap();
        assert_eq!(via_link, b"hello from inside the root");

        // Malicious symlink was NOT created inside the root...
        assert!(
            std::fs::symlink_metadata(out.join("evil")).is_err(),
            "escaping symlink must not be materialized"
        );
        // ...and the report surfaces the skip (not silent).
        assert!(
            report.contains("skipped (unsafe link target)") && report.contains("evil"),
            "skip must be reported, got: {report}"
        );
    }

    // A stream that expands past its declared size (a decompression bomb, or a
    // corrupt entry) must be rejected, and a truthful stream must pass through
    // unchanged. Guards the whole-archive extract paths (zip/7z/tar). (audit A-F1)
    #[test]
    fn copy_entry_bounded_rejects_overrun_and_passes_truthful() {
        use super::copy_entry_bounded;

        // Truthful: declared == actual.
        let src = b"exactly ten".to_vec(); // 11 bytes
        let mut out = Vec::new();
        let n = copy_entry_bounded(&mut &src[..], &mut out, src.len() as u64)
            .expect("a truthful stream must copy");
        assert_eq!(n, src.len() as u64);
        assert_eq!(out, src);

        // Bomb: the stream carries more than it declared -> rejected, no unbounded write.
        let big = vec![0u8; 4096];
        let mut out2 = Vec::new();
        let err = copy_entry_bounded(&mut &big[..], &mut out2, 8)
            .expect_err("an over-declared stream must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    // The single consolidated guard rejects traversal/absolute/drive/null/empty
    // but accepts a legitimate name that merely CONTAINS ".." as a substring
    // (the old archive_browse copy over-rejected `a..b.txt`). (audit A-F4)
    #[test]
    fn is_safe_archive_entry_component_precise() {
        use super::is_safe_archive_entry;
        // Rejected
        for bad in [
            "../etc/passwd",
            "a/../../b",
            "/abs/path",
            "\\abs\\path",
            "C:\\x",
            "with\0null",
            "",
        ] {
            assert!(!is_safe_archive_entry(bad), "must reject: {bad:?}");
        }
        // Accepted (".." only as a substring, not a path component)
        for ok in [
            "a..b.txt",
            "v1..2/report.txt",
            "normal/name.txt",
            "file.txt",
        ] {
            assert!(is_safe_archive_entry(ok), "must accept: {ok:?}");
        }
    }
}

#[cfg(test)]
mod zip_compression_method_tests {
    // TASK-2 interoperability guard. A `.zip` whose members use a non-Deflate
    // compression method (BZip2 / LZMA / Deflate64 / Zstd / Xz) must OPEN in
    // AeroFTP and yield the exact inner bytes. The `zip` crate was previously
    // built with only `deflate` + `aes-crypto`, so such members failed to open
    // with "unsupported compression"; TASK-2 enables these READ codecs (the
    // write path is unchanged: AeroFTP still emits Store/Deflate + AES, which
    // every native archiver opens). Each test asserts BOTH the member's declared
    // method and its exact bytes, so dropping a feature reds loudly.
    //
    // Two fixture strategies, chosen by the crate WRITER's capability (verified
    // against zip 8.6.0 write.rs):
    //  * BZip2 / Zstd / Xz -> the writer can emit them, so the fixture is built
    //    in-test (write member with method X, reopen, assert). No mainstream
    //    native archiver puts Zstd/Xz *inside* a .zip, so a round-trip is the
    //    only practical fixture for those.
    //  * LZMA / Deflate64 -> the writer refuses them ("Compressing X is not
    //    supported"), so we open a committed fixture produced by an external
    //    archiver (interop proof, direction native -> AeroFTP).
    use std::io::{Cursor, Read, Write};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipArchive, ZipWriter};

    // Write a single-member .zip in memory with the crate's own writer using the
    // given method; returns the raw archive bytes.
    fn write_member_zip(method: CompressionMethod, name: &str, payload: &[u8]) -> Vec<u8> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(method);
        zw.start_file(name, opts)
            .unwrap_or_else(|e| panic!("start_file {method:?}: {e}"));
        zw.write_all(payload)
            .unwrap_or_else(|e| panic!("write {method:?}: {e}"));
        zw.finish()
            .unwrap_or_else(|e| panic!("finish {method:?}: {e}"))
            .into_inner()
    }

    // Open a .zip and read one member: returns its declared method and bytes.
    // This is the exact read path behind AeroFTP's extract code; if the codec
    // feature is missing, `by_name`/read fails here instead of returning bytes.
    fn open_member(zip_bytes: &[u8], name: &str) -> (CompressionMethod, Vec<u8>) {
        let mut archive = ZipArchive::new(Cursor::new(zip_bytes.to_vec()))
            .unwrap_or_else(|e| panic!("open: {e}"));
        let mut f = archive
            .by_name(name)
            .unwrap_or_else(|e| panic!("member {name}: {e}"));
        let method = f.compression();
        let mut out = Vec::new();
        f.read_to_end(&mut out)
            .unwrap_or_else(|e| panic!("read {name}: {e}"));
        (method, out)
    }

    #[test]
    fn opens_bzip2_member_and_reads_exact_bytes() {
        let payload = b"AeroFTP zip bzip2 read-path payload line.\n".repeat(96);
        let zip = write_member_zip(CompressionMethod::Bzip2, "payload.txt", &payload);
        let (method, got) = open_member(&zip, "payload.txt");
        assert_eq!(method, CompressionMethod::Bzip2, "member must report Bzip2");
        assert_eq!(got, payload, "bzip2 member bytes must round-trip exactly");
    }

    #[test]
    fn opens_zstd_member_and_reads_exact_bytes() {
        let payload = b"AeroFTP zip zstd read-path payload line.\n".repeat(96);
        let zip = write_member_zip(CompressionMethod::Zstd, "payload.txt", &payload);
        let (method, got) = open_member(&zip, "payload.txt");
        assert_eq!(method, CompressionMethod::Zstd, "member must report Zstd");
        assert_eq!(got, payload, "zstd member bytes must round-trip exactly");
    }

    #[test]
    fn opens_xz_member_and_reads_exact_bytes() {
        let payload = b"AeroFTP zip xz read-path payload line.\n".repeat(96);
        let zip = write_member_zip(CompressionMethod::Xz, "payload.txt", &payload);
        let (method, got) = open_member(&zip, "payload.txt");
        assert_eq!(method, CompressionMethod::Xz, "member must report Xz");
        assert_eq!(got, payload, "xz member bytes must round-trip exactly");
    }

    // LZMA cannot be emitted by the crate's writer, so this is a real .zip made
    // externally (Python `zipfile.ZIP_LZMA`, method 14). Opening it exercises the
    // interop direction native -> AeroFTP for the LZMA codec.
    #[test]
    fn opens_lzma_member_from_native_fixture() {
        let zip = include_bytes!("../tests/fixtures/zip-methods/lzma.zip");
        let (method, got) = open_member(zip, "payload.txt");
        assert_eq!(
            method,
            CompressionMethod::Lzma,
            "fixture member must report Lzma"
        );
        let expected = b"AeroFTP zip-method fixture: lzma read-path.\n".repeat(64);
        assert_eq!(got, expected, "lzma fixture bytes must decode exactly");
    }

    // Deflate64 also cannot be emitted by the crate's writer. This fixture is a
    // real .zip produced by 7-Zip 23.01 (`7z a -tzip -mm=Deflate64`), method 9:
    // a genuine native -> AeroFTP interop case for the Deflate64 codec.
    #[test]
    fn opens_deflate64_member_from_native_fixture() {
        let zip = include_bytes!("../tests/fixtures/zip-methods/deflate64.zip");
        let (method, got) = open_member(zip, "payload.txt");
        assert_eq!(
            method,
            CompressionMethod::Deflate64,
            "fixture member must report Deflate64"
        );
        let expected = b"AeroFTP zip-method fixture: deflate64 read-path.\n".repeat(64);
        assert_eq!(got, expected, "deflate64 fixture bytes must decode exactly");
    }
}

#[cfg(test)]
mod sevenz_advanced_tests {
    use super::{compress_7z_core, extract_7z_core, SevenZAdvanced};

    // Every advanced method (and the solid option) must both COMPRESS and
    // EXTRACT: we only ship a create path for methods our reader can reopen.
    // This packs two known files with each method, extracts them, and compares
    // the bytes. If a method were createable but not decodable (or the solid
    // pack were miswired) the round-trip fails loudly.
    #[tokio::test]
    async fn every_method_and_solid_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        // Two files, so the solid case actually packs a multi-file block.
        let a = "AeroFTP 7z advanced payload A.\n".repeat(2048).into_bytes();
        let b = "AeroFTP 7z advanced payload B.\n".repeat(2048).into_bytes();
        let src_a = dir.path().join("a.txt");
        let src_b = dir.path().join("b.txt");
        std::fs::write(&src_a, &a).unwrap();
        std::fs::write(&src_b, &b).unwrap();
        let inputs = vec![
            src_a.to_string_lossy().to_string(),
            src_b.to_string_lossy().to_string(),
        ];

        // (method, solid). LZMA2 is covered both non-solid and solid.
        let cases = [
            ("lzma2", false),
            ("lzma", false),
            ("ppmd", false),
            ("bzip2", false),
            ("lzma2", true),
        ];
        for (i, (method, solid)) in cases.iter().enumerate() {
            let out = dir
                .path()
                .join(format!("adv{i}.7z"))
                .to_string_lossy()
                .to_string();
            let adv = SevenZAdvanced {
                method: Some((*method).to_string()),
                solid: Some(*solid),
                ..Default::default()
            };
            compress_7z_core(inputs.clone(), out.clone(), None, Some(5), None, Some(adv))
                .await
                .unwrap_or_else(|e| panic!("compress {method} solid={solid}: {e}"));

            let outdir = dir.path().join(format!("x{i}"));
            extract_7z_core(out, outdir.to_string_lossy().to_string(), None, false)
                .await
                .unwrap_or_else(|e| panic!("extract {method} solid={solid}: {e}"));

            let ra = std::fs::read(outdir.join("a.txt"))
                .unwrap_or_else(|_| panic!("{method} solid={solid}: a.txt missing"));
            let rb = std::fs::read(outdir.join("b.txt"))
                .unwrap_or_else(|_| panic!("{method} solid={solid}: b.txt missing"));
            assert_eq!(ra, a, "{method} solid={solid}: a.txt round-trip mismatch");
            assert_eq!(rb, b, "{method} solid={solid}: b.txt round-trip mismatch");
        }
    }

    // An unknown method must be rejected, not silently downgraded to the default
    // codec (which would hide a caller bug and mislabel the archive).
    #[tokio::test]
    async fn unknown_method_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("f.txt");
        std::fs::write(&src, b"hello").unwrap();
        let out = dir.path().join("bad.7z").to_string_lossy().to_string();
        let adv = SevenZAdvanced {
            method: Some("brotli".to_string()),
            ..Default::default()
        };
        let err = compress_7z_core(
            vec![src.to_string_lossy().to_string()],
            out,
            None,
            Some(5),
            None,
            Some(adv),
        )
        .await
        .expect_err("unknown method must be rejected");
        assert!(
            err.contains("unknown 7z method"),
            "unexpected error message: {err}"
        );
    }
}

#[cfg(test)]
mod archive_meta_tests {
    use super::parse_zip_central_dir;

    /// Build one ZIP central-directory file header with the given flags, method,
    /// name and extra field. Fixed 46-byte layout followed by name then extra.
    fn cdfh(flags: u16, method: u16, name: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]); // central-file-header signature
        v.extend_from_slice(&[0, 0]); // version made by
        v.extend_from_slice(&[0, 0]); // version needed
        v.extend_from_slice(&flags.to_le_bytes());
        v.extend_from_slice(&method.to_le_bytes());
        v.extend_from_slice(&[0, 0]); // mod time
        v.extend_from_slice(&[0, 0]); // mod date
        v.extend_from_slice(&[0, 0, 0, 0]); // crc-32
        v.extend_from_slice(&[0, 0, 0, 0]); // compressed size
        v.extend_from_slice(&[0, 0, 0, 0]); // uncompressed size
        v.extend_from_slice(&(name.len() as u16).to_le_bytes());
        v.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        v.extend_from_slice(&[0, 0]); // comment length
        v.extend_from_slice(&[0, 0]); // disk number start
        v.extend_from_slice(&[0, 0]); // internal attrs
        v.extend_from_slice(&[0, 0, 0, 0]); // external attrs
        v.extend_from_slice(&[0, 0, 0, 0]); // local header offset
        v.extend_from_slice(name);
        v.extend_from_slice(extra);
        v
    }

    /// WinZip AES (0x9901) extra field with the given strength byte (1/2/3).
    fn aes_extra(strength: u8) -> Vec<u8> {
        vec![
            0x01, 0x99, // header id 0x9901
            0x07, 0x00, // data size 7
            0x01, 0x00, // vendor version
            b'A', b'E',     // vendor id "AE"
            strength, // 1=128, 2=192, 3=256
            0x63, 0x00, // actual compression method (99 = store here)
        ]
    }

    #[test]
    fn winzip_aes256_is_detected() {
        let cd = cdfh(0x0001, 99, b"a.txt", &aes_extra(3));
        let meta = parse_zip_central_dir(&cd, 1);
        assert!(meta.encrypted);
        assert_eq!(meta.cipher.as_deref(), Some("AES-256"));
    }

    #[test]
    fn winzip_aes128_is_detected() {
        let cd = cdfh(0x0001, 99, b"a.txt", &aes_extra(1));
        let meta = parse_zip_central_dir(&cd, 1);
        assert_eq!(meta.cipher.as_deref(), Some("AES-128"));
    }

    #[test]
    fn legacy_zipcrypto_is_detected() {
        // Encrypted flag set, ordinary deflate method, no AES extra field.
        let cd = cdfh(0x0001, 8, b"a.txt", &[]);
        let meta = parse_zip_central_dir(&cd, 1);
        assert!(meta.encrypted);
        assert_eq!(meta.cipher.as_deref(), Some("ZipCrypto"));
    }

    #[test]
    fn plaintext_zip_is_not_encrypted() {
        let cd = cdfh(0x0000, 8, b"a.txt", &[]);
        let meta = parse_zip_central_dir(&cd, 1);
        assert!(!meta.encrypted);
        assert!(meta.cipher.is_none());
    }

    #[test]
    fn encrypted_entry_found_after_a_plaintext_one() {
        let mut cd = cdfh(0x0000, 8, b"plain.txt", &[]);
        cd.extend_from_slice(&cdfh(0x0001, 99, b"secret.txt", &aes_extra(2)));
        let meta = parse_zip_central_dir(&cd, 2);
        assert!(meta.encrypted);
        assert_eq!(meta.cipher.as_deref(), Some("AES-192"));
    }

    /// WinZip AES (0x9901) extra field with the given strength byte and inner
    /// (real) compression method, so the Compression column can be exercised for
    /// AES entries whose visible method is the 99 wrapper.
    fn aes_extra_method(strength: u8, inner_method: u16) -> Vec<u8> {
        let m = inner_method.to_le_bytes();
        vec![
            0x01, 0x99, // header id 0x9901
            0x07, 0x00, // data size 7
            0x01, 0x00, // vendor version
            b'A', b'E',     // vendor id "AE"
            strength, // 1=128, 2=192, 3=256
            m[0], m[1], // actual compression method
        ]
    }

    #[test]
    fn compression_deflate_is_reported() {
        let cd = cdfh(0x0000, 8, b"a.txt", &[]);
        let meta = super::parse_zip_central_dir(&cd, 1);
        assert_eq!(meta.compression.as_deref(), Some("Deflate"));
    }

    #[test]
    fn compression_store_is_reported() {
        let cd = cdfh(0x0000, 0, b"a.txt", &[]);
        let meta = super::parse_zip_central_dir(&cd, 1);
        assert_eq!(meta.compression.as_deref(), Some("Store"));
    }

    #[test]
    fn compression_reads_inner_method_of_aes_entry() {
        // A WinZip AES entry (method 99) with inner Deflate (8) must report
        // Deflate, not the 99 wrapper.
        let cd = cdfh(0x0001, 99, b"a.txt", &aes_extra_method(3, 8));
        let meta = super::parse_zip_central_dir(&cd, 1);
        assert_eq!(meta.cipher.as_deref(), Some("AES-256"));
        assert_eq!(meta.compression.as_deref(), Some("Deflate"));
    }

    #[test]
    fn compression_first_compressed_entry_wins_over_leading_store() {
        // A stored entry first, then an LZMA entry: the column shows LZMA.
        let mut cd = cdfh(0x0000, 0, b"stored.bin", &[]);
        cd.extend_from_slice(&cdfh(0x0000, 14, b"data.txt", &[]));
        let meta = super::parse_zip_central_dir(&cd, 2);
        assert_eq!(meta.compression.as_deref(), Some("LZMA"));
    }

    #[test]
    fn rar_method_label_store_vs_compressed() {
        // unrar Method: 0x30 = Store, 0x31..0x35 = the RAR compression levels.
        assert_eq!(super::rar_method_label(0x30), "Store");
        assert_eq!(super::rar_method_label(0), "Store");
        assert_eq!(super::rar_method_label(0x31), "RAR");
        assert_eq!(super::rar_method_label(0x35), "RAR");
    }
}

#[cfg(test)]
mod arch_install_format_tests {
    use super::{
        asset_matches_install_format, is_pacman_system, select_release_asset,
        update_download_supported, GitHubAsset, GitHubRelease,
    };

    /// Arch itself, and the derivatives that ship `/etc/arch-release`.
    #[test]
    fn arch_release_marks_a_pacman_system() {
        assert!(is_pacman_system(|path| path == "/etc/arch-release"));
    }

    /// Artix and Parabola ship `pacman.conf` without `/etc/arch-release`.
    #[test]
    fn pacman_conf_alone_marks_a_pacman_system() {
        assert!(is_pacman_system(|path| path == "/etc/pacman.conf"));
    }

    /// The regression this guards: a Debian or Ubuntu box has neither marker and
    /// must keep falling through to the `deb` default.
    #[test]
    fn debian_is_not_a_pacman_system() {
        assert!(!is_pacman_system(|path| path == "/etc/debian_version"));
        assert!(!is_pacman_system(|_| false));
    }

    fn release_with_signed_deb() -> GitHubRelease {
        GitHubRelease {
            tag_name: "4.1.3".to_string(),
            assets: vec![
                GitHubAsset {
                    name: "AeroFTP_4.1.3_amd64.deb".to_string(),
                    browser_download_url: "https://example.invalid/AeroFTP_4.1.3_amd64.deb"
                        .to_string(),
                },
                GitHubAsset {
                    name: "AeroFTP_4.1.3_amd64.deb.sigstore.json".to_string(),
                    browser_download_url:
                        "https://example.invalid/AeroFTP_4.1.3_amd64.deb.sigstore.json".to_string(),
                },
            ],
        }
    }

    /// The in-app updater installs a `.deb` through pkexec. On a pacman system
    /// dpkg does not exist, so `pacman` must never be download-supported.
    #[test]
    fn pacman_is_not_download_supported() {
        assert!(!update_download_supported("pacman"));
    }

    /// Discriminating guard: the very same fixture must still resolve for `deb`.
    /// Without this, `pacman_selects_no_release_asset` could pass because the
    /// fixture is malformed rather than because `pacman` is excluded.
    #[test]
    fn deb_still_resolves_a_signed_asset() {
        assert!(select_release_asset(&release_with_signed_deb(), "deb").is_some());
    }

    /// On Arch the updater must find nothing to install and fall through to the
    /// notify-only path.
    #[test]
    fn pacman_selects_no_release_asset() {
        assert!(select_release_asset(&release_with_signed_deb(), "pacman").is_none());
    }

    #[test]
    fn pacman_never_matches_a_deb_asset() {
        assert!(!asset_matches_install_format(
            "AeroFTP_4.1.3_amd64.deb",
            "pacman"
        ));
    }

    /// `snap` and `flatpak` already degrade to notify-only. `pacman` joins them,
    /// and must behave identically on both halves of the contract.
    #[test]
    fn pacman_honours_the_snap_and_flatpak_contract() {
        for format in ["snap", "flatpak", "pacman"] {
            assert!(
                !update_download_supported(format),
                "{format} must not be download-supported"
            );
            assert!(
                select_release_asset(&release_with_signed_deb(), format).is_none(),
                "{format} must select no release asset"
            );
        }
    }
}
