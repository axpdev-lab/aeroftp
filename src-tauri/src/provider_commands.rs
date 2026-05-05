//! Provider Commands - Tauri commands for multi-protocol cloud storage
//!
//! This module provides Tauri commands that route operations through
//! the StorageProvider abstraction, enabling support for FTP, WebDAV, S3, etc.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::provider_transfer_executor::{ProviderDownloadExecutor, ProviderUploadExecutor};
use crate::providers::{
    FileVersion, LockInfo, ProviderConfig, ProviderError, ProviderFactory, ProviderType,
    RemoteEntry, ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult, SharePermission,
    StorageInfo, StorageProvider,
};
use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
use crate::transfer_orchestrator::{execute_batch, ProgressObserver, TransferBatch};
use crate::transfer_settings::{
    resolve_provider_transfer_settings, ResolvedTransferSettings, TransferSettingsInput,
};
use crate::util::AbortOnDrop;

/// Global flag: when true, filesystem watcher should suppress sync triggers.
/// Set during folder download/upload to prevent AeroCloud interference.
pub static TRANSFER_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// RAII guard that clears `TRANSFER_IN_PROGRESS` on drop. Covers normal
/// returns AND panic-unwind: without this, a panic in the folder transfer
/// pipeline left the watcher suppressed forever until app restart.
struct TransferInProgressGuard(());

impl TransferInProgressGuard {
    fn acquire() -> Self {
        TRANSFER_IN_PROGRESS.store(true, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for TransferInProgressGuard {
    fn drop(&mut self) {
        TRANSFER_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// State for managing the active storage provider
pub struct ProviderState {
    /// Currently active provider (if connected)
    pub provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    /// Current provider configuration
    pub config: Arc<Mutex<Option<ProviderConfig>>>,
    /// Cancel flag for aborting folder transfers
    pub cancel_flag: Arc<AtomicBool>,
    /// Cancellation token cloned into async retry waits so user cancel wakes them immediately.
    cancel_token: Mutex<CancellationToken>,
    /// Held GitHub App installation token: never crosses IPC.
    /// Set by `github_app_token_from_pem`/`_from_vault`, consumed by `provider_connect`.
    pub held_github_app_token: Mutex<Option<String>>,
}

impl ProviderState {
    pub fn new() -> Self {
        Self {
            provider: Arc::new(Mutex::new(None)),
            config: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            cancel_token: Mutex::new(CancellationToken::new()),
            held_github_app_token: Mutex::new(None),
        }
    }

    pub async fn reset_cancel_state(&self) -> CancellationToken {
        self.cancel_flag.store(false, Ordering::Relaxed);
        let token = CancellationToken::new();
        *self.cancel_token.lock().await = token.clone();
        token
    }

    pub async fn current_cancel_token(&self) -> CancellationToken {
        self.cancel_token.lock().await.clone()
    }

    pub async fn request_cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.cancel_token.lock().await.cancel();
    }
}

impl Default for ProviderState {
    fn default() -> Self {
        Self::new()
    }
}

// ============ Auto-reconnect on idle disconnect (T-AUTO-RECONNECT-IDLE) ============

/// Lifecycle phases of a silent reconnect attempt. Mirrored to the
/// frontend via the `provider-session` Tauri event so the UI can
/// surface a transient toast (e.g. "Session expired, reconnecting...")
/// without forcing the user to disconnect and reconnect manually.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum SessionEventKind {
    /// The previous operation hit a dead session.
    Lost,
    /// Reconnect attempt is in flight.
    Reconnecting,
    /// Reconnect succeeded and the original op was replayed.
    Reconnected,
    /// Reconnect itself failed: the original error stands.
    ReconnectFailed,
}

#[derive(Debug, Clone, Serialize)]
struct SessionEvent {
    kind: SessionEventKind,
    /// Free-form detail (server reply, network error, ...). Empty for
    /// success transitions where there is nothing meaningful to show.
    detail: String,
}

fn emit_session_event(app: &AppHandle, kind: SessionEventKind, detail: impl Into<String>) {
    let _ = app.emit(
        "provider-session",
        SessionEvent {
            kind,
            detail: detail.into(),
        },
    );
}

/// Drive a single silent reconnect attempt against the live provider
/// instance, reusing the credentials it captured at original
/// connect-time. The provider's internal `current_dir` is saved and
/// best-effort restored after the new session is established, so a
/// subsequent retry of the user's operation sees the same path
/// context as before the disconnect.
///
/// Returns `Ok(restored_pwd)` on success. The caller is responsible
/// for replaying the failed operation against the freshly-reconnected
/// provider.
async fn try_silent_reconnect(
    app: &AppHandle,
    provider: &mut Box<dyn StorageProvider>,
) -> Result<String, ProviderError> {
    let prev_dir = provider.pwd().await.unwrap_or_else(|_| "/".to_string());
    emit_session_event(app, SessionEventKind::Reconnecting, "");
    tracing::warn!("Provider session lost; attempting silent reconnect");

    provider.connect().await.inspect_err(|e| {
        emit_session_event(app, SessionEventKind::ReconnectFailed, e.to_string());
    })?;

    // Best-effort cwd restore. Failure here is not fatal: the caller's
    // retry will hit the right error if the path is genuinely gone.
    if prev_dir != "/" {
        if let Err(e) = provider.cd(&prev_dir).await {
            tracing::warn!(
                "Reconnect succeeded but failed to restore previous dir {}: {}",
                prev_dir,
                e
            );
        }
    }
    Ok(prev_dir)
}

// ============ Request/Response Types ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConnectionParams {
    /// Protocol type: "ftp", "ftps", "sftp", "webdav", "s3", "mega", "opendrive"
    pub protocol: String,
    /// Host/URL (FTP server, WebDAV URL, or S3 endpoint)
    pub server: String,
    /// Port (optional, defaults based on protocol)
    pub port: Option<u16>,
    /// Username or Access Key ID
    pub username: String,
    /// Password or Secret Access Key
    pub password: String,
    /// Initial remote path to navigate to
    pub initial_path: Option<String>,
    /// S3 bucket name
    pub bucket: Option<String>,
    /// S3/cloud region
    pub region: Option<String>,
    /// Custom endpoint for S3-compatible services
    pub endpoint: Option<String>,
    /// Use path-style URLs for S3
    pub path_style: Option<bool>,
    /// Skip WebDAV Authorization headers for anonymous local bridges
    pub anonymous: Option<bool>,
    /// S3: Default storage class for uploads (STANDARD, STANDARD_IA, GLACIER, etc.)
    pub storage_class: Option<String>,
    /// S3: Server-side encryption mode (AES256 or aws:kms)
    pub sse_mode: Option<String>,
    /// S3: KMS key ID for SSE-KMS encryption
    pub sse_kms_key_id: Option<String>,
    /// Save session keys (MEGA)
    pub save_session: Option<bool>,
    /// Backend selection for MEGA: "native" or "megacmd"
    pub mega_mode: Option<String>,
    /// Session expiry timestamp (MEGA)
    pub session_expires_at: Option<i64>,
    /// MEGA: whether to logout/clear session on disconnect
    pub logout_on_disconnect: Option<bool>,
    /// SFTP: Path to private key file
    pub private_key_path: Option<String>,
    /// SFTP: Passphrase for encrypted private key
    pub key_passphrase: Option<String>,
    /// SFTP: Connection timeout in seconds
    pub timeout: Option<u64>,
    /// FTP/FTPS: TLS mode ("none", "explicit", "implicit", "explicit_if_available")
    pub tls_mode: Option<String>,
    /// FTP/FTPS: Accept invalid/self-signed certificates
    pub verify_cert: Option<bool>,
    /// Filen: Optional TOTP 2FA code
    pub two_factor_code: Option<String>,
    /// GitHub: auth mode used to obtain the token
    pub github_auth_mode: Option<String>,
    /// GitHub: App ID for installation-token mode
    pub github_app_id: Option<String>,
    /// GitHub: Installation ID for installation-token mode
    pub github_installation_id: Option<String>,
    /// GitHub: Local PEM path for installation-token refresh
    pub github_pem_path: Option<String>,
    /// GitHub: Installation token expiry (ISO 8601)
    pub github_token_expires_at: Option<String>,
    /// GitHub: optional branch override
    pub github_branch: Option<String>,
}

impl ProviderConnectionParams {
    /// Convert to provider configuration
    pub fn to_provider_config(&self) -> Result<ProviderConfig, String> {
        let provider_type = match self.protocol.to_lowercase().as_str() {
            "ftp" => ProviderType::Ftp,
            "ftps" => ProviderType::Ftps,
            "sftp" => ProviderType::Sftp,
            "webdav" => ProviderType::WebDav,
            "s3" => ProviderType::S3,
            "mega" => ProviderType::Mega,
            "box" => ProviderType::Box,
            "pcloud" => ProviderType::PCloud,
            "azure" => ProviderType::Azure,
            "filen" => ProviderType::Filen,
            "internxt" => ProviderType::Internxt,
            "kdrive" => ProviderType::KDrive,
            "jottacloud" => ProviderType::Jottacloud,
            "drime" => ProviderType::DrimeCloud,
            "filelu" => ProviderType::FileLu,
            "koofr" => ProviderType::Koofr,
            "opendrive" => ProviderType::OpenDrive,
            "yandexdisk" => ProviderType::YandexDisk,
            "github" => ProviderType::GitHub,
            "gitlab" => ProviderType::GitLab,
            "swift" => ProviderType::Swift,
            "googlephotos" | "google_photos" => ProviderType::GooglePhotos,
            "immich" => ProviderType::Immich,
            "b2" | "backblaze" | "backblazeb2" => ProviderType::Backblaze,
            other => return Err(format!("Unknown protocol: {}", other)),
        };

        let mut extra = std::collections::HashMap::new();

        // Add S3-specific options
        if provider_type == ProviderType::S3 {
            if let Some(ref bucket) = self.bucket {
                extra.insert("bucket".to_string(), bucket.clone());
            } else {
                return Err("S3 requires a bucket name".to_string());
            }
            if let Some(ref region) = self.region {
                extra.insert("region".to_string(), region.clone());
            } else {
                extra.insert("region".to_string(), "us-east-1".to_string());
            }
            if let Some(ref endpoint) = self.endpoint {
                extra.insert("endpoint".to_string(), endpoint.clone());
            }
            if let Some(path_style) = self.path_style {
                extra.insert("path_style".to_string(), path_style.to_string());
            }
            // S3 enterprise: storage class, SSE mode, KMS key
            if let Some(ref sc) = self.storage_class {
                if !sc.is_empty() {
                    extra.insert("storage_class".to_string(), sc.clone());
                }
            }
            if let Some(ref sse) = self.sse_mode {
                if !sse.is_empty() {
                    extra.insert("sse_mode".to_string(), sse.clone());
                }
            }
            if let Some(ref kms) = self.sse_kms_key_id {
                if !kms.is_empty() {
                    extra.insert("sse_kms_key_id".to_string(), kms.clone());
                }
            }
        }

        if provider_type == ProviderType::Backblaze {
            if let Some(ref bucket) = self.bucket {
                extra.insert("bucket".to_string(), bucket.clone());
            } else {
                return Err("Backblaze B2 requires a bucket name".to_string());
            }
        }

        if provider_type == ProviderType::WebDav && self.anonymous.unwrap_or(false) {
            extra.insert("anonymous".to_string(), "true".to_string());
        }

        // Add FTP/FTPS-specific options
        if provider_type == ProviderType::Ftp || provider_type == ProviderType::Ftps {
            if let Some(ref tls_mode) = self.tls_mode {
                extra.insert("tls_mode".to_string(), tls_mode.clone());
            }
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
            }
        }

        // WebDAV scheme override + self-signed cert opt-out. tls_mode accepts
        // "http", "https", or "auto" (default). Required for local WebDAV
        // bridges such as Filen Desktop (port 1900, HTTP) and any custom-
        // port HTTP server where the auto-detection would otherwise pick
        // HTTPS.
        if provider_type == ProviderType::WebDav {
            if let Some(ref tls_mode) = self.tls_mode {
                if !tls_mode.is_empty() {
                    extra.insert("tls_mode".to_string(), tls_mode.clone());
                }
            }
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
            }
        }

        // Add MEGA-specific options
        if provider_type == ProviderType::Mega {
            if self.save_session.unwrap_or(true) {
                extra.insert("save_session".to_string(), "true".to_string());
            }
            if let Some(ref mega_mode) = self.mega_mode {
                if !mega_mode.is_empty() {
                    extra.insert("mega_mode".to_string(), mega_mode.clone());
                }
            }
            if let Some(ts) = self.session_expires_at {
                extra.insert("session_expires_at".to_string(), ts.to_string());
            }
            if let Some(logout) = self.logout_on_disconnect {
                extra.insert("logout_on_disconnect".to_string(), logout.to_string());
            }
        }

        // Add Azure-specific options
        if provider_type == ProviderType::Azure {
            if let Some(ref bucket) = self.bucket {
                extra.insert("container".to_string(), bucket.clone());
            }
            if let Some(ref endpoint) = self.endpoint {
                extra.insert("endpoint".to_string(), endpoint.clone());
            }
            // account_name comes from username field
        }

        // 2FA TOTP forwarding for E2E providers + MEGA. The frontend ships
        // the 6-digit code on connectionParams.options.two_factor_code; we
        // only insert it into extra when actually present so that profiles
        // without 2FA enabled don't send empty fields to the API.
        if provider_type == ProviderType::Filen
            || provider_type == ProviderType::Internxt
            || provider_type == ProviderType::Mega
        {
            if let Some(ref code) = self.two_factor_code {
                if !code.is_empty() {
                    extra.insert("two_factor_code".to_string(), code.clone());
                }
            }
        }

        if provider_type == ProviderType::GitHub || provider_type == ProviderType::GitLab {
            // Branch override: shared by both GitHub and GitLab
            if let Some(ref branch) = self.github_branch {
                if !branch.is_empty() {
                    extra.insert("branch".to_string(), branch.clone());
                }
            }
        }

        // GitLab: accept_invalid_certs for self-hosted instances
        if provider_type == ProviderType::GitLab {
            if let Some(verify) = self.verify_cert {
                extra.insert("verify_cert".to_string(), verify.to_string());
            }
        }

        if provider_type == ProviderType::GitHub {
            if let Some(ref auth_mode) = self.github_auth_mode {
                if !auth_mode.is_empty() {
                    extra.insert("github_auth_mode".to_string(), auth_mode.clone());
                }
            }
            if let Some(ref app_id) = self.github_app_id {
                if !app_id.is_empty() {
                    extra.insert("github_app_id".to_string(), app_id.clone());
                }
            }
            if let Some(ref installation_id) = self.github_installation_id {
                if !installation_id.is_empty() {
                    extra.insert(
                        "github_installation_id".to_string(),
                        installation_id.clone(),
                    );
                }
            }
            if let Some(ref pem_path) = self.github_pem_path {
                if !pem_path.is_empty() {
                    extra.insert("github_pem_path".to_string(), pem_path.clone());
                }
            }
            if let Some(ref expires_at) = self.github_token_expires_at {
                if !expires_at.is_empty() {
                    extra.insert("github_token_expires_at".to_string(), expires_at.clone());
                }
            }
        }

        // Add pCloud-specific options
        if provider_type == ProviderType::PCloud {
            if let Some(ref region) = self.region {
                extra.insert("region".to_string(), region.clone());
            } else {
                extra.insert("region".to_string(), "us".to_string());
            }
        }

        // Add kDrive-specific options
        if provider_type == ProviderType::KDrive {
            if let Some(ref bucket) = self.bucket {
                // Reuse bucket field for drive_id
                extra.insert("drive_id".to_string(), bucket.clone());
            } else {
                return Err("kDrive requires a Drive ID".to_string());
            }
        }

        // Add SFTP-specific options
        if provider_type == ProviderType::Sftp {
            if let Some(ref key_path) = self.private_key_path {
                if !key_path.is_empty() {
                    extra.insert("private_key_path".to_string(), key_path.clone());
                }
            }
            if let Some(ref passphrase) = self.key_passphrase {
                if !passphrase.is_empty() {
                    extra.insert("key_passphrase".to_string(), passphrase.clone());
                }
            }
            if let Some(timeout) = self.timeout {
                extra.insert("timeout".to_string(), timeout.to_string());
            }
        }

        let host = if provider_type == ProviderType::Mega {
            "mega.nz".to_string()
        } else if provider_type == ProviderType::Internxt {
            "gateway.internxt.com".to_string()
        } else if provider_type == ProviderType::KDrive {
            "api.infomaniak.com".to_string()
        } else if provider_type == ProviderType::Jottacloud {
            "jfs.jottacloud.com".to_string()
        } else if provider_type == ProviderType::DrimeCloud {
            "app.drime.cloud".to_string()
        } else if provider_type == ProviderType::FileLu {
            "filelu.com".to_string()
        } else if provider_type == ProviderType::Koofr {
            "app.koofr.net".to_string()
        } else if provider_type == ProviderType::OpenDrive {
            "dev.opendrive.com".to_string()
        } else if provider_type == ProviderType::YandexDisk {
            "cloud-api.yandex.net".to_string()
        } else if provider_type == ProviderType::Azure {
            // Azure constructs endpoint from account_name if server is empty
            if self.server.is_empty() {
                String::new()
            } else {
                self.server.clone()
            }
        } else {
            self.server.clone()
        };

        // Strip port suffix from host if present (e.g. "127.0.0.1:2121" → "127.0.0.1")
        // Users sometimes enter host:port in the server field, but port is a separate param
        let host = if let Some(colon_idx) = host.rfind(':') {
            let after = &host[colon_idx + 1..];
            if after.parse::<u16>().is_ok() {
                host[..colon_idx].to_string()
            } else {
                host
            }
        } else {
            host
        };

        Ok(ProviderConfig {
            name: format!("{}@{}", self.username, host),
            provider_type,
            host,
            port: self.port,
            username: Some(self.username.clone()),
            password: Some(self.password.clone()),
            initial_path: self.initial_path.clone(),
            extra,
        })
    }
}

#[derive(Serialize)]
pub struct ProviderListResponse {
    pub files: Vec<RemoteEntry>,
    pub current_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDriveTrashActionItem {
    pub item_id: String,
    pub is_dir: bool,
}

#[derive(Serialize)]
pub struct ProviderConnectionInfo {
    pub connected: bool,
    pub protocol: Option<String>,
    pub display_name: Option<String>,
    pub server_info: Option<String>,
}

// ============ Tauri Commands ============

/// Connect to a storage provider using the specified protocol
#[tauri::command]
pub async fn provider_connect(
    state: State<'_, ProviderState>,
    params: ProviderConnectionParams,
) -> Result<String, String> {
    info!(
        "Connecting to {} provider: {}",
        params.protocol, params.server
    );

    let mut config = params.to_provider_config()?;

    // SEC-GH-001: For GitHub App mode, inject the held installation token
    // so the token never crosses the IPC boundary.
    // Only inject when password is empty/missing (App mode sends empty password).
    // PAT and Device Flow provide their own password: never overwrite.
    // Uses clone() instead of take() so the token survives connection retries.
    if config.provider_type == ProviderType::GitHub {
        let password_empty = config.password.as_ref().is_none_or(|p| p.is_empty());
        if password_empty {
            let held = state.held_github_app_token.lock().await;
            if let Some(ref token) = *held {
                config.password = Some(token.clone());
            }
        }
    }

    // Create provider using factory
    let mut provider = ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    // A3-05: Zeroize password after it has been consumed by the provider
    config.zeroize_password();

    // Connect
    provider
        .connect()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    let display_name = provider.display_name();
    let protocol = format!("{:?}", provider.provider_type());

    // Store provider and config. If a previous provider is still held here
    // (reconnect-without-disconnect, user swapping servers from the UI, etc.),
    // gracefully disconnect it first; synchronously dropping a connected
    // `Box<dyn StorageProvider>` does not run async disconnect, which leaks
    // server-side sessions, socket handles, and OAuth refresh tokens.
    {
        let mut prov_lock = state.provider.lock().await;
        if let Some(mut previous) = prov_lock.take() {
            if let Err(err) = previous.disconnect().await {
                warn!(
                    "provider_connect: previous provider disconnect failed: {}",
                    err
                );
            }
        }
        *prov_lock = Some(provider);
    }
    {
        let mut config_lock = state.config.lock().await;
        *config_lock = Some(config);
    }

    info!("Connected successfully: {}", display_name);
    Ok(format!("Connected to {} via {}", display_name, protocol))
}

/// Disconnect from the current provider
#[tauri::command]
pub async fn provider_disconnect(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    if let Some(ref mut provider) = *provider_lock {
        info!("Disconnecting from provider: {}", provider.display_name());
        provider
            .disconnect()
            .await
            .map_err(|e| format!("Disconnect failed: {}", e))?;
    }

    *provider_lock = None;

    let mut config_lock = state.config.lock().await;
    *config_lock = None;

    Ok(())
}

/// Check if connected to a provider
#[tauri::command]
pub async fn provider_check_connection(
    state: State<'_, ProviderState>,
) -> Result<ProviderConnectionInfo, String> {
    let provider_lock = state.provider.lock().await;

    match &*provider_lock {
        Some(provider) => Ok(ProviderConnectionInfo {
            connected: provider.is_connected(),
            protocol: Some(format!("{:?}", provider.provider_type())),
            display_name: Some(provider.display_name()),
            server_info: None,
        }),
        None => Ok(ProviderConnectionInfo {
            connected: false,
            protocol: None,
            display_name: None,
            server_info: None,
        }),
    }
}

/// List files in the specified path
#[tauri::command]
pub async fn provider_list_files(
    app: AppHandle,
    state: State<'_, ProviderState>,
    path: Option<String>,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    let list_path = path.as_deref().unwrap_or(".");

    // Retry once on a transport-level disconnect (server idle reaper,
    // NAT eviction). Pre-existing successful sessions can become dead
    // between user actions; surfacing the raw `session closed` as a
    // hard error forces a manual reconnect, which Tom (issue #161)
    // flagged as the workflow killer.
    let files = match provider.list(list_path).await {
        Ok(files) => files,
        Err(e) if e.is_connection_lost() => {
            emit_session_event(&app, SessionEventKind::Lost, e.to_string());
            try_silent_reconnect(&app, provider)
                .await
                .map_err(|err| format!("Failed to reconnect: {}", err))?;
            let files = provider
                .list(list_path)
                .await
                .map_err(|err| format!("Failed to list files after reconnect: {}", err))?;
            emit_session_event(&app, SessionEventKind::Reconnected, "");
            files
        }
        Err(e) => return Err(format!("Failed to list files: {}", e)),
    };

    let current_path = provider.pwd().await.unwrap_or_else(|_| "/".to_string());

    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Change to the specified directory
#[tauri::command]
pub async fn provider_change_dir(
    app: AppHandle,
    state: State<'_, ProviderState>,
    path: String,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    // Run the cd op, and on a transport-level disconnect retry it
    // exactly once after a silent reconnect. `try_silent_reconnect`
    // restores the pre-disconnect cwd, so a `cd("..")` request is
    // resolved relative to where the user actually was (not the
    // post-reconnect home dir).
    let nav_result = if path == ".." {
        provider.cd_up().await
    } else {
        provider.cd(&path).await
    };

    if let Err(e) = nav_result {
        if !e.is_connection_lost() {
            return Err(if path == ".." {
                format!("Failed to go up: {}", e)
            } else {
                format!("Failed to change directory: {}", e)
            });
        }
        emit_session_event(&app, SessionEventKind::Lost, e.to_string());
        try_silent_reconnect(&app, provider)
            .await
            .map_err(|err| format!("Failed to reconnect: {}", err))?;
        let retry = if path == ".." {
            provider.cd_up().await
        } else {
            provider.cd(&path).await
        };
        retry.map_err(|err| {
            if path == ".." {
                format!("Failed to go up after reconnect: {}", err)
            } else {
                format!("Failed to change directory after reconnect: {}", err)
            }
        })?;
        emit_session_event(&app, SessionEventKind::Reconnected, "");
    }

    let files = provider
        .list(".")
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;

    let current_path = provider.pwd().await.unwrap_or_else(|_| path.clone());

    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Navigate to parent directory
#[tauri::command]
pub async fn provider_go_up(
    app: AppHandle,
    state: State<'_, ProviderState>,
) -> Result<ProviderListResponse, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    if let Err(e) = provider.cd_up().await {
        if !e.is_connection_lost() {
            return Err(format!("Failed to go up: {}", e));
        }
        emit_session_event(&app, SessionEventKind::Lost, e.to_string());
        try_silent_reconnect(&app, provider)
            .await
            .map_err(|err| format!("Failed to reconnect: {}", err))?;
        provider
            .cd_up()
            .await
            .map_err(|err| format!("Failed to go up after reconnect: {}", err))?;
        emit_session_event(&app, SessionEventKind::Reconnected, "");
    }

    let files = provider
        .list(".")
        .await
        .map_err(|e| format!("Failed to list files: {}", e))?;

    let current_path = provider.pwd().await.unwrap_or_else(|_| "/".to_string());

    Ok(ProviderListResponse {
        files,
        current_path,
    })
}

/// Get current working directory
#[tauri::command]
pub async fn provider_pwd(state: State<'_, ProviderState>) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .pwd()
        .await
        .map_err(|e| format!("Failed to get working directory: {}", e))
}

/// Download a file from the remote server
#[tauri::command]
pub async fn provider_download_file(
    app: AppHandle,
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
    modified: Option<String>,
    use_delta: Option<bool>,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    let filename = std::path::Path::new(&remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let transfer_id = format!("pdl-{}", chrono::Utc::now().timestamp_millis());

    info!(
        "Downloading via provider: {} -> {}",
        remote_path, local_path
    );

    // Emit start event
    let _ = app.emit(
        "transfer_event",
        crate::TransferEvent {
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

    // Create parent directory if needed
    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let file_size = provider.size(&remote_path).await.unwrap_or(0);
    let app_progress = app.clone();
    let tid_progress = transfer_id.clone();
    let fname_progress = filename.clone();

    let dl_start_time = std::time::Instant::now();
    let progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> = if file_size > 0 {
        Some(Box::new(move |transferred: u64, total: u64| {
            let pct = if total > 0 {
                ((transferred as f64 / total as f64) * 100.0) as u8
            } else {
                0
            };
            let elapsed = dl_start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.1 {
                (transferred as f64 / elapsed) as u64
            } else {
                0
            };
            let eta = if speed > 0 && transferred < total {
                ((total - transferred) as f64 / speed as f64) as u64
            } else {
                0
            };
            let _ = app_progress.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: tid_progress.clone(),
                    filename: fname_progress.clone(),
                    direction: "download".to_string(),
                    message: None,
                    progress: Some(crate::TransferProgress {
                        transfer_id: tid_progress.clone(),
                        filename: fname_progress.clone(),
                        direction: "download".to_string(),
                        percentage: pct,
                        transferred,
                        total,
                        speed_bps: speed,
                        eta_seconds: eta as u32,
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
        }))
    } else {
        None
    };

    // Delta path (SFTP + key-auth + rsync on remote): tried before the
    // classic download. Self-gated inside `try_delta_transfer`: returns
    // `None` for non-SFTP providers, password-only auth, or when the SSH
    // handle is not available. A `hard_error` (host-key mismatch, permission
    // denied) surfaces as a transfer error without the silent classic
    // fallback: security failures must not be masked. Same contract as
    // `sync::perform_download` in the sync_tree path.
    let mut delta_fallback_reason: Option<String> = None;
    #[cfg(unix)]
    {
        if use_delta.unwrap_or(true) {
            let local_path_buf = std::path::PathBuf::from(&local_path);
            if let Some(result) = crate::delta_sync_rsync::try_delta_transfer(
                provider.as_mut(),
                crate::delta_sync_rsync::SyncDirection::Download,
                &local_path_buf,
                &remote_path,
            )
            .await
            {
                if result.used_delta {
                    let delta_stats = result
                        .stats
                        .as_ref()
                        .map(crate::sync::DeltaTransferStats::from_rsync);
                    crate::preserve_remote_mtime(&local_path, modified.as_deref());
                    let actual_size = tokio::fs::metadata(&local_path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(file_size);
                    let _ = app.emit(
                        "transfer_event",
                        crate::TransferEvent {
                            event_type: "complete".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "download".to_string(),
                            message: Some(format!(
                                "({} via delta)",
                                if actual_size > 1_048_576 {
                                    format!("{:.1} MB", actual_size as f64 / 1_048_576.0)
                                } else {
                                    format!("{:.1} KB", actual_size as f64 / 1024.0)
                                }
                            )),
                            progress: None,
                            path: None,
                            delta_stats,
                            fallback_reason: None,
                        },
                    );
                    info!("Download completed via delta path: {}", filename);
                    return Ok(format!("Downloaded: {}", filename));
                }
                if let Some(hard_err) = result.hard_error {
                    let err_msg = format!("delta hard rejection: {}", hard_err);
                    let _ = app.emit(
                        "transfer_event",
                        crate::TransferEvent {
                            event_type: "error".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "download".to_string(),
                            message: Some(err_msg.clone()),
                            progress: None,
                            path: None,
                            delta_stats: None,
                            fallback_reason: None,
                        },
                    );
                    return Err(err_msg);
                }
                // Silent fallback: result.fallback_reason populated but we continue
                // with the classic provider path below.
                delta_fallback_reason = result.fallback_reason;
            }
        }
    }

    // Resume-aware download: if provider supports resume and a partial .aerotmp exists,
    // use resume_download to continue from where we left off. This avoids re-downloading
    // data on S3/Azure (pay-per-GB) and all other HTTP-based providers.
    let result = if provider.supports_resume() {
        let tmp_path = format!("{}.aerotmp", local_path);
        let offset = tokio::fs::metadata(&tmp_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if offset > 0 {
            info!(
                "Resuming download from offset {} bytes: {}",
                offset, filename
            );
            provider
                .resume_download(&remote_path, &local_path, offset, progress_cb)
                .await
        } else {
            provider
                .download(&remote_path, &local_path, progress_cb)
                .await
        }
    } else {
        provider
            .download(&remote_path, &local_path, progress_cb)
            .await
    };

    match &result {
        Ok(()) => {
            // Preserve remote mtime on the local file
            crate::preserve_remote_mtime(&local_path, modified.as_deref());
            let actual_size = tokio::fs::metadata(&local_path)
                .await
                .map(|m| m.len())
                .unwrap_or(file_size);
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: filename.clone(),
                    direction: "download".to_string(),
                    message: Some(format!(
                        "({} in 0s)",
                        if actual_size > 1_048_576 {
                            format!("{:.1} MB", actual_size as f64 / 1_048_576.0)
                        } else {
                            format!("{:.1} KB", actual_size as f64 / 1024.0)
                        }
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: delta_fallback_reason,
                },
            );
            info!("Download completed: {}", filename);
            Ok(format!("Downloaded: {}", filename))
        }
        Err(e) => {
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id,
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

/// Download a folder recursively from the remote server (OAuth providers)
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn provider_download_folder(
    app: AppHandle,
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
    #[allow(unused_variables)] file_exists_action: Option<String>,
    max_concurrent: Option<u32>,
    retry_count: Option<u32>,
    timeout_seconds: Option<u64>,
) -> Result<String, String> {
    let runtime_settings = resolve_provider_transfer_settings(TransferSettingsInput {
        max_concurrent,
        retry_count,
        timeout_seconds,
    });

    // Capture current pwd so we can restore it after folder scan changes it
    let original_pwd = {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            p.pwd().await.unwrap_or_default()
        } else {
            String::new()
        }
    };

    // RAII guard: clears TRANSFER_IN_PROGRESS on every exit path including panic.
    let _transfer_guard = TransferInProgressGuard::acquire();
    let result = provider_download_folder_inner(
        &app,
        &state,
        &remote_path,
        &local_path,
        file_exists_action,
        runtime_settings,
    )
    .await;

    // Restore provider pwd (folder scan traverses subdirectories via cd)
    if !original_pwd.is_empty() {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            let _ = p.cd(&original_pwd).await;
        }
    }

    result
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn provider_upload_folder(
    app: AppHandle,
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    #[allow(unused_variables)] file_exists_action: Option<String>,
    max_concurrent: Option<u32>,
    retry_count: Option<u32>,
    timeout_seconds: Option<u64>,
    commit_message: Option<String>,
) -> Result<String, String> {
    let runtime_settings = resolve_provider_transfer_settings(TransferSettingsInput {
        max_concurrent,
        retry_count,
        timeout_seconds,
    });

    // Capture current pwd so we can restore it after upload
    let original_pwd = {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            p.pwd().await.unwrap_or_default()
        } else {
            String::new()
        }
    };

    let _transfer_guard = TransferInProgressGuard::acquire();
    let result = provider_upload_folder_inner(
        &app,
        &state,
        &local_path,
        &remote_path,
        runtime_settings,
        commit_message,
    )
    .await;

    // Restore provider pwd (upload may change it via mkdir/cd)
    if !original_pwd.is_empty() {
        let mut lock = state.provider.lock().await;
        if let Some(p) = lock.as_mut() {
            let _ = p.cd(&original_pwd).await;
        }
    }

    result
}

/// Collected file entry for 2-phase download
struct DownloadEntry {
    remote_path: String,
    local_path: String,
    name: String,
    size: u64,
    modified: Option<String>,
}

fn provider_transfer_cancelled(state: &State<'_, ProviderState>) -> bool {
    state.cancel_flag.load(Ordering::Relaxed)
}

/// Sanitize a remote filename to prevent path traversal attacks.
/// Strips path separators, `..` components, null bytes, and drive letters.
/// Returns the sanitized filename, or an error if the name is empty or entirely unsafe.
pub(crate) fn sanitize_remote_filename(name: &str) -> Result<String, String> {
    // Split on both Unix and Windows path separators, filter out dangerous components
    let sanitized: Vec<&str> = name
        .split(&['/', '\\'][..])
        .filter(|component| {
            !component.is_empty()
                && *component != "."
                && *component != ".."
                && !component.contains('\0')
        })
        .collect();

    if sanitized.is_empty() {
        return Err(format!("Unsafe remote filename rejected: {:?}", name));
    }

    // Take only the last component (the actual filename)
    let filename = sanitized
        .last()
        .ok_or_else(|| "Internal error: sanitized filename unexpectedly empty".to_string())?
        .to_string();

    // Reject Windows drive letters (e.g. "C:")
    if filename.len() >= 2
        && filename.as_bytes()[1] == b':'
        && filename.as_bytes()[0].is_ascii_alphabetic()
    {
        return Err(format!(
            "Unsafe remote filename with drive letter rejected: {:?}",
            name
        ));
    }

    Ok(filename)
}

/// Verify that a resolved path is safely contained within the expected base directory.
pub(crate) fn verify_path_containment(
    base: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), String> {
    // Use canonicalize on the base (which must already exist)
    let canonical_base = base
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize base path: {}", e))?;

    // For target, canonicalize the parent (which should exist after create_dir_all)
    // and then append the filename
    let canonical_target = if target.exists() {
        target
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize target path: {}", e))?
    } else if let Some(parent) = target.parent() {
        if parent.exists() {
            let canonical_parent = parent
                .canonicalize()
                .map_err(|e| format!("Failed to canonicalize parent path: {}", e))?;
            canonical_parent.join(target.file_name().unwrap_or_default())
        } else {
            target.to_path_buf()
        }
    } else {
        target.to_path_buf()
    };

    if !canonical_target.starts_with(&canonical_base) {
        return Err(format!(
            "Path traversal detected: {:?} escapes base directory {:?}",
            canonical_target, canonical_base
        ));
    }
    Ok(())
}

/// Inner implementation: 2-phase approach with per-file lock release and retry
async fn provider_download_folder_inner(
    app: &AppHandle,
    state: &State<'_, ProviderState>,
    remote_path: &str,
    local_path: &str,
    file_exists_action: Option<String>,
    runtime_settings: ResolvedTransferSettings,
) -> Result<String, String> {
    let file_exists_action = file_exists_action.unwrap_or_default();

    let cancel_token = state.reset_cancel_state().await;

    let folder_name = std::path::Path::new(remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());

    let transfer_id = format!("dl-folder-{}", chrono::Utc::now().timestamp_millis());

    info!(
        "Downloading folder via provider: {} -> {} (concurrency={}, retries={}, timeout={}s)",
        remote_path,
        local_path,
        runtime_settings.max_concurrent,
        runtime_settings.retry_count,
        runtime_settings.timeout_seconds
    );

    // Emit start event
    let _ = app.emit(
        "transfer_event",
        crate::TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "download".to_string(),
            message: Some(format!("Starting folder download: {}", folder_name)),
            progress: None,
            path: Some(remote_path.to_string()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    // Create local folder
    tokio::fs::create_dir_all(local_path)
        .await
        .map_err(|e| format!("Failed to create local folder: {}", e))?;

    // ── Streaming scan + transfer: directory-by-directory interleaving ──
    //
    // Instead of scanning ALL files first, then downloading ALL files,
    // we scan one directory at a time and download its files immediately.
    // This means the first file starts downloading after scanning just the
    // root directory, not after the entire recursive scan completes.
    //
    // Pattern (like an audio player buffer):
    //   scan dir A → transfer files from A
    //   scan dir B → transfer files from B
    //   ...until all directories are exhausted.

    let mut folders_to_scan: Vec<(String, String)> =
        vec![(remote_path.to_string(), local_path.to_string())];
    let mut files_downloaded = 0u32;
    let mut files_skipped = 0u32;
    let mut total_files_discovered = 0u32;
    let mut dirs_scanned = 0u32;
    let mut file_global_index = 0u32;
    let mut last_scan_emit = std::time::Instant::now();
    let base_local = std::path::Path::new(local_path);
    let mut transfer_entries: Vec<TransferEntry> = Vec::new();

    while let Some((remote_folder, local_folder)) = folders_to_scan.pop() {
        // ── Check cancel before scanning next directory ──
        if provider_transfer_cancelled(state) {
            info!(
                "Provider folder download cancelled by user after {} files",
                files_downloaded
            );
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "cancelled".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!(
                        "Download cancelled after {} files",
                        files_downloaded
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return Ok(format!(
                "Download cancelled after {} files",
                files_downloaded
            ));
        }

        // ── Scan this directory (acquire lock, list, release) ──
        let mut dir_files: Vec<DownloadEntry> = Vec::new();
        {
            let mut provider_lock = state.provider.lock().await;
            let provider = provider_lock
                .as_mut()
                .ok_or("Not connected to any provider")?;

            provider
                .cd(&remote_folder)
                .await
                .map_err(|e| format!("Failed to change to folder {}: {}", remote_folder, e))?;

            let files = provider
                .list(".")
                .await
                .map_err(|e| format!("Failed to list files in {}: {}", remote_folder, e))?;

            for file in files {
                let safe_name = match sanitize_remote_filename(&file.name) {
                    Ok(n) => n,
                    Err(e) => {
                        warn!("Skipping unsafe remote entry: {}", e);
                        continue;
                    }
                };

                let remote_file_path = if remote_folder.ends_with('/') {
                    format!("{}{}", remote_folder, file.name)
                } else {
                    format!("{}/{}", remote_folder, file.name)
                };
                let local_file_path_buf = std::path::Path::new(&local_folder).join(&safe_name);
                let local_file_path = local_file_path_buf.to_string_lossy().to_string();

                if file.is_dir {
                    tokio::fs::create_dir_all(&local_file_path)
                        .await
                        .map_err(|e| {
                            format!("Failed to create folder {}: {}", local_file_path, e)
                        })?;
                    verify_path_containment(base_local, &local_file_path_buf)?;
                    folders_to_scan.push((remote_file_path, local_file_path));
                } else {
                    if let Some(parent) = local_file_path_buf.parent() {
                        if parent.exists() {
                            verify_path_containment(base_local, &local_file_path_buf)?;
                        }
                    }
                    dir_files.push(DownloadEntry {
                        remote_path: remote_file_path,
                        local_path: local_file_path,
                        name: safe_name,
                        size: file.size,
                        modified: file.modified.clone(),
                    });
                }
            }
        } // ← provider lock released: ready to transfer this batch

        dirs_scanned += 1;
        total_files_discovered += dir_files.len() as u32;

        // Emit scanning progress
        if last_scan_emit.elapsed().as_millis() > 500 || dirs_scanned <= 1 {
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "scanning".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "download".to_string(),
                    message: Some(format!(
                        "Scanning... {} files found, {} downloaded ({} dirs queued)",
                        total_files_discovered,
                        files_downloaded,
                        folders_to_scan.len()
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            last_scan_emit = std::time::Instant::now();
        }

        // ── Transfer files from this directory immediately ──
        for entry in &dir_files {
            // Check cancel before each file
            if provider_transfer_cancelled(state) {
                info!(
                    "Provider folder download cancelled by user after {} files",
                    files_downloaded
                );
                let _ = app.emit(
                    "transfer_event",
                    crate::TransferEvent {
                        event_type: "cancelled".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: folder_name.clone(),
                        direction: "download".to_string(),
                        message: Some(format!(
                            "Download cancelled after {} files",
                            files_downloaded
                        )),
                        progress: None,
                        path: None,
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                return Ok(format!(
                    "Download cancelled after {} files",
                    files_downloaded
                ));
            }

            file_global_index += 1;

            // Check if local file exists and should be skipped
            if !file_exists_action.is_empty() && file_exists_action != "overwrite" {
                let local_p = std::path::Path::new(&entry.local_path);
                if let Ok(local_meta) = std::fs::metadata(local_p) {
                    if local_meta.is_file() {
                        let remote_modified = entry.modified.as_ref().and_then(|s| {
                            let clean = s.strip_suffix('Z').unwrap_or(s);
                            chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%d %H:%M:%S")
                                .or_else(|_| {
                                    chrono::NaiveDateTime::parse_from_str(
                                        clean,
                                        "%Y-%m-%dT%H:%M:%S",
                                    )
                                })
                                .ok()
                                .map(|ndt| ndt.and_utc())
                        });
                        if crate::should_skip_file_download(
                            &file_exists_action,
                            remote_modified,
                            entry.size,
                            &local_meta,
                        ) {
                            files_skipped += 1;
                            let _ = app.emit(
                                "transfer_event",
                                crate::TransferEvent {
                                    event_type: "file_skip".to_string(),
                                    transfer_id: format!("{}-{}", transfer_id, file_global_index),
                                    filename: entry.name.clone(),
                                    direction: "download".to_string(),
                                    message: Some(format!("Skipped (identical): {}", entry.name)),
                                    progress: None,
                                    path: Some(entry.remote_path.clone()),
                                    delta_stats: None,
                                    fallback_reason: None,
                                },
                            );
                            continue;
                        }
                    }
                }
            }

            let file_transfer_id = format!("{}-{}", transfer_id, file_global_index);

            transfer_entries.push(TransferEntry {
                id: file_transfer_id,
                display_name: entry.name.clone(),
                remote_path: entry.remote_path.clone(),
                local_path: entry.local_path.clone(),
                size: entry.size,
                modified: entry.modified.clone(),
            });
        }
    }

    let batch = TransferBatch {
        id: transfer_id.clone(),
        display_name: folder_name.clone(),
        direction: TransferDirection::Download,
        config: TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
        },
        entries: transfer_entries,
    };

    let progress_app = app.clone();
    let progress_transfer_id = transfer_id.clone();
    let progress_folder_name = folder_name.clone();
    let progress_remote_path = remote_path.to_string();
    let total_files_for_progress = total_files_discovered;
    let initial_skipped = files_skipped;
    let progress_observer: ProgressObserver = Arc::new(move |snapshot| {
        let processed = initial_skipped + snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        let _ = progress_app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_folder_name.clone(),
                direction: "download".to_string(),
                message: Some(format!(
                    "Downloaded {} / {} files ({} skipped, {} errors)",
                    snapshot.completed, total_files_for_progress, initial_skipped, snapshot.failed
                )),
                progress: Some(crate::TransferProgress {
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

    let executor = Arc::new(ProviderDownloadExecutor::new(
        app.clone(),
        state.provider.clone(),
        runtime_settings,
        cancel_token,
    ));

    let batch_result = execute_batch(
        app,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    files_downloaded = batch_result.completed;
    let files_errored = batch_result.failed;

    info!(
        "Provider folder download completed via orchestrator: {} ({} downloaded, {} skipped, {} errors)",
        folder_name, files_downloaded, files_skipped, files_errored
    );

    let event_type = if batch_result.cancelled {
        "cancelled".to_string()
    } else {
        "complete".to_string()
    };
    let result_message = if batch_result.cancelled {
        format!(
            "Download cancelled after {} files",
            files_downloaded + files_skipped + files_errored
        )
    } else {
        format!(
            "Downloaded {} files, {} skipped, {} errors",
            files_downloaded, files_skipped, files_errored
        )
    };

    let _ = app.emit(
        "transfer_event",
        crate::TransferEvent {
            event_type,
            transfer_id,
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

async fn provider_upload_folder_inner(
    app: &AppHandle,
    state: &State<'_, ProviderState>,
    local_path: &str,
    remote_path: &str,
    runtime_settings: ResolvedTransferSettings,
    commit_message: Option<String>,
) -> Result<String, String> {
    let cancel_token = state.reset_cancel_state().await;

    let folder_name = std::path::Path::new(local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".to_string());

    let transfer_id = format!("ul-folder-{}", chrono::Utc::now().timestamp_millis());

    info!(
        "Uploading folder via provider: {} -> {} (concurrency={}, retries={}, timeout={}s)",
        local_path,
        remote_path,
        runtime_settings.max_concurrent,
        runtime_settings.retry_count,
        runtime_settings.timeout_seconds
    );

    let _ = app.emit(
        "transfer_event",
        crate::TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.clone(),
            filename: folder_name.clone(),
            direction: "upload".to_string(),
            message: Some(format!("Starting folder upload: {}", folder_name)),
            progress: None,
            path: Some(remote_path.to_string()),
            delta_stats: None,
            fallback_reason: None,
        },
    );

    let local_base = std::path::Path::new(local_path);
    if !local_base.is_dir() {
        return Err("Source is not a directory".to_string());
    }

    {
        let mut provider_lock = state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or("Not connected to any provider")?;

        if provider.provider_type() == ProviderType::GitHub {
            let github = provider
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            if let Err(e) = github
                .create_directory(remote_path, commit_message.as_deref())
                .await
            {
                let err_str = e.to_string().to_lowercase();
                if !err_str.contains("exist")
                    && !err_str.contains("409")
                    && !err_str.contains("already")
                {
                    return Err(format!("Failed to create directory: {}", e));
                }
            }
        } else if let Err(e) = provider.mkdir(remote_path).await {
            let err_str = e.to_string().to_lowercase();
            if !err_str.contains("exist") && !err_str.contains("409") && !err_str.contains("550") {
                return Err(format!("Failed to create directory: {}", e));
            }
        }
    }

    let mut dirs_to_scan: Vec<(std::path::PathBuf, String)> =
        vec![(local_base.to_path_buf(), remote_path.to_string())];
    let mut dirs_to_create: Vec<String> = Vec::new();
    let mut transfer_entries: Vec<TransferEntry> = Vec::new();
    let mut total_files_discovered = 0u32;
    let mut dirs_scanned = 0u32;
    let mut file_global_index = 0u32;
    let mut last_scan_emit = std::time::Instant::now();

    while let Some((current_local_dir, current_remote_dir)) = dirs_to_scan.pop() {
        if provider_transfer_cancelled(state) {
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "cancelled".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!(
                        "Upload cancelled after {} files",
                        transfer_entries.len()
                    )),
                    progress: None,
                    path: Some(remote_path.to_string()),
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            return Ok(format!(
                "Upload cancelled after {} files",
                transfer_entries.len()
            ));
        }

        let mut read_dir = tokio::fs::read_dir(&current_local_dir)
            .await
            .map_err(|e| format!("Failed to read directory {:?}: {}", current_local_dir, e))?;

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let local_entry_path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let remote_entry_path =
                format!("{}/{}", current_remote_dir.trim_end_matches('/'), name);
            let file_type = match entry.file_type().await {
                Ok(file_type) => file_type,
                Err(error) => {
                    warn!(
                        "Failed to read provider upload entry type {:?}: {}",
                        local_entry_path, error
                    );
                    continue;
                }
            };

            if file_type.is_symlink() {
                let _ = app.emit(
                    "transfer_event",
                    crate::TransferEvent {
                        event_type: "file_skip".to_string(),
                        transfer_id: transfer_id.clone(),
                        filename: name.clone(),
                        direction: "upload".to_string(),
                        message: Some(format!("Skipped symlink: {}", name)),
                        progress: None,
                        path: Some(remote_entry_path.clone()),
                        delta_stats: None,
                        fallback_reason: None,
                    },
                );
                continue;
            }

            if file_type.is_dir() {
                dirs_to_scan.push((local_entry_path.clone(), remote_entry_path.clone()));
                dirs_to_create.push(remote_entry_path);
            } else if file_type.is_file() {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                file_global_index += 1;
                total_files_discovered += 1;
                transfer_entries.push(TransferEntry {
                    id: format!("{}-{}", transfer_id, file_global_index),
                    display_name: name.clone(),
                    remote_path: remote_entry_path,
                    local_path: local_entry_path.to_string_lossy().to_string(),
                    size,
                    modified: None,
                });
            }
        }

        dirs_scanned += 1;
        if last_scan_emit.elapsed().as_millis() > 500 || dirs_scanned <= 1 {
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "scanning".to_string(),
                    transfer_id: transfer_id.clone(),
                    filename: folder_name.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!(
                        "Scanning... {} files found ({} dirs queued)",
                        total_files_discovered,
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

    dirs_to_create.sort_by_key(|a| a.matches('/').count());
    for remote_dir in &dirs_to_create {
        let mut provider_lock = state.provider.lock().await;
        let provider = provider_lock
            .as_mut()
            .ok_or("Not connected to any provider")?;

        let mkdir_result = if provider.provider_type() == ProviderType::GitHub {
            let github = provider
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            github
                .create_directory(remote_dir, commit_message.as_deref())
                .await
                .map_err(|e| e.to_string())
        } else {
            provider.mkdir(remote_dir).await.map_err(|e| e.to_string())
        };

        if let Err(error) = mkdir_result {
            let lowered = error.to_lowercase();
            if !lowered.contains("exist") && !lowered.contains("409") {
                warn!(
                    "Failed to create provider directory {}: {}",
                    remote_dir, error
                );
            }
        }
    }

    let batch = TransferBatch {
        id: transfer_id.clone(),
        display_name: folder_name.clone(),
        direction: TransferDirection::Upload,
        config: TransferBatchConfig {
            max_concurrent: runtime_settings.max_concurrent,
            max_retries: runtime_settings.retry_count,
            timeout_ms: runtime_settings.timeout_seconds * 1000,
        },
        entries: transfer_entries,
    };

    let progress_app = app.clone();
    let progress_transfer_id = transfer_id.clone();
    let progress_folder_name = folder_name.clone();
    let progress_remote_path = remote_path.to_string();
    let total_files_for_progress = total_files_discovered;
    let progress_observer: ProgressObserver = Arc::new(move |snapshot| {
        let processed = snapshot.completed + snapshot.failed + snapshot.skipped;
        let percentage = if total_files_for_progress > 0 {
            ((processed as f64 / total_files_for_progress as f64) * 100.0) as u8
        } else {
            100
        };

        let _ = progress_app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "progress".to_string(),
                transfer_id: progress_transfer_id.clone(),
                filename: progress_folder_name.clone(),
                direction: "upload".to_string(),
                message: Some(format!(
                    "Uploaded {} / {} files ({} errors)",
                    snapshot.completed, total_files_for_progress, snapshot.failed
                )),
                progress: Some(crate::TransferProgress {
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

    let executor = Arc::new(ProviderUploadExecutor::new(
        app.clone(),
        state.provider.clone(),
        runtime_settings,
        commit_message,
        cancel_token,
    ));

    let batch_result = execute_batch(
        app,
        batch,
        executor,
        state.cancel_flag.clone(),
        Some(progress_observer),
    )
    .await;

    let files_uploaded = batch_result.completed;
    let files_errored = batch_result.failed;
    let event_type = if batch_result.cancelled {
        "cancelled".to_string()
    } else {
        "complete".to_string()
    };
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
        crate::TransferEvent {
            event_type,
            transfer_id,
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

/// Upload a file to the remote server
#[tauri::command]
pub async fn provider_upload_file(
    app: AppHandle,
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    commit_message: Option<String>,
    use_delta: Option<bool>,
) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    let filename = std::path::Path::new(&local_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let transfer_id = format!("pul-{}", chrono::Utc::now().timestamp_millis());
    let file_size = tokio::fs::metadata(&local_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    info!("Uploading via provider: {} -> {}", local_path, remote_path);

    // Emit start event
    let _ = app.emit(
        "transfer_event",
        crate::TransferEvent {
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

    let app_progress = app.clone();
    let tid_progress = transfer_id.clone();
    let fname_progress = filename.clone();

    let ul_start_time = std::time::Instant::now();
    let progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> = if file_size > 0 {
        Some(Box::new(move |transferred: u64, total: u64| {
            let pct = if total > 0 {
                ((transferred as f64 / total as f64) * 100.0) as u8
            } else {
                0
            };
            let elapsed = ul_start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.1 {
                (transferred as f64 / elapsed) as u64
            } else {
                0
            };
            let eta = if speed > 0 && transferred < total {
                ((total - transferred) as f64 / speed as f64) as u64
            } else {
                0
            };
            let _ = app_progress.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "progress".to_string(),
                    transfer_id: tid_progress.clone(),
                    filename: fname_progress.clone(),
                    direction: "upload".to_string(),
                    message: None,
                    progress: Some(crate::TransferProgress {
                        transfer_id: tid_progress.clone(),
                        filename: fname_progress.clone(),
                        direction: "upload".to_string(),
                        percentage: pct,
                        transferred,
                        total,
                        speed_bps: speed,
                        eta_seconds: eta as u32,
                        total_files: None,
                        path: None,
                    }),
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
        }))
    } else {
        None
    };

    // Delta path (SFTP + key-auth + rsync on remote): same contract as
    // `sync::perform_upload`. Skipped automatically for GitHub / non-SFTP
    // / password-only auth (self-gated inside `try_delta_transfer`).
    // `hard_error` must not silently fall back to the classic path.
    let mut delta_fallback_reason: Option<String> = None;
    #[cfg(unix)]
    {
        if use_delta.unwrap_or(true) {
            let local_path_buf = std::path::PathBuf::from(&local_path);
            if let Some(delta_result) = crate::delta_sync_rsync::try_delta_transfer(
                provider.as_mut(),
                crate::delta_sync_rsync::SyncDirection::Upload,
                &local_path_buf,
                &remote_path,
            )
            .await
            {
                if delta_result.used_delta {
                    let delta_stats = delta_result
                        .stats
                        .as_ref()
                        .map(crate::sync::DeltaTransferStats::from_rsync);
                    let _ = app.emit(
                        "transfer_event",
                        crate::TransferEvent {
                            event_type: "complete".to_string(),
                            transfer_id: transfer_id.clone(),
                            filename: filename.clone(),
                            direction: "upload".to_string(),
                            message: Some(format!(
                                "({} via delta)",
                                if file_size > 1_048_576 {
                                    format!("{:.1} MB", file_size as f64 / 1_048_576.0)
                                } else {
                                    format!("{:.1} KB", file_size as f64 / 1024.0)
                                }
                            )),
                            progress: None,
                            path: None,
                            delta_stats,
                            fallback_reason: None,
                        },
                    );
                    info!("Upload completed via delta path: {}", filename);
                    return Ok(format!("Uploaded: {}", filename));
                }
                if let Some(hard_err) = delta_result.hard_error {
                    let err_msg = format!("delta hard rejection: {}", hard_err);
                    let _ = app.emit(
                        "transfer_event",
                        crate::TransferEvent {
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
                // Silent fallback to classic provider upload below.
                delta_fallback_reason = delta_result.fallback_reason;
            }
        }
    }

    let result = if provider.provider_type() == ProviderType::GitHub {
        let github = provider
            .as_any_mut()
            .downcast_mut::<crate::providers::github::GitHubProvider>()
            .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
        github
            .upload_file(&local_path, &remote_path, commit_message.as_deref())
            .await
            .map_err(|e| format!("Upload failed: {}", e))
    } else {
        provider
            .upload(&local_path, &remote_path, progress_cb)
            .await
            .map_err(|e| format!("Upload failed: {}", e))
    };

    match &result {
        Ok(()) => {
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id,
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: Some(format!(
                        "({} in 0s)",
                        if file_size > 1_048_576 {
                            format!("{:.1} MB", file_size as f64 / 1_048_576.0)
                        } else {
                            format!("{:.1} KB", file_size as f64 / 1024.0)
                        }
                    )),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: delta_fallback_reason,
                },
            );
            info!("Upload completed: {}", filename);
            Ok(format!("Uploaded: {}", filename))
        }
        Err(e) => {
            let _ = app.emit(
                "transfer_event",
                crate::TransferEvent {
                    event_type: "error".to_string(),
                    transfer_id,
                    filename: filename.clone(),
                    direction: "upload".to_string(),
                    message: Some(e.clone()),
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
            Err(e.clone())
        }
    }
}

/// Create a directory
#[tauri::command]
pub async fn provider_mkdir(
    state: State<'_, ProviderState>,
    path: String,
    commit_message: Option<String>,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Creating directory: {}", path);

    if provider.provider_type() == ProviderType::GitHub {
        let github = provider
            .as_any_mut()
            .downcast_mut::<crate::providers::github::GitHubProvider>()
            .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
        github
            .create_directory(&path, commit_message.as_deref())
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    } else {
        provider
            .mkdir(&path)
            .await
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    Ok(())
}

/// Delete a file
#[tauri::command]
pub async fn provider_delete_file(
    state: State<'_, ProviderState>,
    path: String,
    commit_message: Option<String>,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Deleting file: {}", path);

    if provider.provider_type() == ProviderType::GitHub {
        let github = provider
            .as_any_mut()
            .downcast_mut::<crate::providers::github::GitHubProvider>()
            .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
        github
            .delete_file(&path, commit_message.as_deref())
            .await
            .map_err(|e| format!("Failed to delete file: {}", e))?;
    } else {
        provider
            .delete(&path)
            .await
            .map_err(|e| format!("Failed to delete file: {}", e))?;
    }

    Ok(())
}

/// Delete a directory
#[tauri::command]
pub async fn provider_delete_dir(
    app: AppHandle,
    state: State<'_, ProviderState>,
    path: String,
    recursive: bool,
    commit_message: Option<String>,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Deleting directory: {} (recursive: {})", path, recursive);

    // Emit scanning event for folder deletes so the ScanningToast appears
    if recursive {
        let folder_name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        let _ = app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "scanning".to_string(),
                transfer_id: format!("del-dir-{}", chrono::Utc::now().timestamp_millis()),
                filename: folder_name,
                direction: "delete".to_string(),
                message: Some("Scanning folder for deletion...".to_string()),
                progress: None,
                path: Some(path.clone()),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    if provider.provider_type() == ProviderType::GitHub {
        // QA-GH-006: GitHub always needs recursive delete (no empty dirs in git)
        let github = provider
            .as_any_mut()
            .downcast_mut::<crate::providers::github::GitHubProvider>()
            .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
        github
            .delete_directory_recursive(&path, commit_message.as_deref())
            .await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    } else if recursive {
        provider
            .rmdir_recursive(&path)
            .await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    } else {
        provider
            .rmdir(&path)
            .await
            .map_err(|e| format!("Failed to delete directory: {}", e))?;
    }

    // Emit delete_complete so ScanningToast dismisses
    if recursive {
        let folder_name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        let _ = app.emit(
            "transfer_event",
            crate::TransferEvent {
                event_type: "delete_complete".to_string(),
                transfer_id: format!("del-dir-done-{}", chrono::Utc::now().timestamp_millis()),
                filename: folder_name,
                direction: "delete".to_string(),
                message: Some("Directory deleted".to_string()),
                progress: None,
                path: Some(path),
                delta_stats: None,
                fallback_reason: None,
            },
        );
    }

    Ok(())
}

/// Rename a file or directory
#[tauri::command]
pub async fn provider_rename(
    state: State<'_, ProviderState>,
    from: String,
    to: String,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    info!("Renaming: {} -> {}", from, to);

    provider
        .rename(&from, &to)
        .await
        .map_err(|e| format!("Failed to rename: {}", e))?;

    Ok(())
}

/// Server-side copy (if supported by provider)
#[tauri::command]
pub async fn provider_server_copy(
    state: State<'_, ProviderState>,
    from: String,
    to: String,
) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    if !provider.supports_server_copy() {
        return Err("Server-side copy not supported by this provider".to_string());
    }

    info!("Server copy: {} -> {}", from, to);

    provider
        .server_copy(&from, &to)
        .await
        .map_err(|e| format!("Failed to copy: {}", e))?;

    Ok(())
}

/// Check if provider supports server-side copy
#[tauri::command]
pub async fn provider_supports_server_copy(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_lock = state.provider.lock().await;
    let provider = provider_lock
        .as_ref()
        .ok_or("Not connected to any provider")?;
    Ok(provider.supports_server_copy())
}

/// Get file/directory information
#[tauri::command]
pub async fn provider_stat(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<RemoteEntry, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .stat(&path)
        .await
        .map_err(|e| format!("Failed to get file info: {}", e))
}

/// Keep connection alive (NOOP equivalent)
#[tauri::command]
pub async fn provider_keep_alive(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut provider_lock = state.provider.lock().await;

    if let Some(ref mut provider) = *provider_lock {
        provider
            .keep_alive()
            .await
            .map_err(|e| format!("Keep alive failed: {}", e))?;
    }

    Ok(())
}

/// Get server information
#[tauri::command]
pub async fn provider_server_info(state: State<'_, ProviderState>) -> Result<String, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .server_info()
        .await
        .map_err(|e| format!("Failed to get server info: {}", e))
}

/// Get file size
#[tauri::command]
pub async fn provider_file_size(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<u64, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .size(&path)
        .await
        .map_err(|e| format!("Failed to get file size: {}", e))
}

/// Check if a file/directory exists
#[tauri::command]
pub async fn provider_exists(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<bool, String> {
    let mut provider_lock = state.provider.lock().await;

    let provider = provider_lock
        .as_mut()
        .ok_or("Not connected to any provider")?;

    provider
        .exists(&path)
        .await
        .map_err(|e| format!("Failed to check existence: {}", e))
}

// ============ OAuth2 Commands ============

/// OAuth2 connection parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConnectionParams {
    /// Provider: "google_drive", "dropbox", "onedrive", "zoho_workdrive", etc.
    pub provider: String,
    /// OAuth2 client ID (from app registration)
    pub client_id: String,
    /// OAuth2 client secret (from app registration)
    pub client_secret: String,
    /// Region for multi-region providers (Zoho: "us", "eu", "in", "au", "jp", "ca", "sa")
    #[serde(default = "default_region")]
    pub region: String,
}

fn default_region() -> String {
    "us".to_string()
}

/// OAuth2 flow state
#[derive(Debug, Clone, Serialize)]
pub struct OAuthFlowStarted {
    /// URL to open in browser
    pub auth_url: String,
    /// State parameter for verification
    pub state: String,
}

/// Start OAuth2 authentication flow
/// Returns the authorization URL to open in browser
#[tauri::command]
pub async fn oauth2_start_auth(params: OAuthConnectionParams) -> Result<OAuthFlowStarted, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig};

    info!("Starting OAuth2 flow for {}", params.provider);

    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google(&params.client_id, &params.client_secret)
        }
        "googlephotos" | "google_photos" => {
            OAuthConfig::google_photos(&params.client_id, &params.client_secret)
        }
        "dropbox" => OAuthConfig::dropbox(&params.client_id, &params.client_secret),
        "onedrive" | "microsoft" => OAuthConfig::onedrive(&params.client_id, &params.client_secret),
        "box" => OAuthConfig::box_cloud(&params.client_id, &params.client_secret),
        "pcloud" => OAuthConfig::pcloud(&params.client_id, &params.client_secret, &params.region),
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => {
            OAuthConfig::zoho(&params.client_id, &params.client_secret, &params.region)
        }
        "yandexdisk" | "yandex_disk" | "yandex" => {
            OAuthConfig::yandex_disk(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let manager = OAuth2Manager::new();
    let (auth_url, state) = manager
        .start_auth_flow(&config)
        .await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;

    // Open URL in default browser
    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser automatically: {}", e);
    }

    Ok(OAuthFlowStarted { auth_url, state })
}

/// Complete OAuth2 authentication with the authorization code
#[tauri::command]
pub async fn oauth2_complete_auth(
    params: OAuthConnectionParams,
    code: String,
    state: String,
) -> Result<String, String> {
    use crate::providers::{OAuth2Manager, OAuthConfig};

    info!("Completing OAuth2 flow for {}", params.provider);

    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google(&params.client_id, &params.client_secret)
        }
        "googlephotos" | "google_photos" => {
            OAuthConfig::google_photos(&params.client_id, &params.client_secret)
        }
        "dropbox" => OAuthConfig::dropbox(&params.client_id, &params.client_secret),
        "onedrive" | "microsoft" => OAuthConfig::onedrive(&params.client_id, &params.client_secret),
        "box" => OAuthConfig::box_cloud(&params.client_id, &params.client_secret),
        "pcloud" => OAuthConfig::pcloud(&params.client_id, &params.client_secret, &params.region),
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => {
            OAuthConfig::zoho(&params.client_id, &params.client_secret, &params.region)
        }
        "yandexdisk" | "yandex_disk" | "yandex" => {
            OAuthConfig::yandex_disk(&params.client_id, &params.client_secret)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let manager = OAuth2Manager::new();
    manager
        .complete_auth_flow(&config, &code, &state)
        .await
        .map_err(|e| format!("Failed to complete OAuth flow: {}", e))?;

    Ok("Authentication successful".to_string())
}

/// OAuth2 connection result with display name and account email
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2ConnectResult {
    pub display_name: String,
    pub account_email: Option<String>,
}

/// Connect to an OAuth2-based cloud provider (after authentication)
#[tauri::command]
pub async fn oauth2_connect(
    state: State<'_, ProviderState>,
    params: OAuthConnectionParams,
) -> Result<OAuth2ConnectResult, String> {
    use crate::providers::{
        dropbox::DropboxConfig, google_drive::GoogleDriveConfig, google_photos::GooglePhotosConfig,
        onedrive::OneDriveConfig, types::BoxConfig, types::PCloudConfig,
        zoho_workdrive::ZohoWorkdriveConfig, BoxProvider, DropboxProvider, GoogleDriveProvider,
        GooglePhotosProvider, OneDriveProvider, PCloudProvider, ZohoWorkdriveProvider,
    };

    info!("Connecting to OAuth2 provider: {}", params.provider);

    let provider: Box<dyn StorageProvider> = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            let config = GoogleDriveConfig::new(&params.client_id, &params.client_secret);
            let mut p = GoogleDriveProvider::new(config);
            p.connect()
                .await
                .map_err(|e| format!("Google Drive connection failed: {}", e))?;
            Box::new(p)
        }
        "googlephotos" | "google_photos" => {
            let config = GooglePhotosConfig::new(&params.client_id, &params.client_secret);
            let mut p = GooglePhotosProvider::new(config);
            p.connect()
                .await
                .map_err(|e| format!("Google Photos connection failed: {}", e))?;
            Box::new(p)
        }
        "dropbox" => {
            let config = DropboxConfig::new(&params.client_id, &params.client_secret);
            let mut p = DropboxProvider::new(config);
            p.connect()
                .await
                .map_err(|e| format!("Dropbox connection failed: {}", e))?;
            Box::new(p)
        }
        "onedrive" | "microsoft" => {
            let config = OneDriveConfig::new(&params.client_id, &params.client_secret);
            let mut p = OneDriveProvider::new(config);
            p.connect()
                .await
                .map_err(|e| format!("OneDrive connection failed: {}", e))?;
            Box::new(p)
        }
        "box" => {
            let config = BoxConfig {
                client_id: params.client_id.clone(),
                client_secret: params.client_secret.clone(),
            };
            let mut p = BoxProvider::new(config);
            p.connect()
                .await
                .map_err(|e| format!("Box connection failed: {}", e))?;
            Box::new(p)
        }
        "pcloud" => {
            // pCloud tokens are region-locked: always prefer vault-stored region
            // (detected during token exchange) over serde default "us"
            let region = crate::credential_store::CredentialStore::from_cache()
                .and_then(|store| store.get("oauth_pcloud_region").ok())
                .unwrap_or(params.region.clone());
            let config = PCloudConfig {
                client_id: params.client_id.clone(),
                client_secret: params.client_secret.clone(),
                region,
            };
            let mut p = PCloudProvider::new(config);
            p.connect()
                .await
                .map_err(|e| format!("pCloud connection failed: {}", e))?;
            Box::new(p)
        }
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => {
            let config =
                ZohoWorkdriveConfig::new(&params.client_id, &params.client_secret, &params.region);
            let mut p = ZohoWorkdriveProvider::new(config);
            p.connect()
                .await
                .map_err(|e| format!("Zoho WorkDrive connection failed: {}", e))?;
            Box::new(p)
        }
        "yandexdisk" | "yandex_disk" | "yandex" => {
            // Yandex Disk OAuth: retrieve token from stored OAuth tokens
            use crate::providers::{OAuth2Manager, OAuthProvider};
            let manager = OAuth2Manager::new();
            let tokens = manager
                .load_tokens(OAuthProvider::YandexDisk)
                .map_err(|e| format!("No Yandex Disk tokens found: {}", e))?;
            let mut p =
                crate::providers::YandexDiskProvider::new(tokens.access_token.clone(), None);
            p.connect()
                .await
                .map_err(|e| format!("Yandex Disk connection failed: {}", e))?;
            Box::new(p)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let display_name = provider.display_name();
    let account_email = provider.account_email();

    // Store provider
    let mut provider_lock = state.provider.lock().await;
    *provider_lock = Some(provider);

    info!(
        "Connected to {} ({})",
        display_name,
        account_email.as_deref().unwrap_or("no email")
    );
    Ok(OAuth2ConnectResult {
        display_name,
        account_email,
    })
}

/// Full OAuth2 authentication flow - starts server, opens browser, waits for callback, completes auth
#[tauri::command]
pub async fn oauth2_full_auth(params: OAuthConnectionParams) -> Result<String, String> {
    use crate::providers::{
        oauth2::{bind_callback_listener, bind_callback_listener_on_port, wait_for_callback},
        OAuth2Manager, OAuthConfig,
    };

    info!("Starting full OAuth2 flow for {}", params.provider);

    // Some providers require exact redirect_uri matching, so use a fixed port
    let fixed_port: u16 = match params.provider.to_lowercase().as_str() {
        "box" => 9484,
        "dropbox" => 17548,
        "onedrive" | "microsoft" => 27154,
        "pcloud" => 17384,
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => 18765,
        "yandexdisk" | "yandex_disk" | "yandex" => 19847,
        _ => 0,
    };

    // Bind callback listener (fixed port for Box, ephemeral for others)
    let (listener, port) = if fixed_port > 0 {
        bind_callback_listener_on_port(fixed_port).await
    } else {
        bind_callback_listener().await
    }
    .map_err(|e| format!("Failed to bind callback listener: {}", e))?;

    let config = match params.provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => {
            OAuthConfig::google_with_port(&params.client_id, &params.client_secret, port)
        }
        "googlephotos" | "google_photos" => {
            OAuthConfig::google_photos_with_port(&params.client_id, &params.client_secret, port)
        }
        "dropbox" => OAuthConfig::dropbox_with_port(&params.client_id, &params.client_secret, port),
        "onedrive" | "microsoft" => {
            OAuthConfig::onedrive_with_port(&params.client_id, &params.client_secret, port)
        }
        "box" => OAuthConfig::box_cloud_with_port(&params.client_id, &params.client_secret, port),
        "pcloud" => OAuthConfig::pcloud_with_port(
            &params.client_id,
            &params.client_secret,
            port,
            &params.region,
        ),
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => OAuthConfig::zoho_with_port(
            &params.client_id,
            &params.client_secret,
            port,
            &params.region,
        ),
        "yandexdisk" | "yandex_disk" | "yandex" => {
            OAuthConfig::yandex_disk_with_port(&params.client_id, &params.client_secret, port)
        }
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    // Create manager ONCE and keep it for the entire flow
    let manager = OAuth2Manager::new();

    // Generate auth URL with the dynamic port in redirect_uri
    let (auth_url, expected_state) = manager
        .start_auth_flow(&config)
        .await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;

    // Start waiting for callback in background. AbortOnDrop ensures the task
    // (and the bound TCP listener) is aborted on ANY early-return path below -
    // raw tokio::spawn would detach the handle and leak the port until process
    // restart if `open::that` fails or the 5-minute timeout fires.
    let mut callback_task = AbortOnDrop::spawn(async move { wait_for_callback(listener).await });

    // Open URL in default browser
    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser automatically: {}", e);
        return Err(format!(
            "Could not open browser: {}. Please open this URL manually: {}",
            e, auth_url
        ));
    }

    info!("Browser opened, waiting for callback...");

    // Wait for callback (with timeout). tokio::select! keeps ownership of the
    // guard inside the macro; on timeout the guard drops at function exit and
    // aborts the task, releasing the port.
    let callback_result = tokio::select! {
        res = callback_task.wait() => res
            .map_err(|e| format!("Callback server error: {}", e))?
            .map_err(|e| format!("Callback error: {}", e))?,
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(300)) => {
            return Err("OAuth timeout: no response within 5 minutes".to_string());
        }
    };

    let (code, state) = callback_result;

    // Verify state matches
    if state != expected_state {
        return Err("OAuth state mismatch - possible CSRF attack".to_string());
    }

    info!("Callback received, completing authentication...");

    // pCloud uses non-standard token exchange (GET, no PKCE, no expiry)
    if params.provider.to_lowercase() == "pcloud" {
        pcloud_exchange_code(&config, &code)
            .await
            .map_err(|e| format!("Failed to exchange code for tokens: {}", e))?;
    } else {
        // Standard OAuth2 flow using the SAME manager instance (which has the PKCE verifier stored)
        manager
            .complete_auth_flow(&config, &code, &expected_state)
            .await
            .map_err(|e| format!("Failed to exchange code for tokens: {}", e))?;
    }

    info!(
        "OAuth2 authentication completed successfully for {}",
        params.provider
    );
    Ok("Authentication successful! You can now connect.".to_string())
}

/// pCloud uses a non-standard OAuth2 token exchange:
/// - GET request (not POST)
/// - No PKCE support
/// - Tokens never expire (no refresh_token or expires_in)
/// - Response: {"access_token": "...", "userid": ..., "token_type": "bearer", "result": 0}
/// - Region-aware: tries configured endpoint first, then fallback (US↔EU)
async fn pcloud_exchange_code(
    config: &crate::providers::OAuthConfig,
    code: &str,
) -> Result<(), crate::providers::ProviderError> {
    use crate::providers::{oauth2::StoredTokens, OAuth2Manager, ProviderError};

    let client_secret = config.client_secret.as_deref().ok_or_else(|| {
        ProviderError::InvalidConfig("Missing client_secret for pCloud".to_string())
    })?;

    // pCloud accounts are region-locked (US=api.pcloud.com, EU=eapi.pcloud.com).
    // The auth code is only valid on the account's region endpoint.
    // Try configured endpoint first, fallback to the other region.
    let endpoints = if config.token_url.contains("eapi.pcloud.com") {
        vec![
            "https://eapi.pcloud.com/oauth2_token",
            "https://api.pcloud.com/oauth2_token",
        ]
    } else {
        vec![
            "https://api.pcloud.com/oauth2_token",
            "https://eapi.pcloud.com/oauth2_token",
        ]
    };

    let http = reqwest::Client::new();
    let mut last_error = String::new();

    for endpoint in &endpoints {
        let url = format!(
            "{}?client_id={}&client_secret={}&code={}",
            endpoint,
            urlencoding::encode(&config.client_id),
            urlencoding::encode(client_secret),
            urlencoding::encode(code),
        );

        let resp = match http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                last_error = format!("HTTP error on {}: {}", endpoint, e);
                continue;
            }
        };

        if !resp.status().is_success() {
            last_error = format!("HTTP {} from {}", resp.status(), endpoint);
            continue;
        }

        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                last_error = format!("Read error from {}: {}", endpoint, e);
                continue;
            }
        };

        let body: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                last_error = format!("Parse error from {}: {}", endpoint, e);
                continue;
            }
        };

        // Check for pCloud error response: error 2012 means wrong region, try next
        if let Some(result) = body["result"].as_i64() {
            if result != 0 {
                let error_msg = body["error"].as_str().unwrap_or("Unknown error");
                last_error = format!("pCloud error {} ({}): {}", result, endpoint, error_msg);
                continue;
            }
        }

        let access_token = body["access_token"].as_str().ok_or_else(|| {
            ProviderError::AuthenticationFailed("pCloud: missing access_token".to_string())
        })?;

        let tokens = StoredTokens {
            access_token: access_token.to_string(),
            refresh_token: None, // pCloud tokens don't expire
            expires_at: None,
            token_type: "Bearer".to_string(),
            scopes: vec![],
        };

        let manager = OAuth2Manager::new();
        manager.store_tokens(config.provider, &tokens)?;

        // Persist detected region so oauth2_connect uses the correct API endpoint
        let region = if endpoint.contains("eapi") {
            "eu"
        } else {
            "us"
        };
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            let _ = store.store("oauth_pcloud_region", region);
        }

        info!(
            "pCloud OAuth tokens obtained via {} ({}, permanent, no expiry)",
            endpoint,
            region.to_uppercase()
        );
        return Ok(());
    }

    Err(ProviderError::AuthenticationFailed(format!(
        "pCloud token exchange failed on all endpoints: {}",
        last_error
    )))
}

/// Check if OAuth2 tokens exist for a provider
#[tauri::command]
pub async fn oauth2_has_tokens(provider: String) -> Result<bool, String> {
    use crate::providers::{OAuth2Manager, OAuthProvider};

    let oauth_provider = match provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => OAuthProvider::Google,
        "googlephotos" | "google_photos" => OAuthProvider::GooglePhotos,
        "dropbox" => OAuthProvider::Dropbox,
        "onedrive" | "microsoft" => OAuthProvider::OneDrive,
        "box" => OAuthProvider::Box,
        "pcloud" => OAuthProvider::PCloud,
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => OAuthProvider::ZohoWorkdrive,
        "yandexdisk" | "yandex_disk" | "yandex" => OAuthProvider::YandexDisk,
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let manager = OAuth2Manager::new();
    Ok(manager.has_tokens(oauth_provider))
}

/// Clear OAuth2 tokens for a provider (logout)
#[tauri::command]
pub async fn oauth2_logout(provider: String) -> Result<(), String> {
    use crate::providers::{OAuth2Manager, OAuthProvider};

    let oauth_provider = match provider.to_lowercase().as_str() {
        "google_drive" | "googledrive" | "google" => OAuthProvider::Google,
        "googlephotos" | "google_photos" => OAuthProvider::GooglePhotos,
        "dropbox" => OAuthProvider::Dropbox,
        "onedrive" | "microsoft" => OAuthProvider::OneDrive,
        "box" => OAuthProvider::Box,
        "pcloud" => OAuthProvider::PCloud,
        "zoho" | "zoho_workdrive" | "zohoworkdrive" => OAuthProvider::ZohoWorkdrive,
        "yandexdisk" | "yandex_disk" | "yandex" => OAuthProvider::YandexDisk,
        other => return Err(format!("Unknown OAuth2 provider: {}", other)),
    };

    let manager = OAuth2Manager::new();
    manager
        .clear_tokens(oauth_provider)
        .map_err(|e| format!("Failed to clear tokens: {}", e))?;

    info!("Logged out from {}", provider);
    Ok(())
}

/// Create a shareable link for a file using the OAuth provider's native sharing API
#[tauri::command]
pub async fn provider_create_share_link(
    state: State<'_, ProviderState>,
    path: String,
    expires_in_secs: Option<u64>,
    password: Option<String>,
    permissions: Option<String>,
) -> Result<ShareLinkResult, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_share_links() {
        return Err(format!(
            "{} does not support native share links",
            provider.provider_type()
        ));
    }

    let options = ShareLinkOptions {
        expires_in_secs,
        password,
        permissions,
    };

    let result = provider
        .create_share_link(&path, options)
        .await
        .map_err(|e| format!("Failed to create share link: {}", e))?;

    info!("Created share link for {}: {}", path, result.url);
    Ok(result)
}

/// Query share link capabilities for the current provider
#[tauri::command]
pub async fn provider_share_link_capabilities(
    state: State<'_, ProviderState>,
) -> Result<ShareLinkCapabilities, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    Ok(provider.share_link_capabilities())
}

/// Remove a share/export link for a file or folder
#[tauri::command]
pub async fn provider_remove_share_link(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .remove_share_link(&path)
        .await
        .map_err(|e| format!("Failed to remove share link: {}", e))?;

    info!("Removed share link for {}", path);
    Ok(())
}

/// List existing share links for a file or folder
#[tauri::command]
pub async fn provider_list_share_links(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<crate::providers::ShareLinkInfo>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .list_share_links(&path)
        .await
        .map_err(|e| format!("Failed to list share links: {}", e))
}

/// Import a file/folder from a public link into the account
#[tauri::command]
pub async fn provider_import_link(
    state: State<'_, ProviderState>,
    link: String,
    dest: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_import_link() {
        return Err(format!(
            "{} does not support importing from links",
            provider.provider_type()
        ));
    }

    provider
        .import_link(&link, &dest)
        .await
        .map_err(|e| format!("Failed to import link: {}", e))?;

    info!("Imported link to {}", dest);
    Ok(())
}

/// Get storage quota information (used/total/free bytes)
#[tauri::command]
pub async fn provider_storage_info(state: State<'_, ProviderState>) -> Result<StorageInfo, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .storage_info()
        .await
        .map_err(|e| format!("Failed to get storage info: {}", e))
}

/// Get disk usage for a path in bytes
#[tauri::command]
pub async fn provider_disk_usage(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<u64, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .disk_usage(&path)
        .await
        .map_err(|e| format!("Failed to get disk usage: {}", e))
}

/// Search for files matching a pattern under the given path
#[tauri::command]
pub async fn provider_find(
    state: State<'_, ProviderState>,
    path: String,
    pattern: String,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if !provider.supports_find() {
        return Err(format!(
            "{} does not support remote search",
            provider.provider_type()
        ));
    }

    provider
        .find(&path, &pattern)
        .await
        .map_err(|e| format!("Search failed: {}", e))
}

/// Set transfer speed limits (KB/s, 0 = unlimited)
#[tauri::command]
pub async fn provider_set_speed_limit(
    state: State<'_, ProviderState>,
    upload_kb: u64,
    download_kb: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .set_speed_limit(upload_kb, download_kb)
        .await
        .map_err(|e| format!("Failed to set speed limit: {}", e))
}

/// Get current transfer speed limits (upload_kb, download_kb) in KB/s
#[tauri::command]
pub async fn provider_get_speed_limit(
    state: State<'_, ProviderState>,
) -> Result<(u64, u64), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .get_speed_limit()
        .await
        .map_err(|e| format!("Failed to get speed limit: {}", e))
}

/// Check if the current provider supports resume transfers
#[tauri::command]
pub async fn provider_supports_resume(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_resume())
}

/// Resume a download from a given byte offset
#[tauri::command]
pub async fn provider_resume_download(
    state: State<'_, ProviderState>,
    remote_path: String,
    local_path: String,
    offset: u64,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    provider
        .resume_download(&remote_path, &local_path, offset, None)
        .await
        .map_err(|e| format!("Resume download failed: {}", e))?;

    Ok(format!("Resume download completed: {}", remote_path))
}

/// Resume an upload from a given byte offset
#[tauri::command]
pub async fn provider_resume_upload(
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    offset: u64,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    provider
        .resume_upload(&local_path, &remote_path, offset, None)
        .await
        .map_err(|e| format!("Resume upload failed: {}", e))?;

    Ok(format!("Resume upload completed: {}", remote_path))
}

// --- File Versions ---

#[tauri::command]
pub async fn provider_supports_versions(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_versions())
}

#[tauri::command]
pub async fn provider_list_versions(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<FileVersion>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .list_versions(&path)
        .await
        .map_err(|e| format!("List versions failed: {}", e))
}

#[tauri::command]
pub async fn provider_download_version(
    state: State<'_, ProviderState>,
    path: String,
    version_id: String,
    local_path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .download_version(&path, &version_id, &local_path)
        .await
        .map_err(|e| format!("Download version failed: {}", e))?;
    Ok(format!("Downloaded version {} of {}", version_id, path))
}

#[tauri::command]
pub async fn provider_restore_version(
    state: State<'_, ProviderState>,
    path: String,
    version_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .restore_version(&path, &version_id)
        .await
        .map_err(|e| format!("Restore version failed: {}", e))
}

// --- File Locking ---

#[tauri::command]
pub async fn provider_supports_locking(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_locking())
}

#[tauri::command]
pub async fn provider_lock_file(
    state: State<'_, ProviderState>,
    path: String,
    timeout: u64,
) -> Result<LockInfo, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .lock_file(&path, timeout)
        .await
        .map_err(|e| format!("Lock failed: {}", e))
}

#[tauri::command]
pub async fn provider_unlock_file(
    state: State<'_, ProviderState>,
    path: String,
    lock_token: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .unlock_file(&path, &lock_token)
        .await
        .map_err(|e| format!("Unlock failed: {}", e))
}

// --- Thumbnails ---

#[tauri::command]
pub async fn provider_supports_thumbnails(state: State<'_, ProviderState>) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_thumbnails())
}

#[tauri::command]
pub async fn provider_get_thumbnail(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .get_thumbnail(&path)
        .await
        .map_err(|e| format!("Get thumbnail failed: {}", e))
}

// --- Permissions / Advanced Sharing ---

#[tauri::command]
pub async fn provider_supports_permissions(
    state: State<'_, ProviderState>,
) -> Result<bool, String> {
    let provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_ref()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    Ok(provider.supports_permissions())
}

#[tauri::command]
pub async fn provider_list_permissions(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<SharePermission>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .list_permissions(&path)
        .await
        .map_err(|e| format!("List permissions failed: {}", e))
}

#[tauri::command]
pub async fn provider_add_permission(
    state: State<'_, ProviderState>,
    path: String,
    role: String,
    target_type: String,
    target: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    let perm = SharePermission {
        role,
        target_type,
        target,
    };
    provider
        .add_permission(&path, &perm)
        .await
        .map_err(|e| format!("Add permission failed: {}", e))
}

#[tauri::command]
pub async fn provider_remove_permission(
    state: State<'_, ProviderState>,
    path: String,
    target: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    provider
        .remove_permission(&path, &target)
        .await
        .map_err(|e| format!("Remove permission failed: {}", e))
}

/// Compare local and remote directories using the StorageProvider trait.
/// Works with all protocols (SFTP, WebDAV, S3, Google Drive, etc.)
#[tauri::command]
pub async fn provider_compare_directories(
    app: AppHandle,
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
    options: Option<crate::sync::CompareOptions>,
) -> Result<Vec<crate::sync::FileComparison>, String> {
    use crate::sync::{
        build_comparison_results_with_index, load_sync_index, should_exclude, FileInfo,
    };
    use std::collections::HashMap;

    let options = options.unwrap_or_default();

    info!(
        "Provider compare: local={}, remote={}",
        local_path, remote_path
    );

    // Reset the provider cancel flag: takes ownership for this compare run.
    // The user's next Cancel click flips it back to true and the scan stops.
    state
        .cancel_flag
        .store(false, std::sync::atomic::Ordering::Relaxed);

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "local", "files_found": 0,
        }),
    );

    // Get local files (reuse the same logic from lib.rs).
    // Pass the AppHandle so the scan emits throttled progress events -
    // otherwise large trees (e.g. a home directory) look like a stall.
    let local_files = crate::get_local_files_recursive_with_progress(
        &local_path,
        &local_path,
        &options.exclude_patterns,
        options.compare_checksum,
        Some(&state.cancel_flag),
        Some(&app),
    )
    .await
    .map_err(|e| format!("Failed to scan local directory: {}", e))?;

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "remote", "files_found": local_files.len(),
        }),
    );

    // Get remote files via provider - lock/unlock per directory to avoid blocking other operations
    let mut remote_files: HashMap<String, FileInfo> = HashMap::new();
    let mut dirs_to_process = vec![remote_path.clone()];

    // First check we're connected
    {
        let provider_lock = state.provider.lock().await;
        if provider_lock.is_none() {
            return Err("Not connected to any provider".to_string());
        }
    }

    while let Some(current_dir) = dirs_to_process.pop() {
        // Abort the remote scan if the user cancelled from the UI.
        // Without this, the walk keeps listing directories until the tree is
        // exhausted, which can look like a runaway scan on large providers.
        if state.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Compare cancelled by user".to_string());
        }

        // Lock provider only for this single list operation, then release
        let entries = {
            let mut provider_lock = state.provider.lock().await;
            let provider = provider_lock
                .as_mut()
                .ok_or("Not connected to any provider")?;
            provider
                .list(&current_dir)
                .await
                .map_err(|e| format!("Failed to list {}: {}", current_dir, e))?
        };

        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let relative_path = if current_dir == remote_path {
                entry.name.clone()
            } else {
                let rel_dir = current_dir
                    .strip_prefix(&remote_path)
                    .unwrap_or(&current_dir);
                let rel_dir = rel_dir.trim_start_matches('/');
                if rel_dir.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", rel_dir, entry.name)
                }
            };

            if should_exclude(&relative_path, &options.exclude_patterns) {
                continue;
            }

            let modified = entry.modified.and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok()
                    .or_else(|| {
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
                    })
            });

            let file_info = FileInfo {
                name: entry.name.clone(),
                path: entry.path.clone(),
                size: entry.size,
                modified,
                is_dir: entry.is_dir,
                checksum: None,
            };

            remote_files.insert(relative_path, file_info);

            if entry.is_dir {
                let sub_path = if current_dir.ends_with('/') {
                    format!("{}{}", current_dir, entry.name)
                } else {
                    format!("{}/{}", current_dir, entry.name)
                };
                dirs_to_process.push(sub_path);
            }
        }

        let _ = app.emit(
            "sync_scan_progress",
            serde_json::json!({
                "phase": "remote",
                "files_found": local_files.len() + remote_files.len(),
            }),
        );
    }

    let _ = app.emit(
        "sync_scan_progress",
        serde_json::json!({
            "phase": "comparing",
            "files_found": local_files.len() + remote_files.len(),
        }),
    );

    let index = load_sync_index(&local_path, &remote_path).ok().flatten();
    let results =
        build_comparison_results_with_index(local_files, remote_files, &options, index.as_ref());
    info!(
        "Provider compare complete: {} differences found (index: {})",
        results.len(),
        if index.is_some() { "used" } else { "none" }
    );

    Ok(results)
}

// ============ 4shared OAuth 1.0 Commands ============

/// Parameters for 4shared OAuth 1.0 authentication
#[derive(Debug, Clone, Deserialize)]
pub struct FourSharedAuthParams {
    pub consumer_key: String,
    pub consumer_secret: String,
}

/// Result from starting 4shared OAuth flow
#[derive(Debug, Clone, Serialize)]
pub struct FourSharedAuthStarted {
    pub auth_url: String,
    pub request_token: String,
    pub request_token_secret: String,
}

/// Vault key for 4shared OAuth tokens
const FOURSHARED_TOKEN_KEY: &str = "oauth_fourshared";

/// Store 4shared tokens in credential vault (same pattern as OAuth2)
fn store_fourshared_tokens(access_token: &str, access_token_secret: &str) -> Result<(), String> {
    let token_data = format!("{}:{}", access_token, access_token_secret);

    // Try vault first
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        store
            .store(FOURSHARED_TOKEN_KEY, &token_data)
            .map_err(|e| format!("Failed to store tokens: {}", e))?;
        return Ok(());
    }

    // Try auto-init vault
    if crate::credential_store::CredentialStore::init().is_ok() {
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            store
                .store(FOURSHARED_TOKEN_KEY, &token_data)
                .map_err(|e| format!("Failed to store tokens: {}", e))?;
            return Ok(());
        }
    }

    Err("Credential vault not available. Please unlock the vault first.".to_string())
}

/// Load 4shared tokens from credential vault
fn load_fourshared_tokens() -> Result<(String, String), String> {
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        if let Ok(data) = store.get(FOURSHARED_TOKEN_KEY) {
            let parts: Vec<&str> = data.splitn(2, ':').collect();
            if parts.len() == 2 {
                return Ok((parts[0].to_string(), parts[1].to_string()));
            }
        }
    }
    Err("No 4shared tokens found. Please authenticate first.".to_string())
}

/// Start 4shared OAuth 1.0 flow: obtain request token, return auth URL
#[tauri::command]
pub async fn fourshared_start_auth(
    params: FourSharedAuthParams,
) -> Result<FourSharedAuthStarted, String> {
    use crate::providers::oauth1;

    info!("Starting 4shared OAuth 1.0 flow");

    // Bind a local callback listener to get a port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind callback listener: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get listener port: {}", e))?
        .port();
    drop(listener);

    let callback_url = format!("http://127.0.0.1:{}/callback", port);

    let (request_token, request_token_secret) = oauth1::request_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/initiate",
        &callback_url,
    )
    .await?;

    let auth_url = oauth1::authorize_url(
        "https://api.4shared.com/v1_2/oauth/authorize",
        &request_token,
    );

    if let Err(e) = open::that(&auth_url) {
        info!("Could not open browser: {}", e);
    }

    Ok(FourSharedAuthStarted {
        auth_url,
        request_token,
        request_token_secret,
    })
}

/// Complete 4shared OAuth 1.0 flow: exchange request token + verifier for access token
#[tauri::command]
pub async fn fourshared_complete_auth(
    params: FourSharedAuthParams,
    request_token: String,
    request_token_secret: String,
    verifier: String,
) -> Result<String, String> {
    use crate::providers::oauth1;

    info!("Completing 4shared OAuth 1.0 flow");

    let (access_token, access_token_secret) = oauth1::access_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/token",
        &request_token,
        &request_token_secret,
        &verifier,
    )
    .await?;

    store_fourshared_tokens(&access_token, &access_token_secret)?;

    info!("4shared OAuth 1.0 authentication completed successfully");
    Ok("Authentication successful".to_string())
}

/// Full 4shared OAuth 1.0 flow: start server, open browser, wait for callback, exchange tokens
#[tauri::command]
pub async fn fourshared_full_auth(params: FourSharedAuthParams) -> Result<String, String> {
    use crate::providers::oauth1;

    info!("Starting full 4shared OAuth 1.0 flow");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind callback listener: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get listener port: {}", e))?
        .port();

    let callback_url = format!("http://127.0.0.1:{}/callback", port);

    // Step 1: Request token
    let (request_token, request_token_secret) = oauth1::request_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/initiate",
        &callback_url,
    )
    .await?;

    // Step 2: Open authorization URL
    let auth_url = oauth1::authorize_url(
        "https://api.4shared.com/v1_2/oauth/authorize",
        &request_token,
    );

    if let Err(e) = open::that(&auth_url) {
        return Err(format!(
            "Could not open browser: {}. Open manually: {}",
            e, auth_url
        ));
    }

    info!(
        "Browser opened, waiting for OAuth 1.0 callback on port {}...",
        port
    );

    // Step 3: Wait for callback
    let (token, verifier) = tokio::time::timeout(
        tokio::time::Duration::from_secs(300),
        wait_for_oauth1_callback(listener),
    )
    .await
    .map_err(|_| "OAuth timeout: no response within 5 minutes".to_string())?
    .map_err(|e| format!("Callback error: {}", e))?;

    if token != request_token {
        return Err("OAuth token mismatch: possible CSRF attack".to_string());
    }

    // Step 4: Exchange for access token
    let (access_token, access_token_secret) = oauth1::access_token(
        &params.consumer_key,
        &params.consumer_secret,
        "https://api.4shared.com/v1_2/oauth/token",
        &request_token,
        &request_token_secret,
        &verifier,
    )
    .await?;

    store_fourshared_tokens(&access_token, &access_token_secret)?;

    info!("4shared OAuth 1.0 full auth completed successfully");
    Ok("Authentication successful! You can now connect.".to_string())
}

/// Wait for OAuth 1.0 callback (returns oauth_token, oauth_verifier).
/// oauth_verifier is optional: 4shared uses OAuth 1.0 (not 1.0a) and does NOT send a verifier.
/// Accepts connections in a loop to handle browser prefetch/favicon requests.
async fn wait_for_oauth1_callback(
    listener: tokio::net::TcpListener,
) -> Result<(String, String), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Accept connections in a loop: browsers may send favicon or prefetch requests first
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| format!("Accept error: {}", e))?;

        let mut buf = vec![0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| format!("Read error: {}", e))?;

        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the request line: GET /callback?oauth_token=xxx HTTP/1.1
        let request_path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("");

        // Ignore non-callback requests (favicon, prefetch, etc.)
        if !request_path.starts_with("/callback") {
            let response_404 = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(response_404.as_bytes()).await;
            let _ = stream.shutdown().await;
            continue;
        }

        let query = request_path.split('?').nth(1).unwrap_or("");

        let params: std::collections::HashMap<&str, &str> = query
            .split('&')
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                Some((parts.next()?, parts.next()?))
            })
            .collect();

        let oauth_token = params
            .get("oauth_token")
            .ok_or("Missing oauth_token in callback")?
            .to_string();
        // oauth_verifier is optional: 4shared (OAuth 1.0, not 1.0a) doesn't send it
        let oauth_verifier = params
            .get("oauth_verifier")
            .map(|v| v.to_string())
            .unwrap_or_default();

        let response = r#"HTTP/1.1 200 OK
Content-Type: text/html; charset=utf-8
Connection: close

<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>AeroFTP - Authorization Complete</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            min-height: 100vh;
            display: flex;
            justify-content: center;
            align-items: center;
            background: linear-gradient(135deg, #0f0f1a 0%, #1a1a2e 50%, #16213e 100%);
            color: #fff;
            overflow: hidden;
        }
        .bg-particles {
            position: fixed; top: 0; left: 0; width: 100%; height: 100%;
            pointer-events: none; overflow: hidden; z-index: 0;
        }
        .particle {
            position: absolute; width: 4px; height: 4px;
            background: rgba(0, 212, 255, 0.3); border-radius: 50%;
            animation: float 15s infinite;
        }
        .particle:nth-child(1) { left: 10%; animation-delay: 0s; }
        .particle:nth-child(2) { left: 20%; animation-delay: 2s; }
        .particle:nth-child(3) { left: 30%; animation-delay: 4s; }
        .particle:nth-child(4) { left: 40%; animation-delay: 6s; }
        .particle:nth-child(5) { left: 50%; animation-delay: 8s; }
        .particle:nth-child(6) { left: 60%; animation-delay: 10s; }
        .particle:nth-child(7) { left: 70%; animation-delay: 12s; }
        .particle:nth-child(8) { left: 80%; animation-delay: 14s; }
        .particle:nth-child(9) { left: 90%; animation-delay: 1s; }
        .particle:nth-child(10) { left: 95%; animation-delay: 3s; }
        @keyframes float {
            0%, 100% { transform: translateY(100vh) scale(0); opacity: 0; }
            10% { opacity: 1; } 90% { opacity: 1; }
            100% { transform: translateY(-100vh) scale(1); opacity: 0; }
        }
        .container {
            position: relative; z-index: 1; text-align: center;
            padding: 60px 50px;
            background: rgba(22, 33, 62, 0.8);
            backdrop-filter: blur(20px); border-radius: 24px;
            box-shadow: 0 25px 80px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(255, 255, 255, 0.1);
            max-width: 440px; animation: slideUp 0.6s ease-out;
        }
        @keyframes slideUp {
            from { opacity: 0; transform: translateY(30px); }
            to { opacity: 1; transform: translateY(0); }
        }
        .logo { margin-bottom: 30px; }
        .app-name {
            font-size: 28px; font-weight: 700;
            background: linear-gradient(135deg, #00d4ff, #0099ff);
            -webkit-background-clip: text; -webkit-text-fill-color: transparent;
            background-clip: text; margin-top: 12px; letter-spacing: -0.5px;
        }
        .success-icon {
            width: 90px; height: 90px; margin: 20px auto 30px;
            background: linear-gradient(135deg, #00d4ff, #00ff88);
            border-radius: 50%; display: flex;
            justify-content: center; align-items: center;
            animation: pulse 2s infinite;
            box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3);
        }
        @keyframes pulse {
            0%, 100% { box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3); }
            50% { box-shadow: 0 10px 60px rgba(0, 212, 255, 0.5); }
        }
        .success-icon svg {
            width: 45px; height: 45px; stroke: #fff;
            stroke-width: 3; fill: none;
            animation: checkmark 0.8s ease-out 0.3s both;
        }
        @keyframes checkmark {
            from { stroke-dashoffset: 50; }
            to { stroke-dashoffset: 0; }
        }
        .success-icon svg path { stroke-dasharray: 50; stroke-dashoffset: 0; }
        h1 { font-size: 26px; font-weight: 600; color: #fff; margin-bottom: 12px; }
        .subtitle {
            font-size: 16px; color: rgba(255, 255, 255, 0.7);
            line-height: 1.6; margin-bottom: 30px;
        }
        .provider-badge {
            display: inline-flex; align-items: center; gap: 8px;
            padding: 10px 20px; background: rgba(255, 255, 255, 0.1);
            border-radius: 30px; font-size: 14px;
            color: rgba(255, 255, 255, 0.9); margin-bottom: 30px;
        }
        .provider-badge svg { width: 20px; height: 20px; }
        .close-hint {
            font-size: 13px; color: rgba(255, 255, 255, 0.5);
            padding-top: 20px; border-top: 1px solid rgba(255, 255, 255, 0.1);
        }
        .close-hint kbd {
            display: inline-block; padding: 2px 8px;
            background: rgba(255, 255, 255, 0.1); border-radius: 4px;
            font-family: monospace; font-size: 12px; margin: 0 2px;
        }
    </style>
</head>
<body>
    <div class="bg-particles">
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
        <div class="particle"></div><div class="particle"></div>
    </div>
    <div class="container">
        <div class="logo">
            <div class="app-name">AeroFTP</div>
        </div>
        <div class="success-icon">
            <svg viewBox="0 0 24 24">
                <path d="M5 13l4 4L19 7" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
        </div>
        <h1>Authorization Successful</h1>
        <p class="subtitle">Your 4shared account has been connected securely.<br>You're all set to access your files!</p>
        <div class="provider-badge">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"/>
            </svg>
            4shared Connected
        </div>
        <p class="close-hint">You can close this window and return to AeroFTP<br>or press <kbd>Alt</kbd> + <kbd>F4</kbd></p>
    </div>
</body>
</html>"#;
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;

        return Ok((oauth_token, oauth_verifier));
    }
}

/// Connect to 4shared after authentication
#[tauri::command]
pub async fn fourshared_connect(
    state: State<'_, ProviderState>,
    params: FourSharedAuthParams,
) -> Result<OAuth2ConnectResult, String> {
    use crate::providers::{types::FourSharedConfig, FourSharedProvider};

    info!("Connecting to 4shared...");

    let (access_token, access_token_secret) = load_fourshared_tokens()?;

    let config = FourSharedConfig {
        consumer_key: params.consumer_key,
        consumer_secret: params.consumer_secret.into(),
        access_token: access_token.into(),
        access_token_secret: access_token_secret.into(),
    };

    let mut provider = FourSharedProvider::new(config);
    provider
        .connect()
        .await
        .map_err(|e| format!("4shared connection failed: {}", e))?;

    let display_name = provider.display_name();
    let account_email = provider.account_email();

    let mut provider_lock = state.provider.lock().await;
    *provider_lock = Some(Box::new(provider));

    info!(
        "Connected to 4shared ({})",
        account_email.as_deref().unwrap_or("no email")
    );
    Ok(OAuth2ConnectResult {
        display_name,
        account_email,
    })
}

// ── Zoho WorkDrive Trash Operations ────────────────────────────────────

/// List trashed files/folders in Zoho WorkDrive (privatespace + team folders)
#[tauri::command]
pub async fn zoho_list_trash(state: State<'_, ProviderState>) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    // Downcast to ZohoWorkdriveProvider
    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))
}

/// Permanently delete files/folders from Zoho WorkDrive trash
#[tauri::command]
pub async fn zoho_permanent_delete(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    if file_ids.len() == 1 {
        zoho.permanent_delete(&file_ids[0])
            .await
            .map_err(|e| format!("Permanent delete failed: {}", e))
    } else {
        zoho.permanent_delete_batch(&file_ids)
            .await
            .map_err(|e| format!("Permanent delete batch failed: {}", e))
    }
}

/// Restore files/folders from Zoho WorkDrive trash to their original location
#[tauri::command]
pub async fn zoho_restore_from_trash(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    if file_ids.len() == 1 {
        zoho.restore_from_trash(&file_ids[0])
            .await
            .map_err(|e| format!("Restore failed: {}", e))
    } else {
        zoho.restore_from_trash_batch(&file_ids)
            .await
            .map_err(|e| format!("Restore batch failed: {}", e))
    }
}

// ── Zoho WorkDrive Label Operations ───────────────────────────────────

/// List all labels available in the Zoho WorkDrive team
#[tauri::command]
pub async fn zoho_list_team_labels(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let labels = zoho
        .list_team_labels()
        .await
        .map_err(|e| format!("Failed to list team labels: {}", e))?;

    Ok(labels
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or_default())
        .collect())
}

/// List labels applied to a specific file in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_get_file_labels(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let labels = zoho
        .get_file_labels(&path)
        .await
        .map_err(|e| format!("Failed to get file labels: {}", e))?;

    Ok(labels
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or_default())
        .collect())
}

/// Add a label to a file in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_add_file_label(
    state: State<'_, ProviderState>,
    path: String,
    label_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.add_file_label(&path, &label_id)
        .await
        .map_err(|e| format!("Failed to add label: {}", e))
}

/// Create a new label in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_create_label(
    state: State<'_, ProviderState>,
    name: String,
    color: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let label = zoho
        .create_label(&name, &color)
        .await
        .map_err(|e| format!("Failed to create label: {}", e))?;

    serde_json::to_value(label).map_err(|e| format!("Serialize error: {}", e))
}

/// Remove a label from a file in Zoho WorkDrive
#[tauri::command]
pub async fn zoho_remove_file_label(
    state: State<'_, ProviderState>,
    path: String,
    label_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.remove_file_label(&path, &label_id)
        .await
        .map_err(|e| format!("Failed to remove label: {}", e))
}

// ── Zoho WorkDrive MCP-parity Operations ──────────────────────────────

/// Get authenticated user info (MCP parity: getUserInfo)
#[tauri::command]
pub async fn zoho_get_user_info(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let info = zoho
        .get_user_info()
        .await
        .map_err(|e| format!("Failed to get user info: {}", e))?;

    serde_json::to_value(info).map_err(|e| format!("Serialize error: {}", e))
}

/// List all share links for a file/folder (MCP parity: getFileShareLinks)
#[tauri::command]
pub async fn zoho_get_file_share_links(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    let links = zoho
        .get_file_share_links(&path)
        .await
        .map_err(|e| format!("Failed to get share links: {}", e))?;

    Ok(links
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or_default())
        .collect())
}

/// Delete an external share link (MCP parity: deleteExternalShareLink)
#[tauri::command]
pub async fn zoho_delete_share_link(
    state: State<'_, ProviderState>,
    link_id: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.delete_share_link(&link_id)
        .await
        .map_err(|e| format!("Failed to delete share link: {}", e))
}

/// Create a native Zoho document (MCP parity: createNativeDocument)
/// doc_type: "writer"/"zw", "sheet"/"zs", "show"/"presentation"/"zp"
#[tauri::command]
pub async fn zoho_create_native_document(
    state: State<'_, ProviderState>,
    name: String,
    doc_type: String,
    folder_path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::ZohoWorkdrive {
        return Err("This operation is only available for Zoho WorkDrive".to_string());
    }

    let zoho = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::zoho_workdrive::ZohoWorkdriveProvider>()
        .ok_or_else(|| "Failed to access Zoho WorkDrive provider".to_string())?;

    zoho.create_native_document(&name, &doc_type, &folder_path)
        .await
        .map_err(|e| format!("Failed to create native document: {}", e))
}

// ── Jottacloud Trash Operations ───────────────────────────────────────

/// Move files to Jottacloud Trash (soft delete)
#[tauri::command]
pub async fn jottacloud_move_to_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    for path in &paths {
        jotta
            .move_to_trash(path)
            .await
            .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List items in Jottacloud Trash
#[tauri::command]
pub async fn jottacloud_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    jotta
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))
}

/// Restore files from Jottacloud Trash to their original location
#[tauri::command]
pub async fn jottacloud_restore_from_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    for path in &paths {
        jotta
            .restore_from_trash(path)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Permanently delete files from Jottacloud Trash
#[tauri::command]
pub async fn jottacloud_permanent_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Jottacloud {
        return Err("This operation is only available for Jottacloud".to_string());
    }

    let jotta = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::jottacloud::JottacloudProvider>()
        .ok_or_else(|| "Failed to access Jottacloud provider".to_string())?;

    for path in &paths {
        jotta
            .permanent_delete_from_trash(path)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", path, e))?;
    }
    Ok(())
}

// ── MEGA Trash Operations ────────────────────────────────────────────

/// Move files to MEGA Rubbish Bin (soft delete)
#[tauri::command]
pub async fn mega_move_to_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    // Try native provider first, then MEGAcmd
    if let Some(native) = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        for path in &paths {
            native
                .move_to_trash(path)
                .await
                .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
        }
        return Ok(());
    }

    let mega = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    for path in &paths {
        mega.move_to_trash(path)
            .await
            .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List items in MEGA Rubbish Bin
#[tauri::command]
pub async fn mega_list_trash(state: State<'_, ProviderState>) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    if let Some(native) = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        return native
            .list_trash()
            .await
            .map_err(|e| format!("Failed to list trash: {}", e));
    }

    let mega = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    mega.list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))
}

/// Restore files from MEGA Rubbish Bin to cloud root
#[tauri::command]
pub async fn mega_restore_from_trash(
    state: State<'_, ProviderState>,
    filenames: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    if let Some(native) = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        let cwd = native.pwd().await.unwrap_or_else(|_| "/".to_string());
        for filename in &filenames {
            native
                .restore_from_trash(filename, &cwd)
                .await
                .map_err(|e| format!("Restore failed for {}: {}", filename, e))?;
        }
        return Ok(());
    }

    let mega = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    let cwd = mega.pwd().await.unwrap_or_else(|_| "/".to_string());
    for filename in &filenames {
        mega.restore_from_trash(filename, &cwd)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", filename, e))?;
    }
    Ok(())
}

/// Permanently delete files from MEGA Rubbish Bin
#[tauri::command]
pub async fn mega_permanent_delete(
    state: State<'_, ProviderState>,
    filenames: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::Mega {
        return Err("This operation is only available for MEGA".to_string());
    }

    if let Some(native) = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega_native::MegaNativeProvider>()
    {
        for filename in &filenames {
            native
                .permanent_delete_from_trash(filename)
                .await
                .map_err(|e| format!("Permanent delete failed for {}: {}", filename, e))?;
        }
        return Ok(());
    }

    let mega = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::mega::MegaCmdProvider>()
        .ok_or_else(|| "Failed to access MEGA provider".to_string())?;

    for filename in &filenames {
        mega.permanent_delete_from_trash(filename)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", filename, e))?;
    }
    Ok(())
}

// ── Google Drive Trash Operations ────────────────────────────────────

/// Move files to Google Drive Trash (soft delete)
#[tauri::command]
pub async fn google_drive_trash_file(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    for path in &paths {
        gdrive
            .trash_file(path)
            .await
            .map_err(|e| format!("Move to trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List items in Google Drive Trash
#[tauri::command]
pub async fn google_drive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    gdrive
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))
}

/// Restore files from Google Drive Trash
#[tauri::command]
pub async fn google_drive_restore_from_trash(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    for file_id in &file_ids {
        gdrive
            .restore_from_trash(file_id)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", file_id, e))?;
    }
    Ok(())
}

/// Permanently delete files from Google Drive Trash
#[tauri::command]
pub async fn google_drive_permanent_delete(
    state: State<'_, ProviderState>,
    file_ids: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("This operation is only available for Google Drive".to_string());
    }

    let gdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Failed to access Google Drive provider".to_string())?;

    for file_id in &file_ids {
        gdrive
            .permanent_delete(file_id)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", file_id, e))?;
    }
    Ok(())
}

// ── OpenDrive Trash Operations ──────────────────────────────────────

/// List items in OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    opendrive
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))
}

/// Restore items from OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_restore_from_trash(
    state: State<'_, ProviderState>,
    items: Vec<OpenDriveTrashActionItem>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    for item in &items {
        opendrive
            .restore_from_trash(&item.item_id, item.is_dir)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", item.item_id, e))?;
    }
    Ok(())
}

/// Permanently delete items from OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_permanent_delete(
    state: State<'_, ProviderState>,
    items: Vec<OpenDriveTrashActionItem>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    for item in &items {
        opendrive
            .permanent_delete_from_trash(&item.item_id, item.is_dir)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", item.item_id, e))?;
    }
    Ok(())
}

/// Empty OpenDrive Trash.
#[tauri::command]
pub async fn opendrive_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    opendrive
        .empty_trash()
        .await
        .map_err(|e| format!("Empty trash failed: {}", e))
}

/// Set OpenDrive privacy for a file or folder.
/// is_public=false => private, is_public=true => public.
#[tauri::command]
pub async fn opendrive_set_path_privacy(
    state: State<'_, ProviderState>,
    path: String,
    is_public: bool,
    is_dir: bool,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::OpenDrive {
        return Err("This operation is only available for OpenDrive".to_string());
    }

    let opendrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::opendrive::OpenDriveProvider>()
        .ok_or_else(|| "Failed to access OpenDrive provider".to_string())?;

    if is_dir {
        opendrive
            .set_folder_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set folder privacy failed for {}: {}", path, e))
    } else {
        opendrive
            .set_file_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set file privacy failed for {}: {}", path, e))
    }
}

/// Set FourShared privacy for a file or folder.
/// is_public=false => private, is_public=true => public.
#[tauri::command]
pub async fn fourshared_set_path_privacy(
    state: State<'_, ProviderState>,
    path: String,
    is_public: bool,
    is_dir: bool,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::FourShared {
        return Err("This operation is only available for FourShared".to_string());
    }

    let fourshared = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::fourshared::FourSharedProvider>()
        .ok_or_else(|| "Failed to access FourShared provider".to_string())?;

    if is_dir {
        fourshared
            .set_folder_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set folder privacy failed for {}: {}", path, e))
    } else {
        fourshared
            .set_file_privacy(&path, is_public)
            .await
            .map_err(|e| format!("Set file privacy failed for {}: {}", path, e))
    }
}

// ─── Yandex Disk-Specific Commands ────────────────────────────────────────

/// List items in Yandex Disk trash
#[tauri::command]
pub async fn yandex_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    yandex
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))
}

/// Restore items from Yandex Disk trash
#[tauri::command]
pub async fn yandex_restore_from_trash(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    for path in &paths {
        yandex
            .restore_from_trash(path)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Permanently delete items from Yandex Disk trash
#[tauri::command]
pub async fn yandex_permanent_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    for path in &paths {
        yandex
            .permanent_delete_from_trash(path)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Empty Yandex Disk trash
#[tauri::command]
pub async fn yandex_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::YandexDisk {
        return Err("This operation is only available for Yandex Disk".to_string());
    }

    let yandex = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::yandex_disk::YandexDiskProvider>()
        .ok_or_else(|| "Failed to access Yandex Disk provider".to_string())?;

    yandex
        .empty_trash()
        .await
        .map_err(|e| format!("Empty trash failed: {}", e))
}

// ─── Box-Specific Commands ────────────────────────────────────────────────

/// List items in Box trash
#[tauri::command]
pub async fn box_list_trash(state: State<'_, ProviderState>) -> Result<Vec<RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.list_trash()
        .await
        .map_err(|e| format!("List trash failed: {}", e))
}

/// Move files/folders to Box trash (soft delete)
#[tauri::command]
pub async fn box_trash_files(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.trash_files(&paths)
        .await
        .map_err(|e| format!("Trash failed: {}", e))
}

/// Restore an item from Box trash
#[tauri::command]
pub async fn box_restore_from_trash(
    state: State<'_, ProviderState>,
    item_id: String,
    item_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.restore_from_trash(&item_id, &item_type)
        .await
        .map_err(|e| format!("Restore failed: {}", e))
}

/// Permanently delete an item from Box trash
#[tauri::command]
pub async fn box_permanent_delete(
    state: State<'_, ProviderState>,
    item_id: String,
    item_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.permanent_delete_from_trash(&item_id, &item_type)
        .await
        .map_err(|e| format!("Permanent delete failed: {}", e))
}

/// Move a file or folder to a different parent folder on Box
#[tauri::command]
pub async fn box_move_file(
    state: State<'_, ProviderState>,
    from_path: String,
    to_folder: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.move_item(&from_path, &to_folder)
        .await
        .map_err(|e| format!("Move failed: {}", e))
}

/// List comments on a Box file
#[tauri::command]
pub async fn box_list_comments(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    let comments = bx
        .list_comments(&path)
        .await
        .map_err(|e| format!("List comments failed: {}", e))?;
    serde_json::to_value(&comments)
        .map(|v| v.as_array().cloned().unwrap_or_default())
        .map_err(|e| format!("Serialize failed: {}", e))
}

/// Add a comment to a Box file
#[tauri::command]
pub async fn box_add_comment(
    state: State<'_, ProviderState>,
    path: String,
    message: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.add_comment(&path, &message)
        .await
        .map_err(|e| format!("Add comment failed: {}", e))
}

/// Delete a comment on Box
#[tauri::command]
pub async fn box_delete_comment(
    state: State<'_, ProviderState>,
    comment_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.delete_comment(&comment_id)
        .await
        .map_err(|e| format!("Delete comment failed: {}", e))
}

/// Add a collaboration on a Box file or folder
#[tauri::command]
pub async fn box_add_collaboration(
    state: State<'_, ProviderState>,
    path: String,
    email: String,
    role: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.add_collaboration(&path, &email, &role)
        .await
        .map_err(|e| format!("Add collaboration failed: {}", e))
}

/// Remove a collaboration from Box
#[tauri::command]
pub async fn box_remove_collaboration(
    state: State<'_, ProviderState>,
    collab_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.remove_collaboration(&collab_id)
        .await
        .map_err(|e| format!("Remove collaboration failed: {}", e))
}

/// Apply watermark to a Box file
#[tauri::command]
pub async fn box_set_watermark(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.set_watermark(&path)
        .await
        .map_err(|e| format!("Set watermark failed: {}", e))
}

/// Remove watermark from a Box file
#[tauri::command]
pub async fn box_remove_watermark(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.remove_watermark(&path)
        .await
        .map_err(|e| format!("Remove watermark failed: {}", e))
}

/// Set tags on a Box file or folder
#[tauri::command]
pub async fn box_set_tags(
    state: State<'_, ProviderState>,
    path: String,
    tags: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.set_tags(&path, &tags)
        .await
        .map_err(|e| format!("Set tags failed: {}", e))
}

/// Lock a Box folder
#[tauri::command]
pub async fn box_lock_folder(state: State<'_, ProviderState>, path: String) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.lock_folder(&path)
        .await
        .map_err(|e| format!("Lock folder failed: {}", e))
}

/// Unlock a Box folder by lock ID
#[tauri::command]
pub async fn box_unlock_folder(
    state: State<'_, ProviderState>,
    lock_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    bx.unlock_folder(&lock_id)
        .await
        .map_err(|e| format!("Unlock folder failed: {}", e))
}

/// List collaborations on a Box file or folder
#[tauri::command]
pub async fn box_list_collaborations(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    let collabs = bx
        .list_collaborations(&path)
        .await
        .map_err(|e| format!("List collaborations failed: {}", e))?;
    serde_json::to_value(&collabs)
        .map(|v| v.as_array().cloned().unwrap_or_default())
        .map_err(|e| format!("Serialize failed: {}", e))
}

/// List folder locks on a Box folder
#[tauri::command]
pub async fn box_list_folder_locks(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Box {
        return Err("Only available for Box".to_string());
    }
    let bx = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::box_provider::BoxProvider>()
        .ok_or_else(|| "Box downcast failed".to_string())?;
    let locks = bx
        .list_folder_locks(&path)
        .await
        .map_err(|e| format!("List folder locks failed: {}", e))?;
    serde_json::to_value(&locks)
        .map(|v| v.as_array().cloned().unwrap_or_default())
        .map_err(|e| format!("Serialize failed: {}", e))
}

/// Check if 4shared tokens exist
#[tauri::command]
pub async fn fourshared_has_tokens() -> Result<bool, String> {
    Ok(load_fourshared_tokens().is_ok())
}

/// Clear 4shared tokens (logout)
#[tauri::command]
pub async fn fourshared_logout() -> Result<(), String> {
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        let _ = store.delete(FOURSHARED_TOKEN_KEY);
    }
    info!("Logged out from 4shared");
    Ok(())
}

// ─── FileLu-Specific Commands ─────────────────────────────────────────────

/// Set or unset a file password on FileLu.
/// Pass empty string to remove the password.
#[tauri::command]
pub async fn filelu_set_file_password(
    state: State<'_, ProviderState>,
    path: String,
    password: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_file_password(&path, &password)
        .await
        .map_err(|e| e.to_string())
}

/// Toggle a FileLu file between private (only_me=true) and public.
#[tauri::command]
pub async fn filelu_set_file_privacy(
    state: State<'_, ProviderState>,
    path: String,
    only_me: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_file_privacy(&path, only_me)
        .await
        .map_err(|e| e.to_string())
}

/// Clone a FileLu file server-side. Returns the URL of the cloned file.
#[tauri::command]
pub async fn filelu_clone_file(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.clone_file(&path).await.map_err(|e| e.to_string())
}

/// Set or unset a FileLu folder password (requires folder sharing enabled).
#[tauri::command]
pub async fn filelu_set_folder_password(
    state: State<'_, ProviderState>,
    path: String,
    password: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_folder_password(&path, &password)
        .await
        .map_err(|e| e.to_string())
}

/// Configure FileLu folder settings: filedrop and public visibility.
#[tauri::command]
pub async fn filelu_set_folder_settings(
    state: State<'_, ProviderState>,
    path: String,
    filedrop: bool,
    is_public: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.set_folder_settings(&path, filedrop, is_public)
        .await
        .map_err(|e| e.to_string())
}

/// List all deleted files in FileLu trash.
#[tauri::command]
pub async fn filelu_list_deleted(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::filelu::DeletedFileEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.list_deleted_files().await.map_err(|e| e.to_string())
}

/// Restore a deleted file from FileLu trash by file_code.
#[tauri::command]
pub async fn filelu_restore_file(
    state: State<'_, ProviderState>,
    file_code: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.restore_deleted_file(&file_code)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete a FileLu file from trash by file_code.
#[tauri::command]
pub async fn filelu_permanent_delete(
    state: State<'_, ProviderState>,
    file_code: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.permanent_delete_file(&file_code)
        .await
        .map_err(|e| e.to_string())
}

/// Upload a file from a remote URL to a FileLu folder. Returns file_code.
#[tauri::command]
pub async fn filelu_remote_url_upload(
    state: State<'_, ProviderState>,
    remote_url: String,
    dest_path: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.remote_url_upload(&remote_url, &dest_path)
        .await
        .map_err(|e| e.to_string())
}

/// Restore a deleted folder from FileLu trash by fld_id.
#[tauri::command]
pub async fn filelu_restore_folder(
    state: State<'_, ProviderState>,
    fld_id: u64,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::FileLu {
        return Err("Only available for FileLu".to_string());
    }
    let fl = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filelu::FileLuProvider>()
        .ok_or_else(|| "FileLu downcast failed".to_string())?;
    fl.restore_deleted_folder(fld_id)
        .await
        .map_err(|e| e.to_string())
}

// ─── Google Drive Extended Commands ───────────────────────────────────────

/// Star or unstar files on Google Drive
#[tauri::command]
pub async fn google_drive_set_starred(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
    starred: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    for path in &paths {
        gd.set_starred(path, starred)
            .await
            .map_err(|e| format!("Star failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// List comments on a Google Drive file
#[tauri::command]
pub async fn google_drive_list_comments(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.list_comments(&path).await.map_err(|e| e.to_string())
}

/// Add a comment to a Google Drive file
#[tauri::command]
pub async fn google_drive_add_comment(
    state: State<'_, ProviderState>,
    path: String,
    message: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.add_comment(&path, &message)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a comment from a Google Drive file
#[tauri::command]
pub async fn google_drive_delete_comment(
    state: State<'_, ProviderState>,
    path: String,
    comment_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.delete_comment(&path, &comment_id)
        .await
        .map_err(|e| e.to_string())
}

/// Set custom properties on a Google Drive file
#[tauri::command]
pub async fn google_drive_set_properties(
    state: State<'_, ProviderState>,
    path: String,
    properties: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.set_properties(&path, &properties)
        .await
        .map_err(|e| e.to_string())
}

/// Set description on a Google Drive file
#[tauri::command]
pub async fn google_drive_set_description(
    state: State<'_, ProviderState>,
    path: String,
    description: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::GoogleDrive {
        return Err("Only available for Google Drive".to_string());
    }
    let gd = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::google_drive::GoogleDriveProvider>()
        .ok_or_else(|| "Google Drive downcast failed".to_string())?;
    gd.set_description(&path, &description)
        .await
        .map_err(|e| e.to_string())
}

// ─── Dropbox Extended Commands ────────────────────────────────────────────

/// List items in Dropbox trash (deleted files)
#[tauri::command]
pub async fn dropbox_list_trash(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<Vec<RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.list_deleted(&path).await.map_err(|e| e.to_string())
}

/// Restore a file from Dropbox trash
#[tauri::command]
pub async fn dropbox_restore_from_trash(
    state: State<'_, ProviderState>,
    path: String,
    rev: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.restore_file(&path, &rev)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete a file from Dropbox
#[tauri::command]
pub async fn dropbox_permanent_delete(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.permanent_delete(&path).await.map_err(|e| e.to_string())
}

/// Set tags on a Dropbox file (replaces existing tags)
#[tauri::command]
pub async fn dropbox_set_tags(
    state: State<'_, ProviderState>,
    path: String,
    tags: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.set_tags(&path, &tags).await.map_err(|e| e.to_string())
}

/// Get tags for Dropbox files
#[tauri::command]
pub async fn dropbox_get_tags(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::Dropbox {
        return Err("Only available for Dropbox".to_string());
    }
    let db = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::dropbox::DropboxProvider>()
        .ok_or_else(|| "Dropbox downcast failed".to_string())?;
    db.get_tags(&paths).await.map_err(|e| e.to_string())
}

// ─── OneDrive Extended Commands ───────────────────────────────────────────

/// List items in OneDrive recycle bin
#[tauri::command]
pub async fn onedrive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    od.list_trash().await.map_err(|e| e.to_string())
}

/// Move files to OneDrive recycle bin (soft delete)
#[tauri::command]
pub async fn onedrive_trash_files(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    for path in &paths {
        od.trash_file(path)
            .await
            .map_err(|e| format!("Trash failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Restore an item from OneDrive recycle bin
#[tauri::command]
pub async fn onedrive_restore_from_trash(
    state: State<'_, ProviderState>,
    item_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    od.restore_from_trash(&item_id)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete an item from OneDrive
#[tauri::command]
pub async fn onedrive_permanent_delete(
    state: State<'_, ProviderState>,
    item_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or_else(|| "Not connected".to_string())?;
    if provider.provider_type() != ProviderType::OneDrive {
        return Err("Only available for OneDrive".to_string());
    }
    let od = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::onedrive::OneDriveProvider>()
        .ok_or_else(|| "OneDrive downcast failed".to_string())?;
    od.permanent_delete(&item_id)
        .await
        .map_err(|e| e.to_string())
}

// ─── Folder Size Calculation ───

/// Global cancellation flag for folder size scan
static FOLDER_SIZE_CANCEL: AtomicBool = AtomicBool::new(false);

/// Progress payload emitted during folder size scan
#[derive(Clone, Serialize)]
pub struct FolderSizeProgress {
    total_bytes: u64,
    file_count: u64,
    dir_count: u64,
    scanning: bool,
}

/// Recursively calculate folder size via provider list(): BFS with progress events.
/// Safety: max 50,000 entries, max depth 50.
#[tauri::command]
pub async fn provider_calculate_folder_size(
    state: State<'_, ProviderState>,
    app: AppHandle,
    path: String,
) -> Result<FolderSizeProgress, String> {
    FOLDER_SIZE_CANCEL.store(false, Ordering::Relaxed);

    let mut total_bytes: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;
    let mut entries_scanned: u64 = 0;

    const MAX_ENTRIES: u64 = 50_000;
    const MAX_DEPTH: usize = 50;

    // BFS queue: (path, depth)
    let mut queue: Vec<(String, usize)> = vec![(path, 0)];

    while let Some((current_path, depth)) = queue.pop() {
        if FOLDER_SIZE_CANCEL.load(Ordering::Relaxed) {
            // Cancelled: return partial results
            let result = FolderSizeProgress {
                total_bytes,
                file_count,
                dir_count,
                scanning: false,
            };
            let _ = app.emit("folder-size-progress", &result);
            return Ok(result);
        }

        if depth > MAX_DEPTH || entries_scanned > MAX_ENTRIES {
            break;
        }

        // List directory contents
        let entries = {
            let mut provider_lock = state.provider.lock().await;
            let provider = provider_lock
                .as_mut()
                .ok_or("Not connected to any provider")?;
            provider
                .list(&current_path)
                .await
                .map_err(|e| format!("Failed to list {}: {}", current_path, e))?
        };

        for entry in &entries {
            entries_scanned += 1;
            if entry.is_dir {
                dir_count += 1;
                let subpath = if current_path == "/" || current_path.is_empty() {
                    format!("/{}", entry.name)
                } else if current_path.ends_with('/') {
                    format!("{}{}", current_path, entry.name)
                } else {
                    format!("{}/{}", current_path, entry.name)
                };
                queue.push((subpath, depth + 1));
            } else {
                file_count += 1;
                total_bytes += entry.size;
            }
        }

        // Emit progress every directory listing
        let progress = FolderSizeProgress {
            total_bytes,
            file_count,
            dir_count,
            scanning: true,
        };
        let _ = app.emit("folder-size-progress", &progress);
    }

    let result = FolderSizeProgress {
        total_bytes,
        file_count,
        dir_count,
        scanning: false,
    };
    let _ = app.emit("folder-size-progress", &result);
    Ok(result)
}

/// Cancel an in-progress folder size calculation
#[tauri::command]
pub async fn provider_cancel_folder_size() -> Result<(), String> {
    FOLDER_SIZE_CANCEL.store(true, Ordering::Relaxed);
    Ok(())
}

// ── GitHub-specific commands ──────────────────────────────────────

/// List all branches of the connected GitHub repository
#[tauri::command]
pub async fn github_list_branches(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .list_branches()
        .await
        .map_err(|e| format!("Failed to list branches: {}", e))
}

/// Get info about the connected GitHub repository
#[tauri::command]
pub async fn github_get_info(state: State<'_, ProviderState>) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    Ok(serde_json::json!({
        "owner": github.owner(),
        "repo": github.repo(),
        "branch": github.active_branch(),
        "writeMode": format!("{:?}", github.write_mode()),
        "writeModeKind": match github.write_mode() {
            crate::providers::github::GitHubWriteMode::Unknown => "unknown",
            crate::providers::github::GitHubWriteMode::DirectWrite => "direct",
            crate::providers::github::GitHubWriteMode::DirectWriteProtected { .. } => "direct",
            crate::providers::github::GitHubWriteMode::BranchWorkflow { .. } => "branch",
            crate::providers::github::GitHubWriteMode::ReadOnly { .. } => "readonly",
        },
        "workingBranch": github.working_branch(),
        "repoPrivate": github.is_private(),
    }))
}

// ── GitLab-specific commands ──────────────────────────────────────

/// List all branches of the connected GitLab repository
#[tauri::command]
pub async fn gitlab_list_branches(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }

    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let branches = gitlab
        .list_branches()
        .await
        .map_err(|e| format!("Failed to list branches: {}", e))?;

    Ok(branches
        .iter()
        .map(|b| {
            serde_json::json!({
                "name": b.name,
                "protected": b.is_protected,
                "default": b.is_default,
                "canPush": b.can_push,
            })
        })
        .collect())
}

/// Get info about the connected GitLab repository
#[tauri::command]
pub async fn gitlab_get_info(state: State<'_, ProviderState>) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }

    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let on_non_default = gitlab.active_branch_name() != gitlab.default_branch_name();
    let (write_mode, write_mode_kind, working_branch) = if !gitlab.can_push() {
        ("ReadOnly", "readonly", serde_json::Value::Null)
    } else if on_non_default {
        // On a non-default branch with push access → branch mode (MR available)
        (
            "Branch",
            "branch",
            serde_json::Value::String(gitlab.active_branch_name().to_string()),
        )
    } else {
        ("Direct", "direct", serde_json::Value::Null)
    };

    Ok(serde_json::json!({
        "owner": gitlab.project_path(),
        "repo": gitlab.project_path(),
        "branch": gitlab.active_branch_name(),
        "writeMode": write_mode,
        "writeModeKind": write_mode_kind,
        "workingBranch": working_branch,
        "repoPrivate": gitlab.is_private(),
    }))
}

/// Switch branch on the connected GitLab repository
#[tauri::command]
pub async fn gitlab_switch_branch(
    state: State<'_, ProviderState>,
    branch: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }

    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .switch_branch(&branch)
        .await
        .map_err(|e| format!("Failed to switch branch: {}", e))
}

/// Atomic batch upload of files to GitLab via REST commits API.
#[tauri::command]
pub async fn gitlab_batch_upload(
    state: State<'_, ProviderState>,
    files: Vec<serde_json::Value>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let mut actions = Vec::with_capacity(files.len());
    for file_val in &files {
        let local_path = file_val
            .get("localPath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'localPath'")?;
        let remote_path = file_val
            .get("remotePath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'remotePath'")?;
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {}", local_path, e))?;
        let content_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
        let clean_path = remote_path.trim_start_matches('/');
        // Check if file exists to determine create vs update
        let action = if gitlab.exists(clean_path).await.unwrap_or(false) {
            "update"
        } else {
            "create"
        };
        actions.push(serde_json::json!({
            "action": action,
            "file_path": clean_path,
            "content": content_b64,
            "encoding": "base64",
        }));
    }

    let commit = gitlab
        .commit_actions_pub(&message, actions)
        .await
        .map_err(|e| format!("Batch upload failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": commit.id,
        "files_count": files.len(),
    }))
}

/// Atomic batch delete of files on GitLab via REST commits API.
#[tauri::command]
pub async fn gitlab_batch_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let actions: Vec<serde_json::Value> = paths
        .iter()
        .map(|p| {
            serde_json::json!({
                "action": "delete",
                "file_path": p.trim_start_matches('/'),
            })
        })
        .collect();

    let commit = gitlab
        .commit_actions_pub(&message, actions)
        .await
        .map_err(|e| format!("Batch delete failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": commit.id,
        "deletions_count": paths.len(),
    }))
}

// ── GitLab: Releases ───────────────────────────────────────────────

/// List all releases of the connected GitLab repository
#[tauri::command]
pub async fn gitlab_list_releases(
    state: State<'_, ProviderState>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let releases = gitlab
        .list_releases()
        .await
        .map_err(|e| format!("Failed to list releases: {}", e))?;

    Ok(releases
        .iter()
        .map(|r| {
            serde_json::json!({
                "tag_name": r.tag_name,
                "name": r.name,
                "description": r.description,
                "created_at": r.created_at,
                "released_at": r.released_at,
                "author": r.author.username,
                "assets_count": r.assets.count,
                "sources": r.assets.sources.iter().map(|s| serde_json::json!({
                    "format": s.format,
                    "url": s.url,
                })).collect::<Vec<_>>(),
            })
        })
        .collect())
}

/// List asset links for a GitLab release
#[tauri::command]
pub async fn gitlab_list_release_assets(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let links = gitlab
        .list_release_links(&tag)
        .await
        .map_err(|e| format!("Failed to list release assets: {}", e))?;

    Ok(links
        .iter()
        .map(|l| {
            serde_json::json!({
                "id": l.id,
                "name": l.name,
                "url": l.url,
                "direct_asset_url": l.direct_asset_url,
                "link_type": l.link_type,
                "external": l.external,
            })
        })
        .collect())
}

/// Create a new GitLab release
#[tauri::command]
pub async fn gitlab_create_release(
    state: State<'_, ProviderState>,
    tag: String,
    name: String,
    description: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let release = gitlab
        .create_release(&tag, &name, &description)
        .await
        .map_err(|e| format!("Failed to create release: {}", e))?;

    Ok(serde_json::json!({
        "tag_name": release.tag_name,
        "name": release.name,
    }))
}

/// Delete a GitLab release
#[tauri::command]
pub async fn gitlab_delete_release(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .delete_release(&tag)
        .await
        .map_err(|e| format!("Failed to delete release: {}", e))
}

/// Upload a file as release asset on GitLab.
/// `link_type`: "other" (default), "package", "image", "runbook".
#[tauri::command]
pub async fn gitlab_upload_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    local_path: String,
    asset_name: String,
    link_type: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    let link = gitlab
        .upload_release_asset(&tag, &local_path, &asset_name, link_type.as_deref())
        .await
        .map_err(|e| format!("Failed to upload release asset: {}", e))?;

    Ok(serde_json::json!({
        "id": link.id,
        "name": link.name,
        "url": link.url,
        "link_type": link.link_type,
    }))
}

/// Delete a release asset link on GitLab
#[tauri::command]
pub async fn gitlab_delete_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    link_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .delete_release_link(&tag, link_id)
        .await
        .map_err(|e| format!("Failed to delete release asset: {}", e))
}

/// Read a file from the connected GitLab repository as UTF-8 text
#[tauri::command]
pub async fn gitlab_read_file(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .read_file_content(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))
}

/// Download a release asset via authenticated backend (works for private repos)
#[tauri::command]
pub async fn gitlab_download_release_asset(
    state: State<'_, ProviderState>,
    url: String,
    local_path: String,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .download_release_asset(&url, &local_path)
        .await
        .map_err(|e| format!("Failed to download asset: {}", e))
}

// ── GitLab: Merge Requests ─────────────────────────────────────────

/// Create a merge request on the connected GitLab repository
#[tauri::command]
pub async fn gitlab_create_merge_request(
    state: State<'_, ProviderState>,
    title: String,
    body: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    gitlab
        .create_merge_request(&title, &body)
        .await
        .map_err(|e| format!("Failed to create merge request: {}", e))
}

// ── GitLab: Web URLs ───────────────────────────────────────────────

/// Get web URL for a file or directory on GitLab
#[tauri::command]
pub async fn gitlab_get_web_url(
    state: State<'_, ProviderState>,
    path: String,
    is_dir: bool,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitLab {
        return Err("This operation is only available for GitLab".to_string());
    }
    let gitlab = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::gitlab::GitLabProvider>()
        .ok_or_else(|| "Failed to access GitLab provider".to_string())?;

    Ok(gitlab.web_url(&path, is_dir))
}

/// Create a pull request on the connected GitHub repository
#[tauri::command]
pub async fn github_create_pr(
    state: State<'_, ProviderState>,
    title: String,
    body: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let pr = github
        .ensure_pull_request(&title, Some(&body), false)
        .await
        .map_err(|e| format!("Failed to create PR: {}", e))?;

    Ok(pr.html_url)
}

/// GitHub Device Flow: Step 1: Request device code
/// Returns user_code and verification_uri for the user to authorize in browser
#[tauri::command]
pub async fn github_device_flow_start() -> Result<serde_json::Value, String> {
    let response = crate::providers::github::auth::request_device_code().await?;

    // Try to open browser automatically
    let _ = open::that(&response.verification_uri);

    Ok(serde_json::json!({
        "user_code": response.user_code,
        "verification_uri": response.verification_uri,
        "device_code": response.device_code,
        "expires_in": response.expires_in,
        "interval": response.interval,
    }))
}

/// GitHub Device Flow: Step 2: Poll for token.
/// SEC-GH-001: Token held backend-side, never returned to frontend.
#[tauri::command]
pub async fn github_device_flow_complete(
    state: State<'_, ProviderState>,
    device_code: String,
    interval: u64,
) -> Result<serde_json::Value, String> {
    let token = crate::providers::github::auth::poll_for_token(&device_code, interval).await?;
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token);
    }
    Ok(serde_json::json!({"success": true}))
}

/// Vault key for a GitHub App PEM, keyed by app_id + installation_id
fn github_pem_vault_key(app_id: &str, installation_id: &str) -> String {
    format!("github_pem_{}_{}", app_id, installation_id)
}

/// Validate PEM contents: non-empty, correct RSA header
fn validate_pem_contents(pem_contents: &str) -> Result<(), String> {
    if pem_contents.trim().is_empty() {
        return Err(
            "PEM file is empty. Please download a new private key from GitHub App settings."
                .to_string(),
        );
    }
    if !pem_contents.contains("-----BEGIN RSA PRIVATE KEY-----")
        && !pem_contents.contains("-----BEGIN PRIVATE KEY-----")
    {
        return Err(
            "Invalid PEM format: file does not contain an RSA private key. \
             Download a fresh .pem from GitHub > Settings > Developer settings > GitHub Apps > Private keys."
                .to_string(),
        );
    }
    Ok(())
}

/// GitHub App Bot Mode: Read .pem from disk, store in vault, and get installation token.
/// SEC-GH-001: The installation token is held backend-side and never crosses IPC.
/// The frontend receives only success status and expiry metadata.
#[tauri::command]
pub async fn github_app_token_from_pem(
    state: State<'_, ProviderState>,
    pem_path: String,
    app_id: String,
    installation_id: String,
) -> Result<serde_json::Value, String> {
    log::info!("GitHub App token: reading PEM from {}", pem_path);

    // Check file exists before reading: provide actionable error
    let path = std::path::Path::new(&pem_path);
    if !path.exists() {
        return Err(format!(
            "PEM file not found: '{}'. The .pem file may have been moved or deleted. Please re-import it.",
            pem_path
        ));
    }

    // Read PEM securely in backend: key never crosses IPC
    let pem_contents = std::fs::read_to_string(&pem_path)
        .map_err(|e| format!("Cannot read .pem file '{}': {}", pem_path, e))?;

    log::info!("GitHub App token: PEM read OK, validating...");

    validate_pem_contents(&pem_contents)?;

    // Validate PEM by attempting JWT generation
    crate::providers::github::auth::validate_pem(&pem_contents, &app_id)?;

    // Store PEM content + App credentials in vault (encrypted AES-256-GCM)
    let vault_key = github_pem_vault_key(&app_id, &installation_id);
    if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
        if let Err(e) = store.store(&vault_key, &pem_contents) {
            log::warn!("Could not store PEM in vault (non-fatal): {}", e);
        } else {
            log::info!("GitHub App PEM stored in vault as '{}'", vault_key);
        }
        // Store App ID + Installation ID so the form can pre-populate on new connections
        let creds = serde_json::json!({
            "app_id": app_id,
            "installation_id": installation_id,
        });
        let _ = store.store("github_app_credentials", &creds.to_string());
    }

    log::info!("GitHub App token: PEM valid, requesting installation token...");

    // Get installation token
    let token_resp = crate::providers::github::auth::get_installation_token(
        &pem_contents,
        &app_id,
        &installation_id,
    )
    .await?;

    // SEC-GH-001: Hold the token backend-side: never return it to the frontend
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token_resp.token);
    }

    Ok(serde_json::json!({
        "success": true,
        "expires_at": token_resp.expires_at,
    }))
}

/// GitHub App Bot Mode: Read PEM from vault (previously imported) and refresh installation token.
/// SEC-GH-001: The installation token is held backend-side and never crosses IPC.
#[tauri::command]
pub async fn github_app_token_from_vault(
    state: State<'_, ProviderState>,
    app_id: String,
    installation_id: String,
) -> Result<serde_json::Value, String> {
    let vault_key = github_pem_vault_key(&app_id, &installation_id);
    log::info!(
        "GitHub App token: reading PEM from vault key '{}'",
        vault_key
    );

    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready: cannot retrieve stored PEM".to_string())?;

    let pem_contents = store
        .get(&vault_key)
        .map_err(|_| "PEM not found in vault. Please re-import the .pem file.".to_string())?;

    validate_pem_contents(&pem_contents)?;
    crate::providers::github::auth::validate_pem(&pem_contents, &app_id)?;

    // Ensure App credentials are saved in vault for form pre-population
    let creds = serde_json::json!({
        "app_id": app_id,
        "installation_id": installation_id,
    });
    let _ = store.store("github_app_credentials", &creds.to_string());

    log::info!("GitHub App token: vault PEM valid, requesting installation token...");

    let token_resp = crate::providers::github::auth::get_installation_token(
        &pem_contents,
        &app_id,
        &installation_id,
    )
    .await?;

    // SEC-GH-001: Hold the token backend-side: never return it to the frontend
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token_resp.token);
    }

    Ok(serde_json::json!({
        "success": true,
        "expires_at": token_resp.expires_at,
    }))
}

/// Get stored GitHub App credentials (App ID + Installation ID) from vault
#[tauri::command]
pub async fn github_get_app_credentials() -> Result<serde_json::Value, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    match store.get("github_app_credentials") {
        Ok(json_str) => serde_json::from_str(&json_str)
            .map_err(|e| format!("Invalid credentials format: {}", e)),
        Err(_) => Ok(serde_json::Value::Null),
    }
}

/// Store GitHub PAT in vault (encrypted)
#[tauri::command]
pub async fn github_store_pat(pat: String) -> Result<(), String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    store
        .store("github_pat", &pat)
        .map_err(|e| format!("Failed to store PAT: {}", e))?;
    log::info!("GitHub PAT stored in vault");
    Ok(())
}

/// Store the held GitHub token into vault as PAT (for Device Flow persistence).
/// SEC-GH-001: Takes from held_github_app_token and stores in vault without IPC exposure.
#[tauri::command]
pub async fn github_store_pat_from_held(state: State<'_, ProviderState>) -> Result<(), String> {
    let token = {
        let held = state.held_github_app_token.lock().await;
        held.clone()
    };
    if let Some(token) = token {
        let store = crate::credential_store::CredentialStore::from_cache()
            .ok_or_else(|| "Vault not ready".to_string())?;
        store
            .store("github_oauth_token", &token)
            .map_err(|e| format!("Failed to store OAuth token: {}", e))?;
        log::info!("GitHub Device Flow token stored in vault as OAuth token");
    }
    Ok(())
}

/// Load stored GitHub OAuth token from vault into held token.
/// Used on app restart when OAuth mode reconnects.
#[tauri::command]
pub async fn github_load_oauth_token(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    let token = store
        .get("github_oauth_token")
        .map_err(|_| "No OAuth token stored in vault".to_string())?;
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(token);
    }
    Ok(serde_json::json!({"success": true}))
}

/// Get stored GitHub PAT from vault.
/// SEC-GH-001: Token held backend-side for connect, returns only success status.
#[tauri::command]
pub async fn github_get_pat(state: State<'_, ProviderState>) -> Result<serde_json::Value, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    let pat = store
        .get("github_pat")
        .map_err(|_| "No PAT stored in vault".to_string())?;
    {
        let mut held = state.held_github_app_token.lock().await;
        *held = Some(pat);
    }
    Ok(serde_json::json!({"success": true}))
}

/// Check if a GitHub App PEM is stored in the vault
#[tauri::command]
pub async fn github_has_vault_pem(app_id: String, installation_id: String) -> Result<bool, String> {
    let vault_key = github_pem_vault_key(&app_id, &installation_id);
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not ready".to_string())?;
    Ok(store.get(&vault_key).is_ok())
}

/// List all releases for the connected GitHub repository
#[tauri::command]
pub async fn github_list_releases(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let releases = github
        .list_all_releases()
        .await
        .map_err(|e| format!("Failed to list releases: {}", e))?;

    let result: Vec<serde_json::Value> = releases
        .iter()
        .map(|r| {
            serde_json::json!({
                "tag": &r.name,
                "path": &r.path,
                "published_at": &r.modified,
                "draft": r.metadata.get("draft").map(|v| v == "true").unwrap_or(false),
                "prerelease": r.metadata.get("prerelease").map(|v| v == "true").unwrap_or(false),
                "body": r.metadata.get("body").cloned().unwrap_or_default(),
                "release_id": r.metadata.get("release_id").cloned().unwrap_or_default(),
            })
        })
        .collect();

    Ok(serde_json::json!({ "releases": result, "count": result.len() }))
}

/// List assets for a specific release tag
#[tauri::command]
pub async fn github_list_release_assets(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let assets = github
        .list_assets_for_release(&tag)
        .await
        .map_err(|e| format!("Failed to list release assets: {}", e))?;

    let result: Vec<serde_json::Value> = assets
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": &a.name,
                "size": a.size,
                "content_type": a.mime_type,
                "download_count": a.metadata.get("download_count").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0),
                "browser_download_url": a.metadata.get("browser_download_url").cloned().unwrap_or_default(),
                "updated_at": &a.modified,
            })
        })
        .collect();

    Ok(serde_json::json!({ "assets": result, "count": result.len(), "tag": tag }))
}

/// Create a new release on the connected GitHub repository
#[tauri::command]
pub async fn github_create_release(
    state: State<'_, ProviderState>,
    tag: String,
    name: String,
    body: String,
    draft: bool,
    prerelease: bool,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let release = github
        .create_new_release(&tag, &name, &body, draft, prerelease)
        .await
        .map_err(|e| format!("Failed to create release: {}", e))?;

    Ok(serde_json::json!({
        "id": release.id,
        "tag_name": release.tag_name,
        "name": release.name,
        "draft": release.draft,
        "prerelease": release.prerelease,
        "created_at": release.created_at,
    }))
}

/// Read a text file from the connected GitHub repository (always from repo root).
#[tauri::command]
pub async fn github_read_file(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<String, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    // Prefix with "/" to force resolve_path to treat as absolute (from root),
    // regardless of the user's current navigation directory.
    let root_path = if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };

    let bytes = provider
        .download_to_bytes(&root_path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    String::from_utf8(bytes).map_err(|e| format!("File is not valid UTF-8: {}", e))
}

// ── GitHub Pages ──────────────────────────────────────────────────

/// Get GitHub Pages site info (returns null if not enabled)
#[tauri::command]
pub async fn github_get_pages(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    match github.get_pages_info().await {
        Ok(Some(site)) => Ok(serde_json::to_value(site).unwrap_or_default()),
        Ok(None) => Ok(serde_json::Value::Null),
        Err(e) => Err(format!("Failed to get Pages info: {}", e)),
    }
}

/// List GitHub Pages builds
#[tauri::command]
pub async fn github_list_pages_builds(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let builds = github
        .list_pages_builds()
        .await
        .map_err(|e| format!("Failed to list Pages builds: {}", e))?;
    Ok(serde_json::to_value(builds).unwrap_or_default())
}

/// Trigger a GitHub Pages rebuild (legacy build_type only)
#[tauri::command]
pub async fn github_trigger_pages_build(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let status = github
        .trigger_pages_build()
        .await
        .map_err(|e| format!("Failed to trigger Pages build: {}", e))?;
    Ok(serde_json::to_value(status).unwrap_or_default())
}

/// Update GitHub Pages configuration (CNAME, HTTPS, source)
#[tauri::command]
pub async fn github_update_pages(
    state: State<'_, ProviderState>,
    cname: Option<String>,
    https_enforced: Option<bool>,
    source_branch: Option<String>,
    source_path: Option<String>,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .update_pages_config(
            cname.as_deref(),
            https_enforced,
            source_branch.as_deref(),
            source_path.as_deref(),
        )
        .await
        .map_err(|e| format!("Failed to update Pages config: {}", e))
}

/// Check DNS health for GitHub Pages custom domain
#[tauri::command]
pub async fn github_pages_health(
    state: State<'_, ProviderState>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let health = github
        .pages_health_check()
        .await
        .map_err(|e| format!("Failed to check Pages DNS health: {}", e))?;
    Ok(serde_json::to_value(health).unwrap_or_default())
}

/// Upload a file as a release asset
#[tauri::command]
pub async fn github_upload_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    local_path: String,
    asset_name: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .upload_asset(&tag, &local_path, &asset_name)
        .await
        .map_err(|e| format!("Failed to upload release asset: {}", e))?;

    Ok(serde_json::json!({
        "tag": tag,
        "asset": asset_name,
        "status": "uploaded",
    }))
}

/// Delete an entire release by tag
#[tauri::command]
pub async fn github_delete_release(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .delete_release_by_tag(&tag)
        .await
        .map_err(|e| format!("Failed to delete release: {}", e))?;

    Ok(serde_json::json!({ "tag": tag, "status": "deleted" }))
}

/// Delete a specific asset from a release
#[tauri::command]
pub async fn github_delete_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    asset_name: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .delete_asset(&tag, &asset_name)
        .await
        .map_err(|e| format!("Failed to delete release asset: {}", e))?;

    Ok(serde_json::json!({ "tag": tag, "asset": asset_name, "status": "deleted" }))
}

/// Download a release asset to a local file
#[tauri::command]
pub async fn github_download_release_asset(
    state: State<'_, ProviderState>,
    tag: String,
    asset_name: String,
    local_path: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .download_asset(&tag, &asset_name, &local_path)
        .await
        .map_err(|e| format!("Failed to download release asset: {}", e))?;

    Ok(
        serde_json::json!({ "tag": tag, "asset": asset_name, "path": local_path, "status": "downloaded" }),
    )
}

/// Get detailed release information by tag
#[tauri::command]
pub async fn github_get_release(
    state: State<'_, ProviderState>,
    tag: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let release = github
        .get_release(&tag)
        .await
        .map_err(|e| format!("Failed to get release info: {}", e))?;

    let assets: Vec<serde_json::Value> = release
        .assets
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "size": a.size,
                "download_count": a.download_count,
                "content_type": a.content_type,
                "browser_download_url": a.browser_download_url,
                "created_at": a.created_at,
                "updated_at": a.updated_at,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "id": release.id,
        "tag_name": release.tag_name,
        "name": release.name,
        "body": release.body,
        "draft": release.draft,
        "prerelease": release.prerelease,
        "created_at": release.created_at,
        "published_at": release.published_at,
        "assets": assets,
        "asset_count": assets.len(),
    }))
}

/// Atomic multi-file commit via GraphQL createCommitOnBranch
#[tauri::command]
pub async fn github_batch_commit(
    state: State<'_, ProviderState>,
    branch: String,
    message: String,
    additions: Vec<serde_json::Value>,
    deletions: Vec<String>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;

    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }

    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    // Parse additions: [{path: String, content: String}]
    let parsed_additions: Vec<(String, String)> = additions
        .iter()
        .map(|v| {
            let path = v
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| "Each addition must have a 'path' string field".to_string())?;
            let content = v
                .get("content")
                .and_then(|c| c.as_str())
                .ok_or_else(|| "Each addition must have a 'content' string field".to_string())?;
            Ok((path.to_string(), content.to_string()))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let oid = github
        .batch_commit(&branch, &message, &parsed_additions, &deletions)
        .await
        .map_err(|e| format!("Batch commit failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": oid,
        "branch": branch,
        "additions_count": parsed_additions.len(),
        "deletions_count": deletions.len(),
    }))
}

/// Atomic batch upload of binary files to GitHub via GraphQL createCommitOnBranch.
/// Unlike github_batch_commit (text-only), this reads files from disk as binary.
#[tauri::command]
pub async fn github_batch_upload(
    state: State<'_, ProviderState>,
    files: Vec<serde_json::Value>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let mut additions: Vec<(String, Vec<u8>)> = Vec::with_capacity(files.len());
    for file_val in &files {
        let local_path = file_val
            .get("localPath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'localPath'")?;
        let remote_path = file_val
            .get("remotePath")
            .and_then(|v| v.as_str())
            .ok_or("Each file must have a 'remotePath'")?;
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| format!("Failed to read {}: {}", local_path, e))?;
        additions.push((remote_path.trim_start_matches('/').to_string(), data));
    }

    let oid = github
        .batch_upload(&message, &additions, &[])
        .await
        .map_err(|e| format!("Batch upload failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": oid,
        "files_count": additions.len(),
    }))
}

/// Atomic batch delete of files on GitHub via GraphQL createCommitOnBranch.
#[tauri::command]
pub async fn github_batch_delete(
    state: State<'_, ProviderState>,
    paths: Vec<String>,
    message: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let deletions: Vec<String> = paths
        .iter()
        .map(|p| p.trim_start_matches('/').to_string())
        .collect();

    let oid = github
        .batch_upload(&message, &[], &deletions)
        .await
        .map_err(|e| format!("Batch delete failed: {}", e))?;

    Ok(serde_json::json!({
        "commit_sha": oid,
        "files_count": deletions.len(),
    }))
}

// ── GitHub Local Sync Detection ──────────────────────────────────

/// Check if a git remote line matches a specific owner/repo exactly.
/// Prevents partial matches like `repo-old` matching `repo`.
fn remote_matches_repo(line: &str, owner: &str, repo: &str) -> bool {
    let lower = line.to_lowercase();
    let ssh = format!("github.com:{}/{}", owner, repo).to_lowercase();
    let https = format!("github.com/{}/{}", owner, repo).to_lowercase();

    for pattern in [&ssh, &https] {
        if let Some(idx) = lower.find(pattern) {
            let after = idx + pattern.len();
            // Must be followed by `.git`, whitespace, or end of string
            let rest = &lower[after..];
            if rest.is_empty() || rest.starts_with(".git") || rest.starts_with(char::is_whitespace)
            {
                return true;
            }
        }
    }
    false
}

/// SEC-GH-002/003: Validate and canonicalize a local path for git operations.
/// Returns the canonical path only if it is a real directory containing a `.git` folder.
fn validate_local_git_path(local_path: &str) -> Result<std::path::PathBuf, String> {
    let canonical = std::fs::canonicalize(local_path)
        .map_err(|e| format!("Invalid local path '{}': {}", local_path, e))?;
    let meta = std::fs::metadata(&canonical)
        .map_err(|e| format!("Cannot access '{}': {}", canonical.display(), e))?;
    if !meta.is_dir() {
        return Err(format!("'{}' is not a directory", canonical.display()));
    }
    if !canonical.join(".git").exists() {
        return Err(format!("'{}' is not a git repository", canonical.display()));
    }
    Ok(canonical)
}

/// Helper: run an async git command with non-interactive environment guards.
async fn git_command(args: &[&str], dir: &std::path::Path) -> Result<std::process::Output, String> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .output()
        .await
        .map_err(|e| format!("Failed to run git {}: {}", args.first().unwrap_or(&""), e))
}

/// Check if the local working directory has unpushed commits for the connected GitHub repo.
/// SEC-GH-002/003: Path is canonicalized, validated as git repo, and all commands are async.
#[tauri::command]
pub async fn github_check_local_sync(
    state: State<'_, ProviderState>,
    local_path: String,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Ok(serde_json::json!({"is_local_repo": false}));
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let owner = github.owner().to_string();
    let repo = github.repo().to_string();

    // Validate and canonicalize the local path
    let canonical = match validate_local_git_path(&local_path) {
        Ok(p) => p,
        Err(_) => return Ok(serde_json::json!({"is_local_repo": false})),
    };

    // Check if local repo's remote matches this GitHub repo
    let remote_out = git_command(&["remote", "-v"], &canonical).await?;
    if !remote_out.status.success() {
        return Ok(serde_json::json!({"is_local_repo": false}));
    }
    let remote_output = String::from_utf8_lossy(&remote_out.stdout).to_string();

    let matches = remote_output
        .lines()
        .any(|line| remote_matches_repo(line, &owner, &repo));

    if !matches {
        return Ok(serde_json::json!({"is_local_repo": true, "repo_matches": false}));
    }

    // Get local HEAD
    let head_out = git_command(&["rev-parse", "HEAD"], &canonical).await?;
    let local_head = if head_out.status.success() {
        String::from_utf8_lossy(&head_out.stdout).trim().to_string()
    } else {
        return Ok(
            serde_json::json!({"is_local_repo": true, "repo_matches": true, "error": "Cannot read local HEAD"}),
        );
    };

    // Get remote HEAD via GitHub API
    let branch = github.active_branch().to_string();
    let remote_head = {
        match github
            .client_mut()
            .get_json::<serde_json::Value>(&format!(
                "/repos/{}/{}/git/ref/heads/{}",
                owner,
                repo,
                urlencoding::encode(&branch)
            ))
            .await
        {
            Ok(val) => {
                match val
                    .get("object")
                    .and_then(|o| o.get("sha"))
                    .and_then(|s| s.as_str())
                {
                    Some(sha) => sha.to_string(),
                    None => {
                        return Ok(serde_json::json!({
                            "is_local_repo": true, "repo_matches": true,
                            "error": "Cannot parse remote HEAD SHA"
                        }))
                    }
                }
            }
            Err(e) => {
                return Ok(serde_json::json!({
                    "is_local_repo": true, "repo_matches": true,
                    "error": format!("Cannot fetch remote HEAD: {}", e)
                }))
            }
        }
    };

    // Count unpushed commits
    let count_out = git_command(
        &["rev-list", &format!("{}..HEAD", remote_head), "--count"],
        &canonical,
    )
    .await?;
    let unpushed_count = if count_out.status.success() {
        String::from_utf8_lossy(&count_out.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or(0)
    } else {
        0
    };

    Ok(serde_json::json!({
        "is_local_repo": true,
        "repo_matches": true,
        "local_head": local_head,
        "remote_head": remote_head,
        "unpushed_count": unpushed_count,
        "branch": branch,
    }))
}

/// Push local commits to the remote GitHub repository.
/// SEC-GH-002: Path validated and verified to match the connected repo before executing push.
#[tauri::command]
pub async fn github_push_local(
    state: State<'_, ProviderState>,
    local_path: String,
) -> Result<serde_json::Value, String> {
    // Validate the local path
    let canonical = validate_local_git_path(&local_path)?;

    // Verify the repo remote matches the connected GitHub repo
    {
        let mut provider_guard = state.provider.lock().await;
        let provider = provider_guard
            .as_mut()
            .ok_or_else(|| "Not connected to any provider".to_string())?;
        if provider.provider_type() == ProviderType::GitHub {
            let github = provider
                .as_any_mut()
                .downcast_mut::<crate::providers::github::GitHubProvider>()
                .ok_or_else(|| "Failed to access GitHub provider".to_string())?;
            let owner = github.owner().to_string();
            let repo = github.repo().to_string();

            let remote_out = git_command(&["remote", "-v"], &canonical).await?;
            let remote_output = String::from_utf8_lossy(&remote_out.stdout).to_string();
            let matches = remote_output
                .lines()
                .any(|line| remote_matches_repo(line, &owner, &repo));
            if !matches {
                return Err(format!(
                    "Local repo remote does not match connected GitHub repo {}/{}",
                    owner, repo
                ));
            }
        }
    }

    let output = git_command(&["push"], &canonical).await?;

    if output.status.success() {
        Ok(serde_json::json!({
            "status": "ok",
            "message": String::from_utf8_lossy(&output.stdout).trim().to_string(),
        }))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git push failed: {}", stderr))
    }
}

// ── GitHub Actions ────────────────────────────────────────────────

/// List recent GitHub Actions workflow runs
#[tauri::command]
pub async fn github_list_actions_runs(
    state: State<'_, ProviderState>,
    branch: Option<String>,
    per_page: Option<u8>,
) -> Result<serde_json::Value, String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    let runs = github
        .list_actions_runs(branch.as_deref(), per_page.unwrap_or(20))
        .await
        .map_err(|e| format!("Failed to list Actions runs: {}", e))?;
    Ok(serde_json::to_value(runs).unwrap_or_default())
}

/// Re-run a GitHub Actions workflow
#[tauri::command]
pub async fn github_rerun_workflow(
    state: State<'_, ProviderState>,
    run_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .rerun_actions_workflow(run_id)
        .await
        .map_err(|e| format!("Failed to re-run workflow: {}", e))
}

/// Re-run only failed jobs in a GitHub Actions workflow
#[tauri::command]
pub async fn github_rerun_failed_jobs(
    state: State<'_, ProviderState>,
    run_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .rerun_failed_jobs(run_id)
        .await
        .map_err(|e| format!("Failed to re-run failed jobs: {}", e))
}

/// Cancel an in-progress GitHub Actions workflow run
#[tauri::command]
pub async fn github_cancel_workflow(
    state: State<'_, ProviderState>,
    run_id: u64,
) -> Result<(), String> {
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or_else(|| "Not connected to any provider".to_string())?;
    if provider.provider_type() != ProviderType::GitHub {
        return Err("This operation is only available for GitHub".to_string());
    }
    let github = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::github::GitHubProvider>()
        .ok_or_else(|| "Failed to access GitHub provider".to_string())?;

    github
        .cancel_actions_run(run_id)
        .await
        .map_err(|e| format!("Failed to cancel workflow: {}", e))
}

// ============ Filen Encrypted Notes ============

/// List all Filen encrypted notes
#[tauri::command]
pub async fn filen_notes_list(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::filen::notes::FilenNote>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.list_notes().await.map_err(|e| e.to_string())
}

/// Create a new Filen encrypted note
#[tauri::command]
pub async fn filen_notes_create(
    state: State<'_, ProviderState>,
    title: String,
    content: String,
    note_type: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    let nt = parse_note_type_str(&note_type);
    filen
        .create_note(&title, &content, &nt)
        .await
        .map_err(|e| e.to_string())
}

/// Get decrypted content of a Filen note
#[tauri::command]
pub async fn filen_notes_get_content(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<crate::providers::filen::notes::FilenNoteContent, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .get_note_content(&uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Edit content of a Filen encrypted note
#[tauri::command]
pub async fn filen_notes_edit_content(
    state: State<'_, ProviderState>,
    uuid: String,
    content: String,
    note_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    let nt = parse_note_type_str(&note_type);
    filen
        .edit_note_content(&uuid, &content, &nt)
        .await
        .map_err(|e| e.to_string())
}

/// Edit title of a Filen encrypted note
#[tauri::command]
pub async fn filen_notes_edit_title(
    state: State<'_, ProviderState>,
    uuid: String,
    title: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .edit_note_title(&uuid, &title)
        .await
        .map_err(|e| e.to_string())
}

/// Change the type of a Filen note (text, md, code, rich, checklist)
#[tauri::command]
pub async fn filen_notes_change_type(
    state: State<'_, ProviderState>,
    uuid: String,
    note_type: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    let nt = parse_note_type_str(&note_type);
    filen
        .change_note_type(&uuid, &nt)
        .await
        .map_err(|e| e.to_string())
}

/// Move a Filen note to trash
#[tauri::command]
pub async fn filen_notes_trash(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.trash_note(&uuid).await.map_err(|e| e.to_string())
}

/// Archive a Filen note
#[tauri::command]
pub async fn filen_notes_archive(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.archive_note(&uuid).await.map_err(|e| e.to_string())
}

/// Restore a Filen note from trash or archive
#[tauri::command]
pub async fn filen_notes_restore(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.restore_note(&uuid).await.map_err(|e| e.to_string())
}

/// Permanently delete a Filen note
#[tauri::command]
pub async fn filen_notes_delete(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.delete_note(&uuid).await.map_err(|e| e.to_string())
}

/// Returns the authVersion observed during Filen connect (/v3/auth/info).
#[tauri::command]
pub async fn filen_get_auth_version(
    state: State<'_, ProviderState>,
) -> Result<Option<u32>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    Ok(filen.auth_version())
}

/// Toggle favorite on a Filen note
#[tauri::command]
pub async fn filen_notes_toggle_favorite(
    state: State<'_, ProviderState>,
    uuid: String,
    favorite: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .toggle_note_favorite(&uuid, favorite)
        .await
        .map_err(|e| e.to_string())
}

/// Toggle pinned on a Filen note
#[tauri::command]
pub async fn filen_notes_toggle_pinned(
    state: State<'_, ProviderState>,
    uuid: String,
    pinned: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .toggle_note_pinned(&uuid, pinned)
        .await
        .map_err(|e| e.to_string())
}

/// Get version history for a Filen note
#[tauri::command]
pub async fn filen_notes_history(
    state: State<'_, ProviderState>,
    uuid: String,
) -> Result<Vec<crate::providers::filen::notes::FilenNoteHistoryEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .get_note_history(&uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Restore a specific history version of a Filen note
#[tauri::command]
pub async fn filen_notes_history_restore(
    state: State<'_, ProviderState>,
    uuid: String,
    history_id: u64,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .restore_note_history(&uuid, history_id)
        .await
        .map_err(|e| e.to_string())
}

/// List all Filen note tags
#[tauri::command]
pub async fn filen_notes_tags_list(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::filen::notes::FilenNoteTag>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen.list_note_tags().await.map_err(|e| e.to_string())
}

/// Create a new Filen note tag
#[tauri::command]
pub async fn filen_notes_tags_create(
    state: State<'_, ProviderState>,
    name: String,
) -> Result<String, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .create_note_tag(&name)
        .await
        .map_err(|e| e.to_string())
}

/// Rename a Filen note tag
#[tauri::command]
pub async fn filen_notes_tags_rename(
    state: State<'_, ProviderState>,
    tag_uuid: String,
    name: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .rename_note_tag(&tag_uuid, &name)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a Filen note tag
#[tauri::command]
pub async fn filen_notes_tags_delete(
    state: State<'_, ProviderState>,
    tag_uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .delete_note_tag(&tag_uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Assign a tag to a Filen note
#[tauri::command]
pub async fn filen_notes_tag_note(
    state: State<'_, ProviderState>,
    note_uuid: String,
    tag_uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .tag_note(&note_uuid, &tag_uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Remove a tag from a Filen note
#[tauri::command]
pub async fn filen_notes_untag_note(
    state: State<'_, ProviderState>,
    note_uuid: String,
    tag_uuid: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Filen {
        return Err("Only available for Filen".into());
    }
    let filen = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::filen::FilenProvider>()
        .ok_or("Failed to access Filen provider")?;
    filen
        .untag_note(&note_uuid, &tag_uuid)
        .await
        .map_err(|e| e.to_string())
}

/// Parse note type string to enum (delegates to notes module).
fn parse_note_type_str(s: &str) -> crate::providers::filen::notes::NoteType {
    crate::providers::filen::notes::parse_note_type(s)
}

// ============ S3 Enterprise Commands ============

/// Change storage class of an S3 object (via server-side copy)
#[tauri::command]
pub async fn s3_change_storage_class(
    state: State<'_, ProviderState>,
    path: String,
    storage_class: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.change_storage_class(&path, &storage_class)
        .await
        .map_err(|e| e.to_string())
}

/// Initiate Glacier/Deep Archive restore for an S3 object
#[tauri::command]
pub async fn s3_glacier_restore(
    state: State<'_, ProviderState>,
    path: String,
    days: u32,
    tier: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.glacier_restore(&path, days, &tier)
        .await
        .map_err(|e| e.to_string())
}

/// Get object tags for an S3 object
#[tauri::command]
pub async fn s3_get_object_tags(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.get_object_tags(&path).await.map_err(|e| e.to_string())
}

/// Set object tags on an S3 object (max 10 tags per AWS)
#[tauri::command]
pub async fn s3_set_object_tags(
    state: State<'_, ProviderState>,
    path: String,
    tags: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.set_object_tags(&path, &tags)
        .await
        .map_err(|e| e.to_string())
}

/// Delete all tags from an S3 object
#[tauri::command]
pub async fn s3_delete_object_tags(
    state: State<'_, ProviderState>,
    path: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::S3 {
        return Err("Only available for S3".into());
    }
    let s3 = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::s3::S3Provider>()
        .ok_or("Failed to access S3 provider")?;
    s3.delete_object_tags(&path)
        .await
        .map_err(|e| e.to_string())
}

// ============ Azure Enterprise Commands ============

/// Set the access tier of an Azure blob (Hot, Cool, Cold, Archive)
#[tauri::command]
pub async fn azure_set_blob_tier(
    state: State<'_, ProviderState>,
    path: String,
    tier: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Azure {
        return Err("Only available for Azure".into());
    }
    let azure = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::azure::AzureProvider>()
        .ok_or("Failed to access Azure provider")?;
    azure
        .set_blob_tier(&path, &tier)
        .await
        .map_err(|e| e.to_string())
}

/// List soft-deleted blobs in the Azure container
#[tauri::command]
pub async fn azure_list_deleted_blobs(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Azure {
        return Err("Only available for Azure".into());
    }
    let azure = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::azure::AzureProvider>()
        .ok_or("Failed to access Azure provider")?;
    azure.list_deleted_blobs().await.map_err(|e| e.to_string())
}

/// Undelete a soft-deleted Azure blob
#[tauri::command]
pub async fn azure_undelete_blob(
    state: State<'_, ProviderState>,
    path: Option<String>,
    blob_name: Option<String>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Azure {
        return Err("Only available for Azure".into());
    }
    let azure = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::azure::AzureProvider>()
        .ok_or("Failed to access Azure provider")?;
    let resolved_path = path
        .or(blob_name)
        .ok_or_else(|| "Missing path or blobName".to_string())?;
    azure
        .undelete_blob(&resolved_path)
        .await
        .map_err(|e| e.to_string())
}

// ============ pCloud Trash Commands ============

/// List items in the Internxt trash
#[tauri::command]
pub async fn internxt_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::Internxt {
        return Err("Only available for Internxt".into());
    }
    let internxt = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::internxt::InternxtProvider>()
        .ok_or("Failed to access Internxt provider")?;
    internxt.list_trash().await.map_err(|e| e.to_string())
}

/// List items in the pCloud trash
#[tauri::command]
pub async fn pcloud_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    pcloud.list_trash().await.map_err(|e| e.to_string())
}

/// Restore item from pCloud trash
#[tauri::command]
pub async fn pcloud_restore_from_trash(
    state: State<'_, ProviderState>,
    id: String,
    is_folder: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    pcloud
        .restore_from_trash(&id, is_folder)
        .await
        .map_err(|e| e.to_string())
}

/// Empty pCloud trash
#[tauri::command]
pub async fn pcloud_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    pcloud.empty_trash().await.map_err(|e| e.to_string())
}

/// Permanently delete a single item from pCloud trash
#[tauri::command]
pub async fn pcloud_permanently_delete_trash(
    state: State<'_, ProviderState>,
    id: String,
    is_folder: bool,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::PCloud {
        return Err("Only available for pCloud".into());
    }
    let pcloud = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::pcloud::PCloudProvider>()
        .ok_or("Failed to access pCloud provider")?;
    pcloud
        .permanent_delete_from_trash(&id, is_folder)
        .await
        .map_err(|e| e.to_string())
}

// ============ kDrive Trash Commands ============

/// List items in the kDrive trash
#[tauri::command]
pub async fn kdrive_list_trash(
    state: State<'_, ProviderState>,
) -> Result<Vec<crate::providers::RemoteEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    kdrive.list_trash().await.map_err(|e| e.to_string())
}

/// Restore item from kDrive trash
#[tauri::command]
pub async fn kdrive_restore_from_trash(
    state: State<'_, ProviderState>,
    file_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    kdrive
        .restore_from_trash(&file_id)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete item from kDrive trash
#[tauri::command]
pub async fn kdrive_permanently_delete_trash(
    state: State<'_, ProviderState>,
    file_id: String,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    kdrive
        .permanently_delete_trash(&file_id)
        .await
        .map_err(|e| e.to_string())
}

/// Empty the entire kDrive trash
#[tauri::command]
pub async fn kdrive_empty_trash(state: State<'_, ProviderState>) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    if provider.provider_type() != ProviderType::KDrive {
        return Err("Only available for kDrive".into());
    }
    let kdrive = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::kdrive::KDriveProvider>()
        .ok_or("Failed to access kDrive provider")?;
    kdrive.empty_trash().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{remote_matches_repo, ProviderConnectionParams};

    fn s3_params(path_style: Option<bool>) -> ProviderConnectionParams {
        ProviderConnectionParams {
            protocol: "s3".to_string(),
            server: "http://localhost".to_string(),
            port: Some(3900),
            username: "access".to_string(),
            password: "secret".to_string(),
            initial_path: None,
            bucket: Some("garage-bucket".to_string()),
            region: Some("garage".to_string()),
            endpoint: None,
            path_style,
            anonymous: None,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            save_session: None,
            mega_mode: None,
            session_expires_at: None,
            logout_on_disconnect: None,
            private_key_path: None,
            key_passphrase: None,
            timeout: None,
            tls_mode: None,
            verify_cert: None,
            two_factor_code: None,
            github_auth_mode: None,
            github_app_id: None,
            github_installation_id: None,
            github_pem_path: None,
            github_token_expires_at: None,
            github_branch: None,
        }
    }

    #[test]
    fn test_s3_provider_params_preserve_absent_path_style() {
        let config = s3_params(None).to_provider_config().unwrap();
        assert!(!config.extra.contains_key("path_style"));
    }

    #[test]
    fn test_s3_provider_params_preserve_explicit_virtual_host_style() {
        let config = s3_params(Some(false)).to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("path_style").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn test_backblaze_provider_params_forward_bucket() {
        let mut params = s3_params(None);
        params.protocol = "backblaze".to_string();
        let config = params.to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("bucket").map(String::as_str),
            Some("garage-bucket")
        );
    }

    #[test]
    fn test_webdav_provider_params_forward_anonymous() {
        let mut params = s3_params(None);
        params.protocol = "webdav".to_string();
        params.bucket = None;
        params.anonymous = Some(true);
        let config = params.to_provider_config().unwrap();
        assert_eq!(
            config.extra.get("anonymous").map(String::as_str),
            Some("true")
        );
    }

    // SEC-GH-002: Exact repo matching with boundary detection
    #[test]
    fn test_remote_matches_exact_ssh() {
        assert!(remote_matches_repo(
            "origin\tgit@github.com:axpdev-lab/aeroftp.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_matches_exact_https() {
        assert!(remote_matches_repo(
            "origin\thttps://github.com/axpdev-lab/aeroftp.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_matches_without_git_suffix() {
        assert!(remote_matches_repo(
            "origin\thttps://github.com/axpdev-lab/aeroftp (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_rejects_prefix_collision() {
        // "aeroftp" should NOT match "aeroftp-old"
        assert!(!remote_matches_repo(
            "origin\tgit@github.com:axpdev-lab/aeroftp-old.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_rejects_different_owner() {
        assert!(!remote_matches_repo(
            "origin\tgit@github.com:other-org/aeroftp.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_case_insensitive() {
        assert!(remote_matches_repo(
            "origin\tgit@GitHub.com:AxpDev-Lab/AeroFTP.git (fetch)",
            "axpdev-lab",
            "aeroftp"
        ));
    }

    #[test]
    fn test_remote_rejects_empty_line() {
        assert!(!remote_matches_repo("", "axpdev-lab", "aeroftp"));
    }

    #[test]
    fn test_remote_matches_end_of_line_boundary() {
        // URL ends at whitespace (fetch/push marker)
        assert!(remote_matches_repo(
            "origin\thttps://github.com/axpdev-lab/aeroftp (push)",
            "axpdev-lab",
            "aeroftp"
        ));
    }
}
