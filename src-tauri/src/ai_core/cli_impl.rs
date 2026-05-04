//! CLI implementations of AI Core traits
//!
//! Used by the aeroftp-cli binary when running in agent mode.
//! No Tauri dependency: pure Rust.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::credential_provider::{
    CredentialProvider, ProviderExtraOptions, ServerCredentials, ServerProfile,
};
use super::event_sink::{EventSink, ToolProgress};
use super::remote_backend::{RemoteBackend, StorageQuota};
use crate::ai_stream::StreamChunk;
use crate::providers::{RemoteEntry, StorageProvider};

// ─── CliEventSink ─────────────────────────────────────────────────────

/// CLI event sink: writes streaming output to stdout/stderr.
pub struct CliEventSink {
    pub json_mode: bool,
}

impl EventSink for CliEventSink {
    fn emit_stream_chunk(&self, _stream_id: &str, chunk: &StreamChunk) {
        if self.json_mode {
            if let Ok(json) = serde_json::to_string(chunk) {
                println!("{}", json);
            }
        } else {
            if !chunk.content.is_empty() {
                print!("{}", chunk.content);
            }
            if let Some(ref thinking) = chunk.thinking {
                if !thinking.is_empty() {
                    eprint!("\x1b[2m{}\x1b[0m", thinking);
                }
            }
            if chunk.done {
                println!();
            }
        }
    }

    fn emit_tool_progress(&self, progress: &ToolProgress) {
        if self.json_mode {
            if let Ok(json) = serde_json::to_string(progress) {
                eprintln!("{}", json);
            }
        } else {
            eprint!(
                "\r  [{}/{}] {}",
                progress.current, progress.total, progress.item
            );
            if progress.current == progress.total {
                eprintln!();
            }
        }
    }

    fn emit_app_control(&self, _event_name: &str, _payload: &Value) {
        // CLI ignores GUI-only events (theme, sync control)
    }
}

// ─── StdioEventSink ───────────────────────────────────────────────────

/// JSON-RPC event sink for orchestration mode: line-delimited JSON on stdout.
pub struct StdioEventSink;

impl EventSink for StdioEventSink {
    fn emit_stream_chunk(&self, stream_id: &str, chunk: &StreamChunk) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "stream/chunk",
            "params": {
                "stream_id": stream_id,
                "content": chunk.content,
                "done": chunk.done,
                "tool_calls": chunk.tool_calls,
                "thinking": chunk.thinking,
                "input_tokens": chunk.input_tokens,
                "output_tokens": chunk.output_tokens,
            }
        });
        println!("{}", msg);
    }

    fn emit_tool_progress(&self, progress: &ToolProgress) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "progress",
            "params": progress
        });
        println!("{}", msg);
    }

    fn emit_app_control(&self, event_name: &str, payload: &Value) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "app_control",
            "params": {
                "event": event_name,
                "payload": payload
            }
        });
        println!("{}", msg);
    }
}

// ─── CliCredentialProvider ─────────────────────────────────────────────

/// CLI credential provider: vault cache (if open) → env vars (AEROFTP_HOST/USER/PASS).
pub struct CliCredentialProvider;

impl CredentialProvider for CliCredentialProvider {
    fn list_servers(&self) -> Result<Vec<ServerProfile>, String> {
        // Try vault first (may be open from a previous unlock)
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if let Ok(json_str) = store.get("config_server_profiles") {
                if let Ok(profiles) = serde_json::from_str::<Vec<Value>>(&json_str) {
                    return Ok(profiles
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
                        .collect());
                }
            }
        }
        // No vault: return empty
        Ok(Vec::new())
    }

    fn get_credentials(&self, server_id: &str) -> Result<ServerCredentials, String> {
        // 1. Try vault
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if let Ok(json_str) = store.get(&format!("server_{}", server_id)) {
                #[derive(serde::Deserialize)]
                struct Creds {
                    #[serde(default)]
                    server: String,
                    #[serde(default)]
                    username: String,
                    #[serde(default)]
                    password: String,
                }
                if let Ok(c) = serde_json::from_str::<Creds>(&json_str) {
                    return Ok(ServerCredentials {
                        server: c.server,
                        username: c.username,
                        password: c.password,
                    });
                }
            }
        }
        // 2. Try env vars
        if let (Ok(host), Ok(user)) = (std::env::var("AEROFTP_HOST"), std::env::var("AEROFTP_USER"))
        {
            let pass = std::env::var("AEROFTP_PASS").unwrap_or_default();
            return Ok(ServerCredentials {
                server: host,
                username: user,
                password: pass,
            });
        }
        Err(format!("No credentials found for server '{}'. Open vault or set AEROFTP_HOST/AEROFTP_USER/AEROFTP_PASS env vars.", server_id))
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

        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if let Ok(json_str) = store.get(&format!("server_{}", server_id)) {
                let val: Value = serde_json::from_str(&json_str).unwrap_or(serde_json::json!({}));
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
                return Ok(opts);
            }
        }
        Ok(ProviderExtraOptions::new())
    }
}

// ─── NullRemoteBackend ────────────────────────────────────────────────

/// No-op remote backend: used when no server is connected in CLI mode.
pub struct NullRemoteBackend;

#[async_trait]
impl RemoteBackend for NullRemoteBackend {
    async fn is_connected(&self) -> bool {
        false
    }
    async fn list(&self, _path: &str) -> Result<Vec<RemoteEntry>, String> {
        Err("Not connected to any server".into())
    }
    async fn stat(&self, _path: &str) -> Result<RemoteEntry, String> {
        Err("Not connected".into())
    }
    async fn download_to_bytes(&self, _path: &str) -> Result<Vec<u8>, String> {
        Err("Not connected".into())
    }
    async fn upload_from_bytes(&self, _data: &[u8], _path: &str) -> Result<(), String> {
        Err("Not connected".into())
    }
    async fn download(&self, _remote: &str, _local: &str) -> Result<(), String> {
        Err("Not connected".into())
    }
    async fn upload(&self, _local: &str, _remote: &str) -> Result<(), String> {
        Err("Not connected".into())
    }
    async fn delete(&self, _path: &str) -> Result<(), String> {
        Err("Not connected".into())
    }
    async fn mkdir(&self, _path: &str) -> Result<(), String> {
        Err("Not connected".into())
    }
    async fn rename(&self, _from: &str, _to: &str) -> Result<(), String> {
        Err("Not connected".into())
    }
    async fn search(&self, _path: &str, _pattern: &str) -> Result<Vec<RemoteEntry>, String> {
        Err("Not connected".into())
    }
    async fn storage_info(&self) -> Result<StorageQuota, String> {
        Err("Not connected".into())
    }
}

// ─── CliRemoteBackend ─────────────────────────────────────────────────

/// CLI owns a single StorageProvider connection directly.
pub struct CliRemoteBackend {
    provider: Mutex<Option<Box<dyn StorageProvider>>>,
}

impl Default for CliRemoteBackend {
    fn default() -> Self {
        Self {
            provider: Mutex::new(None),
        }
    }
}

impl CliRemoteBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn connect(&self, provider: Box<dyn StorageProvider>) {
        *self.provider.lock().await = Some(provider);
    }

    pub async fn disconnect(&self) {
        *self.provider.lock().await = None;
    }
}

const MAX_AI_DOWNLOAD_SIZE: u64 = 50 * 1024 * 1024;

#[async_trait]
impl RemoteBackend for CliRemoteBackend {
    async fn is_connected(&self) -> bool {
        self.provider.lock().await.is_some()
    }

    async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected to any server")?;
        p.list(path).await.map_err(|e| e.to_string())
    }

    async fn stat(&self, path: &str) -> Result<RemoteEntry, String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        p.stat(path).await.map_err(|e| e.to_string())
    }

    async fn download_to_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
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

    async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp_cli_upload_{}_{}",
            std::process::id(),
            nonce
        ));
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        let result = p
            .upload(tmp.to_str().unwrap_or(""), path, None)
            .await
            .map_err(|e| e.to_string());
        let _ = std::fs::remove_file(&tmp);
        result
    }

    async fn download(&self, remote: &str, local: &str) -> Result<(), String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        p.download(remote, local, None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn upload(&self, local: &str, remote: &str) -> Result<(), String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        p.upload(local, remote, None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn delete(&self, path: &str) -> Result<(), String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        p.delete(path).await.map_err(|e| e.to_string())
    }

    async fn mkdir(&self, path: &str) -> Result<(), String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        p.mkdir(path).await.map_err(|e| e.to_string())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        p.rename(from, to).await.map_err(|e| e.to_string())
    }

    async fn search(&self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        p.find(path, pattern).await.map_err(|e| e.to_string())
    }

    async fn storage_info(&self) -> Result<StorageQuota, String> {
        let mut guard = self.provider.lock().await;
        let p = guard.as_mut().ok_or("Not connected")?;
        let _info = p.server_info().await.map_err(|e| e.to_string())?;
        Err("Storage quota extraction not yet implemented for this provider".to_string())
    }
}

// ─── CliToolCtx ───────────────────────────────────────────────────────

use crate::ai_core::tools::{Surfaces, ToolCtx};
use std::sync::Arc;

pub struct CliToolCtx {
    pub sink: CliEventSink,
    pub creds: CliCredentialProvider,
    /// Cwd snapshot al momento della costruzione, usato come base per
    /// risolvere path relativi. Mirror del legacy CLI `resolve_path`
    /// che faceva `std::env::current_dir().join(path)`.
    pub cwd: Option<String>,
}

impl CliToolCtx {
    /// Costruttore con `cwd` automatica. Se `current_dir()` fallisce
    /// (caso raro: dir cancellata), `cwd` resta `None` e il dispatcher
    /// userà il path inalterato (stesso comportamento del legacy CLI
    /// con `unwrap_or_else(|_| path.to_string())`).
    pub fn new(sink: CliEventSink, creds: CliCredentialProvider) -> Self {
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        Self { sink, creds, cwd }
    }
}

#[async_trait]
impl ToolCtx for CliToolCtx {
    fn event_sink(&self) -> &dyn EventSink {
        &self.sink
    }
    fn credentials(&self) -> &dyn CredentialProvider {
        &self.creds
    }
    async fn remote_backend(&self, _server_id: &str) -> Result<Arc<dyn RemoteBackend>, String> {
        Err("remote_backend not wired in CliToolCtx (Area A)".to_string())
    }
    fn context_local_path(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
    fn surface(&self) -> Surfaces {
        Surfaces::CLI
    }
}
