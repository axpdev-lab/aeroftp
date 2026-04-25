//! MCP implementation of `ToolCtx` for the unified tool dispatcher.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::credential_provider::{
    CredentialProvider, ProviderExtraOptions, ServerCredentials, ServerProfile,
};
use super::event_sink::{EventSink, ToolProgress};
use super::remote_backend::{RemoteBackend, StorageQuota};
use super::tools::{Surfaces, ToolCtx};
use crate::ai_stream::StreamChunk;
use crate::mcp::notifier::McpNotifier;
use crate::mcp::pool::ConnectionPool;
use crate::providers::RemoteEntry;

pub struct McpEventSink {
    pub notifier: Option<McpNotifier>,
}

impl EventSink for McpEventSink {
    fn emit_stream_chunk(&self, _stream_id: &str, _chunk: &StreamChunk) {}

    fn emit_tool_progress(&self, progress: &ToolProgress) {
        if let Some(notifier) = self.notifier.clone() {
            let current = progress.current as u64;
            let total = progress.total as u64;
            let msg = format!("{}/{} {}", progress.current, progress.total, progress.item);
            tokio::spawn(async move {
                notifier
                    .send_progress(current, Some(total), Some(msg))
                    .await;
            });
        }
    }

    fn emit_app_control(&self, _event_name: &str, _payload: &Value) {}
}

pub struct McpCredentialProvider;

impl CredentialProvider for McpCredentialProvider {
    fn list_servers(&self) -> Result<Vec<ServerProfile>, String> {
        let profiles = crate::mcp::load_safe_profiles()?;
        Ok(profiles
            .into_iter()
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
                        .map(str::to_string),
                    provider_id: p
                        .get("providerId")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                })
            })
            .collect())
    }

    fn get_credentials(&self, server_id: &str) -> Result<ServerCredentials, String> {
        Err(format!(
            "MCP credentials for '{}' are resolved inside ConnectionPool and are not exposed",
            server_id
        ))
    }

    fn get_extra_options(&self, _server_id: &str) -> Result<ProviderExtraOptions, String> {
        Ok(ProviderExtraOptions::new())
    }
}

pub struct McpToolCtx {
    pub pool: Arc<ConnectionPool>,
    pub sink: McpEventSink,
    pub creds: McpCredentialProvider,
}

impl McpToolCtx {
    pub fn new(pool: Arc<ConnectionPool>, notifier: Option<McpNotifier>) -> Self {
        Self {
            pool,
            sink: McpEventSink { notifier },
            creds: McpCredentialProvider,
        }
    }
}

#[async_trait]
impl ToolCtx for McpToolCtx {
    fn event_sink(&self) -> &dyn EventSink {
        &self.sink
    }

    fn credentials(&self) -> &dyn CredentialProvider {
        &self.creds
    }

    async fn remote_backend(&self, server_id: &str) -> Result<Arc<dyn RemoteBackend>, String> {
        Ok(Arc::new(McpRemoteBackend {
            pool: Arc::clone(&self.pool),
            server: server_id.to_string(),
        }))
    }

    fn surface(&self) -> Surfaces {
        Surfaces::MCP
    }
}

pub struct McpRemoteBackend {
    pool: Arc<ConnectionPool>,
    server: String,
}

impl McpRemoteBackend {
    async fn with_provider<T, F>(&self, op: F) -> Result<T, String>
    where
        F: for<'p> FnOnce(
            &'p mut Box<dyn crate::providers::StorageProvider>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<T, crate::providers::ProviderError>>
                    + Send
                    + 'p,
            >,
        >,
    {
        let arc = self.pool.get_provider(&self.server).await?;
        let mut guard = arc.lock().await;
        op(&mut guard)
            .await
            .map_err(|e| crate::providers::sanitize_api_error(&e.to_string()))
    }
}

#[async_trait]
impl RemoteBackend for McpRemoteBackend {
    async fn is_connected(&self) -> bool {
        self.pool.get_provider(&self.server).await.is_ok()
    }

    async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.list(&path).await }))
            .await
    }

    async fn stat(&self, path: &str) -> Result<RemoteEntry, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.stat(&path).await }))
            .await
    }

    async fn download_to_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.download_to_bytes(&path).await }))
            .await
    }

    async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp_mcp_upload_{}_{}",
            std::process::id(),
            nonce
        ));
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        let tmp_str = tmp.to_string_lossy().to_string();
        let path = path.to_string();
        let result = self
            .with_provider(move |p| {
                let local = tmp_str.clone();
                Box::pin(async move { p.upload(&local, &path, None).await })
            })
            .await;
        let _ = std::fs::remove_file(&tmp);
        result
    }

    async fn download(&self, remote: &str, local: &str) -> Result<(), String> {
        let remote = remote.to_string();
        let local = local.to_string();
        self.with_provider(move |p| {
            Box::pin(async move { p.download(&remote, &local, None).await })
        })
        .await
    }

    async fn upload(&self, local: &str, remote: &str) -> Result<(), String> {
        let local = local.to_string();
        let remote = remote.to_string();
        self.with_provider(move |p| Box::pin(async move { p.upload(&local, &remote, None).await }))
            .await
    }

    async fn delete(&self, path: &str) -> Result<(), String> {
        let path = path.to_string();
        self.with_provider(move |p| {
            let path = path.clone();
            Box::pin(async move {
                let entry = p.stat(&path).await?;
                if entry.is_dir {
                    p.rmdir_recursive(&path).await
                } else {
                    p.delete(&path).await
                }
            })
        })
        .await
    }

    async fn mkdir(&self, path: &str) -> Result<(), String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.mkdir(&path).await }))
            .await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let from = from.to_string();
        let to = to.to_string();
        self.with_provider(move |p| Box::pin(async move { p.rename(&from, &to).await }))
            .await
    }

    async fn search(&self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, String> {
        let path = path.to_string();
        let pattern = pattern.to_string();
        self.with_provider(move |p| Box::pin(async move { p.find(&path, &pattern).await }))
            .await
    }

    async fn storage_info(&self) -> Result<StorageQuota, String> {
        let info = self
            .with_provider(move |p| Box::pin(async move { p.storage_info().await }))
            .await?;
        Ok(StorageQuota {
            used: info.used,
            total: info.total,
            available: info.free,
        })
    }
}
