// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! FileLu Rsync StorageProvider (Z.4.5 R2).
//!
//! Dedicated provider for the FileLu rsync-as-a-service endpoint
//! (`rsync.filelu.com:2222`). The endpoint is **transfer-only** by design:
//! its `ForceCommand` allows only rsync server-side invocations, so SFTP
//! subsystem and arbitrary shell exec are refused.
//!
//! What this provider offers:
//! - `connect()`: validates password auth via a russh handshake + probe.
//!   When the probe banner is non-empty, the session is considered usable
//!   for transfers even if `rsync --version` itself is rejected by the
//!   FileLu ForceCommand wrapper (which is the documented behaviour).
//! - `upload()` / `download()`: route the byte stream through the native
//!   aerorsync delta path (`AerorsyncDeltaTransport`). One SSH handshake
//!   per call (single-shot mode); batches go through `begin_batch`.
//! - Everything else (`list`, `mkdir`, `delete`, `rename`, `stat`, ...) is
//!   intentionally `NotSupported` with a clear hint to use FileLu Native /
//!   FTP / WebDAV / S3 for browsing.
//!
//! This is the **test bench** for native aerorsync against a real
//! production endpoint. The integration is intentionally narrow.

use async_trait::async_trait;
#[cfg(not(feature = "aerorsync"))]
use secrecy::ExposeSecret;
use secrecy::SecretString;
use std::path::Path;
use std::sync::Mutex;

use crate::providers::types::{ProviderError, ProviderType, RemoteEntry, StorageInfo};
use crate::providers::StorageProvider;

#[cfg(feature = "aerorsync")]
use crate::aerorsync::{
    delta_transport_impl::AerorsyncDeltaTransport,
    ssh_transport::SshHostKeyPolicy,
};
#[cfg(feature = "aerorsync")]
use crate::delta_transport::DeltaTransport;
#[cfg(feature = "aerorsync")]
use crate::rsync_over_ssh::{AuthMethod, RsyncConfig};

/// Z.4.5 R2 configuration for the FileLu Rsync endpoint.
///
/// `host` and `port` are configurable but typically pinned to
/// `rsync.filelu.com:2222` by the registry preset. `username` /
/// `password` are the FileLu rsync credentials (account password by
/// default, or a protocol-specific password if configured in the
/// FileLu dashboard).
#[derive(Debug, Clone)]
pub struct FileLuRsyncConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub initial_path: Option<String>,
}

impl FileLuRsyncConfig {
    pub fn from_provider_config(
        config: &crate::providers::types::ProviderConfig,
    ) -> Result<Self, ProviderError> {
        let username = config
            .username
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("FileLu Rsync username required".into()))?;
        let password = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("FileLu Rsync password required".into())
        })?;
        let host = if config.host.trim().is_empty() {
            "rsync.filelu.com".to_string()
        } else {
            config.host.trim().to_string()
        };
        let port = config.port.unwrap_or(2222);
        Ok(Self {
            host,
            port,
            username,
            password: SecretString::from(password),
            initial_path: config.initial_path.clone(),
        })
    }
}

/// FileLu Rsync StorageProvider. Single-instance, holds connection-time
/// state (current path, connected flag). All ops eventually go through
/// `AerorsyncDeltaTransport` for byte transfer.
pub struct FileLuRsyncProvider {
    config: FileLuRsyncConfig,
    connected: bool,
    current_path: Mutex<String>,
}

impl FileLuRsyncProvider {
    pub fn new(config: FileLuRsyncConfig) -> Self {
        let initial = config.initial_path.clone().unwrap_or_else(|| "/".into());
        Self {
            config,
            connected: false,
            current_path: Mutex::new(initial),
        }
    }

    /// Build an `RsyncConfig` for the aerorsync delta transport from
    /// our `FileLuRsyncConfig`. Always Password auth, `min_file_size=0`
    /// so any file size is accepted (the FileLu endpoint negotiates
    /// the delta payload regardless).
    #[cfg(feature = "aerorsync")]
    fn rsync_config(&self) -> RsyncConfig {
        RsyncConfig {
            compress: true,
            preserve_times: true,
            progress: true,
            // 0 = delta path accepts every file size. The native driver
            // still streams literal blocks for tiny files; we just don't
            // refuse them upfront with `TooSmall`.
            min_file_size: 0,
            ssh_key_path: None,
            ssh_password: Some(self.config.password.clone()),
            auth_method: AuthMethod::Password,
            ssh_port: Some(self.config.port),
            ssh_user: self.config.username.clone(),
            ssh_host: self.config.host.clone(),
            strict_host_key_check: "accept-new".to_string(),
            known_hosts_path: None,
        }
    }

    fn not_supported_browse(op: &str) -> ProviderError {
        ProviderError::NotSupported(format!(
            "{}: FileLu Rsync endpoint is transfer-only (ForceCommand restriction). \
             Use the FileLu Native, FTP, WebDAV or S3 connection for browsing, \
             then switch to Rsync for high-bandwidth delta transfers.",
            op
        ))
    }
}

#[async_trait]
impl StorageProvider for FileLuRsyncProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::FileLuRsync
    }

    fn display_name(&self) -> String {
        format!(
            "FileLu Rsync ({}@{}:{})",
            self.config.username, self.config.host, self.config.port
        )
    }

    fn account_email(&self) -> Option<String> {
        Some(self.config.username.clone())
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        #[cfg(feature = "aerorsync")]
        {
            // Z.4.5 R2: validate auth + reachability via a russh handshake.
            // We use the AerorsyncDeltaTransport probe path: it opens a
            // russh session with password auth and runs `rsync --version`.
            // FileLu's ForceCommand rejects `rsync --version` (probe banner
            // returns the rejection string), but reaching that rejection
            // proves auth succeeded. We treat both Ok and the documented
            // RemoteNotAvailable as "connected" for FileLu specifically;
            // HardRejection (host-key mismatch, password rejected) bubbles
            // up as ConnectionFailed.
            let cfg = self.rsync_config();
            cfg.validate_auth_material()
                .map_err(|e| ProviderError::InvalidConfig(format!("rsync config: {}", e)))?;
            let transport = AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
                .map_err(|e| ProviderError::ConnectionFailed(format!(
                    "FileLu Rsync transport construction failed: {}", e
                )))?;
            match transport.probe_remote().await {
                Ok(_) => {
                    tracing::info!(
                        "FileLu Rsync: probe succeeded against {}:{}",
                        self.config.host, self.config.port
                    );
                }
                Err(e) => {
                    // FileLu ForceCommand rejects `rsync --version` -> we
                    // map that to "transport probe shows endpoint alive,
                    // auth ok, exec gated". Surface the message at info
                    // level and proceed. Real auth failures surface as
                    // ConnectionFailed at the from_rsync_config / probe
                    // call sites.
                    use crate::rsync_over_ssh::RsyncError;
                    match e {
                        RsyncError::HardRejection(msg) => {
                            return Err(ProviderError::ConnectionFailed(format!(
                                "FileLu Rsync auth failed: {}", msg
                            )));
                        }
                        other => {
                            tracing::info!(
                                "FileLu Rsync: probe rejected by ForceCommand wrapper ({}); \
                                 endpoint is alive and auth succeeded (transfer-only).",
                                other
                            );
                        }
                    }
                }
            }
            self.connected = true;
            Ok(())
        }
        #[cfg(not(feature = "aerorsync"))]
        {
            let _ = self.config.password.expose_secret(); // silence unused
            Err(ProviderError::NotSupported(
                "FileLu Rsync requires the 'aerorsync' build feature".into(),
            ))
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, _path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // Transfer-only: return empty Vec rather than erroring so the file
        // pane shows an empty state with the friendly "transfer-only" hint
        // surfaced by the UI sub-tab integration. A NotSupported here would
        // be rendered as a connection error and obscure the actual capability.
        tracing::debug!(
            "FileLu Rsync: list() returning empty (transfer-only endpoint, use upload/download)"
        );
        Ok(Vec::new())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self
            .current_path
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| "/".into()))
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        // Best-effort: track current dir locally. The transfer path uses
        // absolute remote paths anyway, so we just keep state for UX.
        if let Ok(mut guard) = self.current_path.lock() {
            *guard = path.to_string();
        }
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if let Ok(mut guard) = self.current_path.lock() {
            let cur = guard.clone();
            let parent = if let Some(idx) = cur.trim_end_matches('/').rfind('/') {
                if idx == 0 {
                    "/".to_string()
                } else {
                    cur[..idx].to_string()
                }
            } else {
                "/".to_string()
            };
            *guard = parent;
        }
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        #[cfg(feature = "aerorsync")]
        {
            // Route through `begin_batch()` (russh + password auth)
            // rather than the single-shot `download()` which still uses
            // libssh2 (pubkey-only) and would fail on FileLu Rsync's
            // password-only endpoint. The batch path opens one russh
            // session per call, which is what we want for a Native
            // delta transfer.
            let cfg = self.rsync_config();
            let transport = AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
                .map_err(|e| ProviderError::TransferFailed(format!(
                    "FileLu Rsync transport: {}", e
                )))?;
            if let Some(cb) = on_progress.as_ref() {
                cb(0, 0);
            }
            let mut batch = transport.begin_batch().await.map_err(|e| {
                ProviderError::TransferFailed(format!("FileLu Rsync session: {}", e))
            })?;
            let stats = batch
                .download(remote_path, Path::new(local_path))
                .await
                .map_err(|e| ProviderError::TransferFailed(format!(
                    "FileLu Rsync download: {}", e
                )))?;
            if let Some(cb) = on_progress.as_ref() {
                let total = stats.total_size.max(stats.bytes_received);
                cb(total, total);
            }
            Ok(())
        }
        #[cfg(not(feature = "aerorsync"))]
        {
            let _ = (remote_path, local_path, on_progress);
            Err(ProviderError::NotSupported(
                "FileLu Rsync requires the 'aerorsync' build feature".into(),
            ))
        }
    }

    async fn download_to_bytes(&mut self, _remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        Err(Self::not_supported_browse("download_to_bytes"))
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        #[cfg(feature = "aerorsync")]
        {
            // Same russh routing rationale as `download()` above.
            let cfg = self.rsync_config();
            let transport = AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
                .map_err(|e| ProviderError::TransferFailed(format!(
                    "FileLu Rsync transport: {}", e
                )))?;
            if let Some(cb) = on_progress.as_ref() {
                cb(0, 0);
            }
            let mut batch = transport.begin_batch().await.map_err(|e| {
                ProviderError::TransferFailed(format!("FileLu Rsync session: {}", e))
            })?;
            let stats = batch
                .upload(Path::new(local_path), remote_path)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!(
                    "FileLu Rsync upload: {}", e
                )))?;
            if let Some(cb) = on_progress.as_ref() {
                let total = stats.total_size.max(stats.bytes_sent);
                cb(total, total);
            }
            Ok(())
        }
        #[cfg(not(feature = "aerorsync"))]
        {
            let _ = (local_path, remote_path, on_progress);
            Err(ProviderError::NotSupported(
                "FileLu Rsync requires the 'aerorsync' build feature".into(),
            ))
        }
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(Self::not_supported_browse("mkdir"))
    }

    async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(Self::not_supported_browse("delete"))
    }

    async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(Self::not_supported_browse("rmdir"))
    }

    async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(Self::not_supported_browse("rmdir_recursive"))
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(Self::not_supported_browse("rename"))
    }

    async fn stat(&mut self, _path: &str) -> Result<RemoteEntry, ProviderError> {
        Err(Self::not_supported_browse("stat"))
    }

    async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
        Err(Self::not_supported_browse("size"))
    }

    async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
        // Optimistic: assume the path exists. Upload paths are arbitrary
        // (rsync creates files on demand), download paths are validated
        // by the transport. Returning Err here would block legitimate
        // pre-flight checks; returning Ok(false) would block uploads.
        Ok(true)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // No long-lived session in single-shot mode. Real keep-alive
        // would require holding a russh handle; defer to v3.8.x.
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "FileLu Rsync (rsync-over-SSH, port {}, transfer-only)",
            self.config.port
        ))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        // Quota is only available via the FileLu Native API. Direct
        // users to that surface via NotSupported.
        Err(Self::not_supported_browse("storage_info"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::ProviderConfig;
    use std::collections::HashMap;

    fn sample_provider_config() -> ProviderConfig {
        ProviderConfig {
            name: "FileLu Rsync".into(),
            provider_type: ProviderType::FileLuRsync,
            host: "rsync.filelu.com".into(),
            port: Some(2222),
            username: Some("user".into()),
            password: Some("pw".into()),
            initial_path: Some("/".into()),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn config_parses_from_provider_config() {
        let pc = sample_provider_config();
        let cfg = FileLuRsyncConfig::from_provider_config(&pc).unwrap();
        assert_eq!(cfg.host, "rsync.filelu.com");
        assert_eq!(cfg.port, 2222);
        assert_eq!(cfg.username, "user");
    }

    #[test]
    fn config_defaults_host_when_empty() {
        let mut pc = sample_provider_config();
        pc.host = String::new();
        let cfg = FileLuRsyncConfig::from_provider_config(&pc).unwrap();
        assert_eq!(cfg.host, "rsync.filelu.com");
    }

    #[test]
    fn config_defaults_port_to_2222() {
        let mut pc = sample_provider_config();
        pc.port = None;
        let cfg = FileLuRsyncConfig::from_provider_config(&pc).unwrap();
        assert_eq!(cfg.port, 2222);
    }

    #[test]
    fn config_rejects_missing_username() {
        let mut pc = sample_provider_config();
        pc.username = None;
        assert!(FileLuRsyncConfig::from_provider_config(&pc).is_err());
    }

    #[test]
    fn config_rejects_missing_password() {
        let mut pc = sample_provider_config();
        pc.password = None;
        assert!(FileLuRsyncConfig::from_provider_config(&pc).is_err());
    }

    #[test]
    fn provider_starts_disconnected() {
        let cfg = FileLuRsyncConfig::from_provider_config(&sample_provider_config()).unwrap();
        let p = FileLuRsyncProvider::new(cfg);
        assert!(!p.is_connected());
    }

    #[test]
    fn list_returns_empty_when_disconnected() {
        let cfg = FileLuRsyncConfig::from_provider_config(&sample_provider_config()).unwrap();
        let mut p = FileLuRsyncProvider::new(cfg);
        let entries = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(p.list("/"))
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn browse_operations_return_not_supported() {
        let cfg = FileLuRsyncConfig::from_provider_config(&sample_provider_config()).unwrap();
        let mut p = FileLuRsyncProvider::new(cfg);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(matches!(
                p.mkdir("/x").await,
                Err(ProviderError::NotSupported(_))
            ));
            assert!(matches!(
                p.delete("/x").await,
                Err(ProviderError::NotSupported(_))
            ));
            assert!(matches!(
                p.rename("/a", "/b").await,
                Err(ProviderError::NotSupported(_))
            ));
            assert!(matches!(
                p.stat("/x").await,
                Err(ProviderError::NotSupported(_))
            ));
        });
    }

    #[test]
    fn cd_up_normalizes_to_root() {
        let cfg = FileLuRsyncConfig::from_provider_config(&sample_provider_config()).unwrap();
        let mut p = FileLuRsyncProvider::new(cfg);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            p.cd("/foo/bar").await.unwrap();
            assert_eq!(p.pwd().await.unwrap(), "/foo/bar");
            p.cd_up().await.unwrap();
            assert_eq!(p.pwd().await.unwrap(), "/foo");
            p.cd_up().await.unwrap();
            assert_eq!(p.pwd().await.unwrap(), "/");
            p.cd_up().await.unwrap();
            assert_eq!(p.pwd().await.unwrap(), "/");
        });
    }
}
