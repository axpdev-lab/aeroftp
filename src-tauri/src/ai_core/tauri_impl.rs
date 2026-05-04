//! Tauri implementations of AI Core traits
//!
//! Wraps existing AppHandle, ProviderState, AppState for backward compatibility.
//! The GUI continues to work exactly as before via these wrappers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::credential_provider::{
    CredentialProvider, ProviderExtraOptions, ServerCredentials, ServerProfile,
};
use super::event_sink::{EventSink, ToolProgress};
use super::remote_backend::{RemoteBackend, StorageQuota};
use crate::ai_stream::StreamChunk;
use crate::provider_commands::ProviderState;
use crate::providers::RemoteEntry;
use crate::AppState;

// ─── TauriEventSink ────────────────────────────────────────────────────

/// Wraps Tauri AppHandle to emit events to the frontend.
pub struct TauriEventSink {
    pub(crate) app: AppHandle,
}

impl TauriEventSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit_stream_chunk(&self, stream_id: &str, chunk: &StreamChunk) {
        let event_name = format!("ai-stream-{}", stream_id);
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
        let json_str = store
            .get(&format!("server_{}", server_id))
            .map_err(|_| format!("No credentials stored for server '{}'", server_id))?;

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
        let json_str = store
            .get(&format!("server_{}", server_id))
            .unwrap_or_else(|_| "{}".to_string());
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

/// Wraps ProviderState + AppState for remote operations via Tauri managed state.
pub struct TauriRemoteBackend<'a> {
    pub(crate) provider_state: &'a ProviderState,
    pub(crate) app_state: &'a AppState,
}

const MAX_AI_DOWNLOAD_SIZE: u64 = 50 * 1024 * 1024;

#[async_trait]
impl<'a> RemoteBackend for TauriRemoteBackend<'a> {
    async fn is_connected(&self) -> bool {
        self.provider_state.provider.lock().await.is_some()
            || self.app_state.ftp_manager.lock().await.is_connected()
    }

    async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p.list(path).await.map_err(|e| e.to_string());
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
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

    async fn stat(&self, path: &str) -> Result<RemoteEntry, String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p.stat(path).await.map_err(|e| e.to_string());
        }
        Err("stat not supported on FTP fallback".to_string())
    }

    async fn download_to_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            if let Ok(entry) = p.stat(path).await {
                if entry.size > MAX_AI_DOWNLOAD_SIZE {
                    return Err(format!(
                        "File too large ({:.1} MB). Limit is {} MB.",
                        entry.size as f64 / 1_048_576.0,
                        MAX_AI_DOWNLOAD_SIZE / 1_048_576
                    ));
                }
            }
            return p.download_to_bytes(path).await.map_err(|e| e.to_string());
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
        mgr.download_to_bytes(path).await.map_err(|e| e.to_string())
    }

    async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            // Write data to a secure temp file (exclusive create), upload, then clean up
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let tmp = std::env::temp_dir().join(format!(
                "aeroftp_upload_{}_{}",
                std::process::id(),
                nonce
            ));
            std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
            let result = p
                .upload(tmp.to_str().unwrap_or(""), path, None)
                .await
                .map_err(|e| e.to_string());
            let _ = std::fs::remove_file(&tmp);
            return result;
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp =
            std::env::temp_dir().join(format!("aeroftp_upload_{}_{}", std::process::id(), nonce));
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        let result = mgr
            .upload_file(tmp.to_str().unwrap_or(""), path)
            .await
            .map_err(|e| e.to_string());
        let _ = std::fs::remove_file(&tmp);
        result
    }

    async fn download(&self, remote: &str, local: &str) -> Result<(), String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p
                .download(remote, local, None)
                .await
                .map_err(|e| e.to_string());
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
        mgr.download_file(remote, local)
            .await
            .map_err(|e| e.to_string())
    }

    async fn upload(&self, local: &str, remote: &str) -> Result<(), String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p
                .upload(local, remote, None)
                .await
                .map_err(|e| e.to_string());
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
        mgr.upload_file(local, remote)
            .await
            .map_err(|e| e.to_string())
    }

    async fn delete(&self, path: &str) -> Result<(), String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p.delete(path).await.map_err(|e| e.to_string());
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
        mgr.remove(path).await.map_err(|e| e.to_string())
    }

    async fn mkdir(&self, path: &str) -> Result<(), String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p.mkdir(path).await.map_err(|e| e.to_string());
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
        mgr.mkdir(path).await.map_err(|e| e.to_string())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p.rename(from, to).await.map_err(|e| e.to_string());
        }
        let mut mgr = self.app_state.ftp_manager.lock().await;
        mgr.rename(from, to).await.map_err(|e| e.to_string())
    }

    async fn search(&self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            return p.find(path, pattern).await.map_err(|e| e.to_string());
        }
        Err("search not supported on FTP fallback".to_string())
    }

    async fn storage_info(&self) -> Result<StorageQuota, String> {
        if let Some(ref mut p) = *self.provider_state.provider.lock().await {
            let _info = p.server_info().await.map_err(|e| e.to_string())?;
            return Err(
                "Storage quota extraction not yet implemented for this provider".to_string(),
            );
        }
        Err("storage quota not supported on FTP fallback".to_string())
    }
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
}

#[async_trait]
impl ToolCtx for TauriToolCtx {
    fn event_sink(&self) -> &dyn EventSink {
        &self.sink
    }
    fn credentials(&self) -> &dyn CredentialProvider {
        &self.creds
    }
    async fn remote_backend(&self, _server_id: &str) -> Result<Arc<dyn RemoteBackend>, String> {
        Err("remote_backend not wired in TauriToolCtx (Area A)".to_string())
    }
    fn context_local_path(&self) -> Option<&str> {
        self.context_local_path.as_deref()
    }
    fn approval_grant_id(&self) -> Option<&str> {
        self.approval_grant_id.as_deref()
    }
    fn tauri_app_handle(&self) -> Option<tauri::AppHandle> {
        Some(self.app.clone())
    }
    fn surface(&self) -> Surfaces {
        Surfaces::GUI
    }
}
