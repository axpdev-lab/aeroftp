//! Tauri implementations of AI Core traits
//!
//! Wraps existing AppHandle, ProviderState, AppState for backward compatibility.
//! The GUI continues to work exactly as before via these wrappers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::credential_provider::{
    CredentialProvider, ProviderExtraOptions, ServerCredentials, ServerProfile,
};
use super::event_sink::{EventSink, ToolProgress};
use super::remote_backend::{RemoteBackend, StorageQuota};
use crate::ai_stream::StreamChunk;
use crate::providers::{RemoteEntry, StorageProvider};
use crate::AppState;

// ─── TauriEventSink ────────────────────────────────────────────────────

/// PERF-01: coalesce text deltas into larger chunks before crossing the webview
/// IPC bridge. Without this every provider token produced one `app.emit`
/// (JSON-serialize + IPC round-trip), which in turn drove a full message-list
/// re-render and a smooth scroll per token on the frontend.
const COALESCE_FLUSH_BYTES: usize = 256;
const COALESCE_FLUSH_MILLIS: u128 = 40;

struct CoalesceState {
    buf: String,
    last_flush: std::time::Instant,
}

/// Wraps Tauri AppHandle to emit events to the frontend.
pub struct TauriEventSink {
    pub(crate) app: AppHandle,
    coalesce: std::sync::Mutex<CoalesceState>,
}

/// Build a content-only `StreamChunk` (no thinking/tool/usage/done flags), used
/// to emit a coalesced batch of text deltas.
fn content_only_chunk(content: String) -> StreamChunk {
    StreamChunk {
        content,
        done: false,
        tool_calls: None,
        input_tokens: None,
        output_tokens: None,
        thinking: None,
        thinking_done: None,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    }
}

impl TauriEventSink {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            coalesce: std::sync::Mutex::new(CoalesceState {
                buf: String::new(),
                last_flush: std::time::Instant::now(),
            }),
        }
    }
}

impl EventSink for TauriEventSink {
    fn emit_stream_chunk(&self, stream_id: &str, chunk: &StreamChunk) {
        let event_name = format!("ai-stream-{}", stream_id);

        // A "pure content" chunk carries only text. Thinking, tool calls, usage
        // and the terminal `done` chunk are emitted unbuffered so semantics and
        // ordering are unchanged; buffered text is flushed before each of them.
        let is_pure_content = !chunk.content.is_empty()
            && !chunk.done
            && chunk.tool_calls.is_none()
            && chunk.input_tokens.is_none()
            && chunk.output_tokens.is_none()
            && chunk.thinking.is_none()
            && chunk.thinking_done.is_none()
            && chunk.cache_creation_input_tokens.is_none()
            && chunk.cache_read_input_tokens.is_none();

        if is_pure_content {
            let to_emit = {
                let mut state = match self.coalesce.lock() {
                    Ok(s) => s,
                    Err(p) => p.into_inner(),
                };
                state.buf.push_str(&chunk.content);
                if state.buf.len() >= COALESCE_FLUSH_BYTES
                    || state.last_flush.elapsed().as_millis() >= COALESCE_FLUSH_MILLIS
                {
                    state.last_flush = std::time::Instant::now();
                    Some(std::mem::take(&mut state.buf))
                } else {
                    None
                }
            };
            if let Some(content) = to_emit {
                let _ = self.app.emit(&event_name, &content_only_chunk(content));
            }
            return;
        }

        // Non-content chunk: flush any pending text first to preserve order.
        let pending = {
            let mut state = match self.coalesce.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            state.last_flush = std::time::Instant::now();
            if state.buf.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut state.buf))
            }
        };
        if let Some(content) = pending {
            let _ = self.app.emit(&event_name, &content_only_chunk(content));
        }
        let _ = self.app.emit(&event_name, chunk);
    }

    fn emit_tool_progress(&self, progress: &ToolProgress) {
        let _ = self.app.emit(
            "ai-tool-progress",
            json!({
                "tool": progress.tool,
                "current": progress.current,
                "total": progress.total,
                "item": progress.item,
            }),
        );
    }

    fn emit_app_control(&self, event_name: &str, payload: &Value) {
        const ALLOWED_EVENTS: &[&str] = &["ai-set-theme", "ai-sync-control", "ai-tool-progress"];
        if ALLOWED_EVENTS.contains(&event_name) {
            let _ = self.app.emit(event_name, payload.clone());
        }
    }
}

// ─── VaultCredentialProvider ───────────────────────────────────────────

/// Reads credentials from the encrypted vault.db via CredentialStore.
pub struct VaultCredentialProvider;

impl CredentialProvider for VaultCredentialProvider {
    fn list_servers(&self) -> Result<Vec<ServerProfile>, String> {
        let store = crate::credential_store::CredentialStore::from_cache()
            .ok_or_else(|| "Credential vault not open. Unlock the vault first.".to_string())?;
        let json_str = store
            .get("config_server_profiles")
            .map_err(|_| "No saved servers found in vault.".to_string())?;
        let profiles: Vec<Value> = serde_json::from_str(&json_str)
            .map_err(|e| format!("Failed to parse server profiles: {}", e))?;

        Ok(profiles
            .iter()
            .filter_map(|p| {
                Some(ServerProfile {
                    id: p.get("id")?.as_str()?.to_string(),
                    name: p
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    host: p
                        .get("host")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    port: p.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16,
                    username: p
                        .get("username")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    protocol: p
                        .get("protocol")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ftp")
                        .to_string(),
                    initial_path: p
                        .get("initialPath")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    provider_id: p
                        .get("providerId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                })
            })
            .collect())
    }

    fn get_credentials(&self, server_id: &str) -> Result<ServerCredentials, String> {
        let store = crate::credential_store::CredentialStore::from_cache()
            .ok_or_else(|| "Credential vault not open".to_string())?;
        // MUV-3: per-user store (active user) with fallback to the legacy vault.
        let json_str = crate::user_partitions::resolve_active_credential(
            &store,
            &format!("server_{}", server_id),
        )
        .map_err(|e| format!("No credentials stored for server '{}': {}", server_id, e))?
        .ok_or_else(|| format!("No credentials stored for server '{}'", server_id))?
        .to_string();

        #[derive(serde::Deserialize)]
        struct Creds {
            #[serde(default)]
            server: String,
            #[serde(default)]
            username: String,
            #[serde(default)]
            password: String,
        }
        let c: Creds = serde_json::from_str(&json_str).map_err(|e| e.to_string())?;
        Ok(ServerCredentials {
            server: c.server,
            username: c.username,
            password: c.password,
        })
    }

    fn get_extra_options(&self, server_id: &str) -> Result<ProviderExtraOptions, String> {
        const SENSITIVE_KEYS: &[&str] = &[
            "password",
            "access_token",
            "refresh_token",
            "api_key",
            "secret",
            "token",
            "oauth_token",
            "oauth_token_secret",
            "server",
            "username",
        ];

        let store = crate::credential_store::CredentialStore::from_cache()
            .ok_or_else(|| "Credential vault not open".to_string())?;
        // MUV-3: per-user store (active user) with fallback to the legacy vault.
        let json_str = crate::user_partitions::resolve_active_credential(
            &store,
            &format!("server_{}", server_id),
        )
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "{}".to_string());
        let val: Value = serde_json::from_str(&json_str).unwrap_or(json!({}));
        let mut opts = ProviderExtraOptions::new();
        if let Some(obj) = val.as_object() {
            for (k, v) in obj {
                let lower = k.to_lowercase();
                if SENSITIVE_KEYS.iter().any(|s| lower == *s) {
                    continue;
                }
                if let Some(s) = v.as_str() {
                    opts.insert(k.clone(), s.to_string());
                }
            }
        }
        Ok(opts)
    }
}

// ─── TauriRemoteBackend ──────────────────────────────────────────────

type ActiveProviderArc = Arc<tokio::sync::Mutex<Option<Box<dyn StorageProvider>>>>;

/// Remote backend for GUI tool calls (TOOLS-01).
///
/// Previously this struct borrowed `&'a ProviderState` / `&'a AppState`, which
/// could never satisfy the `'static` bound of `Arc<dyn RemoteBackend>`; that is
/// exactly why `TauriToolCtx::remote_backend` was a permanent error stub and
/// every GUI remote tool failed at runtime. It now owns `'static` handles, so it
/// can be wrapped in an `Arc` and returned:
///
/// * `Active` resolves the live `ProviderState`/`AppState` managed state per
///   call (the GUI's "operate on the active connection" contract).
/// * `Temp` holds a vault-resolved provider for a named server, mirroring the
///   CLI/MCP named-server path.
pub enum TauriRemoteBackend {
    Active {
        app: AppHandle,
    },
    Temp {
        provider: tokio::sync::Mutex<Box<dyn StorageProvider>>,
    },
}

const MAX_AI_DOWNLOAD_SIZE: u64 = 50 * 1024 * 1024;

impl TauriRemoteBackend {
    /// Clone the active provider `Arc` out of managed state so we never hold the
    /// Tauri `State` guard across an `.await`.
    fn active_provider(app: &AppHandle) -> ActiveProviderArc {
        app.state::<crate::provider_commands::ProviderState>()
            .provider
            .clone()
    }
}

#[async_trait]
impl RemoteBackend for TauriRemoteBackend {
    async fn is_connected(&self) -> bool {
        match self {
            TauriRemoteBackend::Active { app } => {
                if Self::active_provider(app).lock().await.is_some() {
                    return true;
                }
                app.state::<AppState>()
                    .ftp_manager
                    .lock()
                    .await
                    .is_connected()
            }
            TauriRemoteBackend::Temp { .. } => true,
        }
    }

    async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p.list(path).await.map_err(|e| e.to_string());
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                mgr.change_dir(path).await.map_err(|e| e.to_string())?;
                let files = mgr.list_files().await.map_err(|e| e.to_string())?;
                Ok(files
                    .into_iter()
                    .map(|f| RemoteEntry {
                        name: f.name.clone(),
                        path: format!("{}/{}", path.trim_end_matches('/'), f.name),
                        is_dir: f.is_dir,
                        size: f.size.unwrap_or(0),
                        modified: f.modified.clone(),
                        permissions: f.permissions.clone(),
                        owner: None,
                        group: None,
                        is_symlink: false,
                        link_target: None,
                        mime_type: None,
                        metadata: std::collections::HashMap::new(),
                    })
                    .collect())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .list(path)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn stat(&self, path: &str) -> Result<RemoteEntry, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p.stat(path).await.map_err(|e| e.to_string());
                }
                Err("stat not supported on FTP fallback".to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .stat(path)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn download_to_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return download_provider_bytes(p, path).await;
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                mgr.download_to_bytes(path).await.map_err(|e| e.to_string())
            }
            TauriRemoteBackend::Temp { provider } => {
                let mut guard = provider.lock().await;
                download_provider_bytes(&mut guard, path).await
            }
        }
    }

    async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return upload_provider_bytes(p, data, path).await;
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                let tmp = temp_upload_path();
                std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
                let result = mgr
                    .upload_file(tmp.to_str().unwrap_or(""), path)
                    .await
                    .map_err(|e| e.to_string());
                let _ = std::fs::remove_file(&tmp);
                result
            }
            TauriRemoteBackend::Temp { provider } => {
                let mut guard = provider.lock().await;
                upload_provider_bytes(&mut guard, data, path).await
            }
        }
    }

    async fn download(&self, remote: &str, local: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p
                        .download(remote, local, None)
                        .await
                        .map_err(|e| e.to_string());
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                mgr.download_file(remote, local)
                    .await
                    .map_err(|e| e.to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .download(remote, local, None)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn upload(&self, local: &str, remote: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p
                        .upload(local, remote, None)
                        .await
                        .map_err(|e| e.to_string());
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                mgr.upload_file(local, remote)
                    .await
                    .map_err(|e| e.to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .upload(local, remote, None)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn delete(&self, path: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p.delete(path).await.map_err(|e| e.to_string());
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                mgr.remove(path).await.map_err(|e| e.to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .delete(path)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn mkdir(&self, path: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p.mkdir(path).await.map_err(|e| e.to_string());
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                mgr.mkdir(path).await.map_err(|e| e.to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .mkdir(path)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p.rename(from, to).await.map_err(|e| e.to_string());
                }
                let app_state = app.state::<AppState>();
                let mut mgr = app_state.ftp_manager.lock().await;
                mgr.rename(from, to).await.map_err(|e| e.to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .rename(from, to)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn search(&self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p.find(path, pattern).await.map_err(|e| e.to_string());
                }
                Err("search not supported on FTP fallback".to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .find(path, pattern)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn storage_info(&self) -> Result<StorageQuota, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return provider_storage_quota(p).await;
                }
                Err("storage quota not supported on FTP fallback".to_string())
            }
            TauriRemoteBackend::Temp { provider } => {
                let mut guard = provider.lock().await;
                provider_storage_quota(&mut guard).await
            }
        }
    }

    async fn transfer_capabilities(
        &self,
    ) -> Result<Option<crate::transfer_dag::TransferCapabilities>, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref p) = *Self::active_provider(app).lock().await {
                    return Ok(Some(p.transfer_capabilities()));
                }
                Ok(None)
            }
            TauriRemoteBackend::Temp { provider } => {
                Ok(Some(provider.lock().await.transfer_capabilities()))
            }
        }
    }

    async fn list_versions(
        &self,
        path: &str,
    ) -> Result<Vec<crate::providers::FileVersion>, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p.list_versions(path).await.map_err(|e| e.to_string());
                }
                Err("versions require an active provider connection".to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .list_versions(path)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn restore_version(&self, path: &str, version_id: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p
                        .restore_version(path, version_id)
                        .await
                        .map_err(|e| e.to_string());
                }
                Err("versions require an active provider connection".to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .restore_version(path, version_id)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn delete_version(&self, path: &str, version_id: &str) -> Result<(), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p
                        .delete_version(path, version_id)
                        .await
                        .map_err(|e| e.to_string());
                }
                Err("versions require an active provider connection".to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .delete_version(path, version_id)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn list_object_versions(
        &self,
        prefix: &str,
        include_noncurrent: bool,
    ) -> Result<Vec<crate::providers::TrashEntry>, String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p
                        .list_object_versions(prefix, include_noncurrent)
                        .await
                        .map_err(|e| e.to_string());
                }
                Err("trash browse requires an active provider connection".to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .list_object_versions(prefix, include_noncurrent)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    async fn empty_object_versions(
        &self,
        prefix: &str,
        include_noncurrent: bool,
        dry_run: bool,
    ) -> Result<(u64, u64), String> {
        match self {
            TauriRemoteBackend::Active { app } => {
                if let Some(ref mut p) = *Self::active_provider(app).lock().await {
                    return p
                        .empty_object_versions(prefix, include_noncurrent, dry_run)
                        .await
                        .map_err(|e| e.to_string());
                }
                Err("trash empty requires an active provider connection".to_string())
            }
            TauriRemoteBackend::Temp { provider } => provider
                .lock()
                .await
                .empty_object_versions(prefix, include_noncurrent, dry_run)
                .await
                .map_err(|e| e.to_string()),
        }
    }
}

fn temp_upload_path() -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("aeroftp_upload_{}_{}", std::process::id(), nonce))
}

async fn download_provider_bytes(
    p: &mut Box<dyn StorageProvider>,
    path: &str,
) -> Result<Vec<u8>, String> {
    if let Ok(entry) = p.stat(path).await {
        if entry.size > MAX_AI_DOWNLOAD_SIZE {
            return Err(format!(
                "File too large ({:.1} MB). Limit is {} MB.",
                entry.size as f64 / 1_048_576.0,
                MAX_AI_DOWNLOAD_SIZE / 1_048_576
            ));
        }
    }
    p.download_to_bytes(path).await.map_err(|e| e.to_string())
}

async fn upload_provider_bytes(
    p: &mut Box<dyn StorageProvider>,
    data: &[u8],
    path: &str,
) -> Result<(), String> {
    // Write data to a secure temp file, upload, then clean up.
    let tmp = temp_upload_path();
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    let result = p
        .upload(tmp.to_str().unwrap_or(""), path, None)
        .await
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&tmp);
    result
}

async fn provider_storage_quota(p: &mut Box<dyn StorageProvider>) -> Result<StorageQuota, String> {
    let info = p.storage_info().await.map_err(|e| e.to_string())?;
    Ok(StorageQuota {
        used: info.used,
        total: info.total,
        available: info.free,
    })
}

// ─── TauriToolCtx ─────────────────────────────────────────────────────

use crate::ai_core::tools::{Surfaces, ToolCtx};
use std::sync::Arc;

pub struct TauriToolCtx {
    pub app: AppHandle,
    pub sink: TauriEventSink,
    pub creds: VaultCredentialProvider,
    pub context_local_path: Option<String>,
    pub approval_grant_id: Option<String>,
    pub session_id: Option<String>,
}

#[async_trait]
impl ToolCtx for TauriToolCtx {
    fn event_sink(&self) -> &dyn EventSink {
        &self.sink
    }
    fn credentials(&self) -> &dyn CredentialProvider {
        &self.creds
    }
    async fn remote_backend(&self, server_id: &str) -> Result<Arc<dyn RemoteBackend>, String> {
        // TOOLS-01 / W1-2: an empty server selects the live active connection
        // (the GUI's default contract); a named server resolves a vault profile
        // and builds a temp provider, mirroring the CLI/MCP named-server path.
        if server_id.trim().is_empty() {
            return Ok(Arc::new(TauriRemoteBackend::Active {
                app: self.app.clone(),
            }));
        }
        let servers = crate::ai_tools::load_saved_servers()?;
        let server = crate::ai_tools::find_server_by_name_or_id(&servers, server_id)?;
        let provider = crate::ai_tools::create_temp_provider(&server).await?;
        Ok(Arc::new(TauriRemoteBackend::Temp {
            provider: tokio::sync::Mutex::new(provider),
        }))
    }
    fn context_local_path(&self) -> Option<&str> {
        self.context_local_path.as_deref()
    }
    fn approval_grant_id(&self) -> Option<&str> {
        self.approval_grant_id.as_deref()
    }
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    fn tauri_app_handle(&self) -> Option<tauri::AppHandle> {
        Some(self.app.clone())
    }
    fn surface(&self) -> Surfaces {
        Surfaces::GUI
    }
}
