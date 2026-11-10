//! MCP implementation of `ToolCtx` for the unified tool dispatcher.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
use crate::providers::{ProviderType, RemoteEntry};

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
            .filter_map(|p| ServerProfile::from_profile_json(&p))
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
        Ok(Arc::new(McpRemoteBackend::new(
            Arc::clone(&self.pool),
            server_id.to_string(),
        )))
    }

    fn surface(&self) -> Surfaces {
        Surfaces::MCP
    }
}

pub struct McpRemoteBackend {
    pool: Arc<ConnectionPool>,
    server: String,
    /// The connection identity the profile had when the call was approved:
    /// the pool reaches it only while it still connects there.
    expected_identity: Option<String>,
}

/// Heuristic: does this error look like an expired / invalid OAuth or auth
/// token rather than a transport or logic failure? Used to trigger a single
/// vault hot-reload retry (F5 #2): the long-running MCP process pools the
/// provider with the token captured at construction time, so once the desktop
/// app refreshes the token in the vault the pooled provider keeps presenting
/// the stale one. Dropping the pooled entry forces `create_provider_from_vault`
/// to re-read the fresh token from disk.
/// Is this failure the kind a fresh credential could fix?
///
/// It gates one hot-reload from the vault and a single retry, so the two
/// directions cost differently and that decides how wide it should be. A miss
/// means a user sees a failure that reloading would have repaired; a false
/// positive costs one wasted round trip. Wider is the cheap side here, which is
/// the opposite of most guards in this file.
///
/// UNDERSCORES ARE NORMALISED TO SPACES before matching, and that is the fix
/// rather than a tidy-up. `yandex_disk.rs` emits `invalid_token`; this list
/// carried `invalid token`, and one does not contain the other, so a revoked
/// Yandex token reached the pool and never triggered the reload that exists for
/// exactly that case. Adding the second spelling would have fixed one wording
/// and left the next one to be discovered; normalising means neither spelling
/// of any needle can be the one that was not thought of.
///
/// `revoked` belongs here even though a revoked token cannot be refreshed. The
/// retry does not refresh anything: it re-reads the vault, which the desktop
/// app may have repopulated by a full re-authentication. A revoked token is
/// therefore precisely a case where the stored credential may already be good
/// again. Note that `yandex_disk.rs` asks the OPPOSITE question of the same
/// words, whether an auth failure is terminal for its own retry loop, so the
/// two lists must not be merged however alike they look.
fn looks_like_auth_failure(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase().replace('_', " ");
    m.contains("401")
        || m.contains("unauthorized")
        || m.contains("access token")
        || m.contains("invalid grant")
        || m.contains("invalid token")
        || m.contains("invalid oauth")
        || m.contains("token is invalid")
        || m.contains("token not valid")
        || m.contains("revoked")
        || m.contains("token expired")
        || m.contains("expired token")
        || m.contains("authentication failed")
        || m.contains("requires a valid access token")
}

/// What a pooled call does after a provider error.
///
/// Auth reloads the vault credential and runs the call once more: an auth
/// failure means the server did not apply it. A transport failure removes
/// the pool entry and returns the error. The call is not run again, because
/// a write may already have reached the server. Every other failure,
/// including a missing path and a permission error, leaves the entry in place.
#[derive(Debug, PartialEq, Eq)]
enum PooledSessionAction {
    ReloadCredential,
    DropSession,
    KeepSession,
}

fn pooled_session_action(error: &crate::providers::ProviderError) -> PooledSessionAction {
    // Auth before transport. A dead-session variant whose text is an auth
    // failure still takes the one vault reload. `broken pipe` is not auth,
    // so it drops.
    if looks_like_auth_failure(&error.to_string()) {
        PooledSessionAction::ReloadCredential
    } else if crate::sync_core::scan::is_transport_level(error) {
        PooledSessionAction::DropSession
    } else {
        PooledSessionAction::KeepSession
    }
}

/// The provider call, or the pool's own failure to hand one over.
enum PooledAttempt<T> {
    Done(T),
    Provider(crate::providers::ProviderError),
    Pool(String),
}

impl McpRemoteBackend {
    /// A backend for `server` over `pool`: the MCP server's, and the CLI
    /// agent's over its own process-wide pool.
    pub(crate) fn new(pool: Arc<ConnectionPool>, server: String) -> Self {
        Self {
            pool,
            server,
            expected_identity: None,
        }
    }

    /// The same backend, bound to the identity its profile was approved with.
    pub(crate) fn expecting(mut self, identity: Option<String>) -> Self {
        self.expected_identity = identity;
        self
    }

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
            > + Clone,
    {
        // The provider lock and the pool Arc both end with this block, before
        // any invalidate. invalidate closes the socket only when this call is
        // the last holder.
        let first = match self
            .pool
            .get_provider_expecting(&self.server, self.expected_identity.as_deref())
            .await
        {
            Ok(arc) => {
                let mut guard = arc.lock().await;
                match op.clone()(&mut guard).await {
                    Ok(value) => PooledAttempt::Done(value),
                    // Classify the typed error before sanitize_api_error. A
                    // redacted string can hide the words the decision uses.
                    Err(error) => PooledAttempt::Provider(error),
                }
            }
            Err(error) => PooledAttempt::Pool(error),
        };

        match first {
            PooledAttempt::Done(value) => Ok(value),
            PooledAttempt::Provider(error) => match pooled_session_action(&error) {
                PooledSessionAction::ReloadCredential => self.reload_credential_and_retry(op).await,
                PooledSessionAction::DropSession => {
                    let _ = self.pool.invalidate(&self.server).await;
                    Err(crate::providers::sanitize_api_error(&error.to_string()))
                }
                PooledSessionAction::KeepSession => {
                    Err(crate::providers::sanitize_api_error(&error.to_string()))
                }
            },
            // No typed provider error: a connect that fails is not inserted.
            // An auth-shaped acquisition failure keeps today's one reload.
            PooledAttempt::Pool(error) if looks_like_auth_failure(&error) => {
                self.reload_credential_and_retry(op).await
            }
            PooledAttempt::Pool(error) => Err(error),
        }
    }

    /// Drop the pooled provider and run `op` once against a vault reload.
    ///
    /// F5 #2: the long-running process captured the token at construction.
    /// Dropping the entry makes the next `get_provider` re-read the vault,
    /// which the desktop app may already have refreshed. An auth failure
    /// means the operation did not take effect server-side, so this single
    /// retry is safe for a write. The caller has already released the
    /// provider lock. The retried call is not run again. A transport failure
    /// on that fresh session drops the entry. Another auth failure is not a
    /// second reload.
    async fn reload_credential_and_retry<T, F>(&self, op: F) -> Result<T, String>
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
        let _ = self.pool.invalidate(&self.server).await;
        let arc = self
            .pool
            .get_provider_expecting(&self.server, self.expected_identity.as_deref())
            .await?;
        let result = {
            let mut guard = arc.lock().await;
            op(&mut guard).await
        };
        self.finish_retried_call(arc, result).await
    }

    /// Settle the one auth retry. The call has already finished, and the
    /// provider lock is not held. A transport failure drops the fresh entry.
    /// The call is not run again.
    async fn finish_retried_call<T>(
        &self,
        arc: Arc<tokio::sync::Mutex<Box<dyn crate::providers::StorageProvider>>>,
        result: Result<T, crate::providers::ProviderError>,
    ) -> Result<T, String> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                drop(arc);
                if pooled_session_action(&error) == PooledSessionAction::DropSession {
                    let _ = self.pool.invalidate(&self.server).await;
                }
                Err(crate::providers::sanitize_api_error(&error.to_string()))
            }
        }
    }
}

#[async_trait]
impl RemoteBackend for McpRemoteBackend {
    async fn is_connected(&self) -> bool {
        self.pool
            .get_provider_expecting(&self.server, self.expected_identity.as_deref())
            .await
            .is_ok()
    }

    async fn provider_type(&self) -> Option<ProviderType> {
        self.provider_type_or_reason().await.ok()
    }

    async fn provider_type_or_reason(&self) -> Result<ProviderType, String> {
        // Cold acquisition can fail with stale credentials before the type is
        // available. Use the same vault reload and retry as the write itself.
        self.with_provider(|p| Box::pin(async move { Ok(p.provider_type()) }))
            .await
    }

    async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.list(&path).await }))
            .await
    }

    async fn list_for_display(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.list_for_display(&path).await }))
            .await
    }

    async fn stat(&self, path: &str) -> Result<RemoteEntry, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.stat(&path).await }))
            .await
    }

    async fn supports_upload_if_absent(&self) -> bool {
        self.with_provider(|p| Box::pin(async move { Ok(p.supports_upload_if_absent()) }))
            .await
            .unwrap_or(false)
    }
    async fn upload_if_absent(&self, local: &str, remote: &str) -> Result<bool, String> {
        let local = local.to_string();
        let remote = remote.to_string();
        self.with_provider(move |p| {
            Box::pin(async move {
                crate::providers::create_only_upload(p.as_mut(), &local, &remote).await
            })
        })
        .await
    }
    async fn listing_is_authoritative(&self) -> bool {
        self.with_provider(|p| Box::pin(async move { Ok(p.listing_is_authoritative()) }))
            .await
            .unwrap_or(false)
    }

    async fn download_to_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.download_to_bytes(&path).await }))
            .await
    }

    async fn download_to_bytes_capped(
        &self,
        path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, String> {
        let path = path.to_string();
        self.with_provider(move |p| {
            Box::pin(async move { p.download_to_bytes_capped(&path, max_bytes).await })
        })
        .await
    }

    async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String> {
        use std::io::Write;

        let mut tmp = tempfile::Builder::new()
            .prefix("aeroftp_mcp_upload_")
            .tempfile()
            .map_err(|e| e.to_string())?;
        tmp.write_all(data).map_err(|e| e.to_string())?;
        tmp.flush().map_err(|e| e.to_string())?;
        let tmp_str = tmp.path().to_string_lossy().to_string();
        let path = path.to_string();
        let result = self
            .with_provider(move |p| {
                let local = tmp_str.clone();
                Box::pin(async move { p.upload(&local, &path, None).await })
            })
            .await;
        result
    }

    async fn download(&self, remote: &str, local: &str) -> Result<(), String> {
        let remote = remote.to_string();
        let local = local.to_string();
        self.with_provider(move |p| {
            Box::pin(async move { p.download_checked(&remote, &local, None, None).await })
        })
        .await
    }

    async fn upload(&self, local: &str, remote: &str) -> Result<(), String> {
        let local = local.to_string();
        let remote = remote.to_string();
        self.with_provider(move |p| Box::pin(async move { p.upload(&local, &remote, None).await }))
            .await
    }

    async fn upload_bulk(
        &self,
        items: &[crate::providers::bulk::BulkTransferItem],
    ) -> Option<Result<Vec<crate::providers::bulk::BulkTransferOutcome>, String>> {
        // A refusal means no item reached the wire, so the single vault
        // reload and retry stays as safe here as it is for `upload`.
        let items = items.to_vec();
        self.with_provider(move |p| {
            Box::pin(async move {
                super::remote_backend::provider_upload_bulk(p.as_mut(), &items).await
            })
        })
        .await
        .transpose()
    }

    async fn delete(&self, path: &str) -> Result<(), String> {
        let path = path.to_string();
        self.with_provider(move |p| {
            let path = path.clone();
            Box::pin(async move {
                // Recursion is opt-in and lands in `delete_recursive`: a
                // directory is removed here only when it is empty, since
                // `rmdir` itself recurses on several backends.
                crate::providers::delete_non_recursive(p.as_mut(), &path).await
            })
        })
        .await
    }

    async fn delete_recursive(&self, path: &str) -> Result<(), String> {
        let path = path.to_string();
        self.with_provider(move |p| {
            let path = path.clone();
            Box::pin(async move { crate::providers::delete_recursive(p.as_mut(), &path).await })
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

    async fn replace(&self, from: &str, to: &str) -> Result<(), String> {
        let from = from.to_string();
        let to = to.to_string();
        self.with_provider(move |p| Box::pin(async move { p.replace(&from, &to).await }))
            .await
    }

    async fn supports_atomic_replace(&self) -> Result<bool, String> {
        self.with_provider(move |p| Box::pin(async move { p.supports_atomic_replace().await }))
            .await
    }

    async fn replace_sets_aside(&self) -> Result<bool, String> {
        self.with_provider(move |p| Box::pin(async move { Ok(p.replace_sets_aside()) }))
            .await
    }

    async fn supports_chmod(&self) -> Result<bool, String> {
        self.with_provider(move |p| Box::pin(async move { Ok(p.supports_chmod()) }))
            .await
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<(), String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.chmod(&path, mode).await }))
            .await
    }

    async fn search(&self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, String> {
        let path = path.to_string();
        let pattern = pattern.to_string();
        self.with_provider(move |p| {
            Box::pin(
                async move { crate::providers::find_or_walk(p.as_mut(), &path, &pattern).await },
            )
        })
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

    async fn transfer_capabilities(
        &self,
    ) -> Result<Option<crate::transfer_dag::TransferCapabilities>, String> {
        self.with_provider(move |p| Box::pin(async move { Ok(p.transfer_capabilities()) }))
            .await
            .map(Some)
    }

    async fn list_versions(
        &self,
        path: &str,
    ) -> Result<Vec<crate::providers::FileVersion>, String> {
        let path = path.to_string();
        self.with_provider(move |p| Box::pin(async move { p.list_versions(&path).await }))
            .await
    }

    async fn restore_version(&self, path: &str, version_id: &str) -> Result<(), String> {
        let path = path.to_string();
        let version_id = version_id.to_string();
        self.with_provider(move |p| {
            Box::pin(async move { p.restore_version(&path, &version_id).await })
        })
        .await
    }

    async fn delete_version(&self, path: &str, version_id: &str) -> Result<(), String> {
        let path = path.to_string();
        let version_id = version_id.to_string();
        self.with_provider(move |p| {
            Box::pin(async move { p.delete_version(&path, &version_id).await })
        })
        .await
    }

    async fn list_object_versions(
        &self,
        prefix: &str,
        include_noncurrent: bool,
    ) -> Result<Vec<crate::providers::TrashEntry>, String> {
        let prefix = prefix.to_string();
        self.with_provider(move |p| {
            Box::pin(async move { p.list_object_versions(&prefix, include_noncurrent).await })
        })
        .await
    }

    async fn empty_object_versions(
        &self,
        prefix: &str,
        include_noncurrent: bool,
        dry_run: bool,
    ) -> Result<(u64, u64), String> {
        let prefix = prefix.to_string();
        self.with_provider(move |p| {
            Box::pin(async move {
                p.empty_object_versions(&prefix, include_noncurrent, dry_run)
                    .await
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{looks_like_auth_failure, pooled_session_action, PooledSessionAction};
    use crate::providers::ProviderError;
    use crate::sync_core::scan::transport_failure_text;

    #[test]
    fn pooled_session_action_classifies_reload_drop_and_keep() {
        let drop_session = [
            ProviderError::ConnectionLost("idle".into()),
            ProviderError::NotConnected,
            ProviderError::IoError(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "broken pipe",
            )),
            ProviderError::Other("broken pipe".into()),
        ];
        for error in &drop_session {
            assert_eq!(
                pooled_session_action(error),
                PooledSessionAction::DropSession,
                "{error}"
            );
            assert!(
                !looks_like_auth_failure(&error.to_string()),
                "broken pipe and a dead session are not a vault reload: {error}"
            );
        }

        let keep_session = [
            ProviderError::NotFound("/reports/q3".into()),
            ProviderError::PermissionDenied("/reports/q3".into()),
            ProviderError::Other("404 Not Found".into()),
        ];
        for error in &keep_session {
            assert_eq!(
                pooled_session_action(error),
                PooledSessionAction::KeepSession,
                "{error}"
            );
            assert!(
                !transport_failure_text(&format!("failed to list /reports/q3: {error}")),
                "a missing path must not drop the check_tree session: {error}"
            );
        }

        // The strings auth_failure_classification already reloads. They stay
        // a credential reload, and the transport classifier does not claim them.
        for message in [
            "HTTP 401 Unauthorized",
            "Authentication failed: Unauthorized",
            "OAuth2 provider 'box' requires a valid access token",
            "invalid_grant",
            "the access token expired",
            "Box API: invalid token",
            "invalid_token",
            "OAuth token is invalid",
            "token not valid",
            "The OAuth token has been revoked",
            "invalid oauth token",
        ] {
            let error = ProviderError::Other(message.into());
            assert_eq!(
                pooled_session_action(&error),
                PooledSessionAction::ReloadCredential,
                "{message}"
            );
            assert!(
                !crate::sync_core::scan::is_transport_level(&error),
                "auth text is not a transport drop: {message}"
            );
        }

        let lost = ProviderError::ConnectionLost("idle".into());
        assert!(transport_failure_text(&format!(
            "failed to list /remote: {lost}"
        )));
        assert!(transport_failure_text(&format!(
            "failed to list /remote: {}",
            ProviderError::NotConnected
        )));
        assert!(transport_failure_text(
            "failed to list /remote: broken pipe"
        ));
        // The rendered Display of each typed transport variant. check_tree
        // only has this sentence, and it drops the session from it.
        assert!(transport_failure_text("failed to list /remote: Timeout"));
        assert!(transport_failure_text(
            "failed to list /remote: Connection failed: dns lookup failed"
        ));
        assert!(transport_failure_text(
            "failed to list /remote: Network error: reset"
        ));
        assert!(transport_failure_text(
            "failed to list /remote: IO error: closed"
        ));
        assert_eq!(
            pooled_session_action(&ProviderError::Timeout),
            PooledSessionAction::DropSession
        );
        // The scan's own time limit is not the Display word Timeout, even
        // when the directory name contains a transport phrase.
        assert!(!transport_failure_text(
            "timed out listing /remote after 30s"
        ));
        assert!(!transport_failure_text(
            "timed out listing /remote/connection closed after 30s"
        ));
        assert!(!transport_failure_text(
            "failed to list /reports/q3: Path not found: /reports/q3"
        ));
        assert!(!transport_failure_text("Permission denied"));
        // The word sits in the path. The Display is NotFound, so the session stays.
        assert!(!transport_failure_text(
            "failed to list /remote/Timeout: Path not found: /remote/Timeout"
        ));
        assert!(!transport_failure_text(
            "failed to list /remote/timeout backup: Permission denied: /remote/timeout backup"
        ));
    }

    #[test]
    fn auth_failure_classification() {
        // F5 #2: these must trigger the single vault hot-reload retry.
        for s in [
            "HTTP 401 Unauthorized",
            "Authentication failed: Unauthorized",
            "OAuth2 provider 'box' requires a valid access token",
            "invalid_grant",
            "the access token expired",
            "Box API: invalid token",
            // Every wording `yandex_disk.rs` produces for a dead token. None
            // of these matched before: `invalid_token` is not `invalid token`
            // once the underscore is in the way, and the two word orders below
            // are not the phrase this list knew either.
            "invalid_token",
            "OAuth token is invalid",
            "token not valid",
            "The OAuth token has been revoked",
            "invalid oauth token",
        ] {
            assert!(looks_like_auth_failure(s), "should be auth-class: {s}");
        }
        // These must NOT: transport/logic errors keep their own recovery path.
        for s in [
            "Data connection is already open",
            "broken pipe",
            "404 Not Found",
            "directory not found",
            "connection refused",
            // Widening for the underscore must not reach past auth: a expired
            // certificate is not a credential the vault can replace.
            "certificate expired",
            "the cached listing is invalid",
        ] {
            assert!(!looks_like_auth_failure(s), "should NOT be auth-class: {s}");
        }
    }

    /// The auth retry has already run. A transport error on that fresh session
    /// drops the inserted entry and does not call the operation again. An auth
    /// error and a missing path leave the entry. No vault is opened.
    #[tokio::test]
    async fn pooled_session_action_retry_transport_drops_the_fresh_entry() {
        use crate::providers::probe_fake::ProbeFake;
        use crate::providers::ProviderError;

        let pool = std::sync::Arc::new(crate::mcp::pool::ConnectionPool::new(
            4,
            std::time::Duration::from_secs(300),
        ));
        let backend =
            super::McpRemoteBackend::new(std::sync::Arc::clone(&pool), "probe".to_string());

        let arc = pool
            .insert_for_test("srv_probe", "probe", Box::new(ProbeFake::new()))
            .await;
        let dropped = backend
            .finish_retried_call(
                arc,
                Err::<(), _>(ProviderError::ConnectionLost("broken pipe".into())),
            )
            .await
            .unwrap_err();
        assert!(dropped.contains("Connection lost"), "{dropped}");
        assert!(pool.status().await.is_empty());

        let arc = pool
            .insert_for_test("srv_probe", "probe", Box::new(ProbeFake::new()))
            .await;
        let kept = backend
            .finish_retried_call(
                arc,
                Err::<(), _>(ProviderError::NotFound("/reports/q3".into())),
            )
            .await
            .unwrap_err();
        assert!(kept.contains("Path not found"), "{kept}");
        assert_eq!(pool.status().await.len(), 1);

        let arc = pool
            .insert_for_test("srv_auth", "auth", Box::new(ProbeFake::new()))
            .await;
        let auth = super::McpRemoteBackend::new(std::sync::Arc::clone(&pool), "auth".to_string());
        let reloaded = auth
            .finish_retried_call(
                arc,
                Err::<(), _>(ProviderError::Other("HTTP 401 Unauthorized".into())),
            )
            .await
            .unwrap_err();
        assert!(reloaded.contains("401"), "{reloaded}");
        assert_eq!(
            pool.status().await.len(),
            2,
            "a second auth failure is not another reload and does not drop the entry"
        );
    }
}
