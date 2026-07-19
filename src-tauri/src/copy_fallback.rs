// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Server-side copy with automatic download → upload fallback.
//!
//! Several providers advertise `server_copy` capability but reject specific
//! copy operations at runtime: S3 cross-bucket without proper IAM, WebDAV
//! servers returning 501/Method Not Allowed on cross-collection MOVE,
//! cloud APIs that allow same-folder copy but refuse cross-account moves.
//!
//! This module classifies those "soft" failures and degrades to a streaming
//! download-then-upload so the user-visible operation still succeeds. Hard
//! errors (auth, quota, source not found, cancellation) propagate untouched.
//!
//! Production GUI, CLI `cp`, and WebDAV-bridge `COPY` route through
//! `transfer_dag_single_file::execute_copy_dag`, which reuses
//! [`should_attempt_copy_fallback`] but exposes both payload legs as graph
//! nodes. [`server_side_copy_with_fallback`] remains behavior-compatible for
//! library callers and policy regression coverage.

use crate::providers::{ProviderError, StorageProvider};

/// Outcome of a copy attempt: either the provider's server-side copy
/// succeeded, or the fallback download-then-upload path was used.
///
/// Callers that want to surface the degradation to the user (CLI in
/// verbose mode, GUI log) can inspect this; callers that only care
/// about success can discard it.
#[derive(Debug, Clone)]
pub enum CopyOutcome {
    /// The provider's native server-side copy completed.
    ServerSide,
    /// Server-side copy was not attempted (provider does not advertise
    /// the capability) or failed with a recoverable error; the fallback
    /// download-then-upload path succeeded. `note` describes why the
    /// fast path was skipped (for logs / verbose CLI output).
    Fallback { note: String },
}

/// Classify a `server_copy` failure as eligible for the
/// download-then-upload fallback.
///
/// Eligible:
/// - `NotSupported`: the provider has the trait method but the specific
///   operation is rejected (S3 cross-bucket without IAM, Nextcloud MOVE
///   across shares, providers that gate by file type).
/// - `ServerError` / `Other` / `TransferFailed` whose message hints at a
///   capability boundary (HTTP 501, "method not allowed", "cross-bucket",
///   "cross-region", "cross-account", "not supported", "copy not"). Many
///   provider implementations swallow the HTTP status into one of these
///   variants instead of mapping to `NotSupported`.
///
/// NOT eligible — these propagate as-is because a fallback would silently
/// hide a real problem:
/// - `NotConnected`, `AuthenticationFailed`, `PermissionDenied`: would just
///   fail again on `download`.
/// - `NotFound`, `AlreadyExists`, `DirectoryNotEmpty`, `InvalidPath`:
///   semantic errors about the requested paths; download+upload would
///   reproduce them.
/// - `Cancelled`: user-initiated, never override.
/// - `Timeout`, `NetworkError`, `ConnectionLost`: transient transport
///   errors; the caller's own retry policy handles them.
/// - `IoError`, `ParseError`, `InvalidConfig`, `Unknown`: ambiguous;
///   prefer to surface them than to swallow them.
pub fn should_attempt_copy_fallback(err: &ProviderError) -> bool {
    use ProviderError::*;
    match err {
        NotSupported(_) => true,
        ServerError(msg) | Other(msg) | TransferFailed(msg) => {
            message_hints_capability_boundary(msg)
        }
        _ => false,
    }
}

fn message_hints_capability_boundary(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    [
        "not supported",
        "unsupported",
        "not allowed",
        "method not allowed",
        "cross-bucket",
        "cross bucket",
        "cross-region",
        "cross region",
        "cross-account",
        "cross account",
        "server-side copy",
        "server side copy",
        "copy not",
        " 501 ",
        "(501)",
        "http 501",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Stream the object through a local temp file: download from `from`,
/// upload to `to`, best-effort cleanup. Always exercises the provider's
/// own `download` and `upload` paths so retry / throttling / progress
/// observability stay identical to a normal pull-push.
///
/// The temp file lives in the OS temp dir; the caller is responsible for
/// having enough free space — large copies of >50 GiB cross-bucket may
/// fail at this step on small Docker tmpfs / runners.
pub async fn copy_via_download_upload(
    provider: &mut dyn StorageProvider,
    from: &str,
    to: &str,
) -> Result<(), ProviderError> {
    // `Builder::tempfile()` returns a handle that auto-deletes on drop,
    // but the provider needs a stable on-disk path it can write to: take
    // the path and let the file go out of scope, then run the transfer,
    // then remove the path on the way out.
    let temp = tempfile::Builder::new()
        .prefix("aeroftp-copy-")
        .tempfile()
        .map_err(ProviderError::IoError)?;
    let (_, temp_path) = temp.keep().map_err(|e| ProviderError::IoError(e.error))?;
    let temp_str = temp_path.to_string_lossy().into_owned();

    let result = async {
        provider.download(from, &temp_str, None).await?;
        provider.upload(&temp_str, to, None).await?;
        Ok::<(), ProviderError>(())
    }
    .await;

    let _ = std::fs::remove_file(&temp_path);
    result
}

/// Legacy compatibility helper: try native server-side copy and degrade to a
/// download-then-upload on a recoverable failure.
///
/// Production copy endpoints use `execute_copy_dag` so the decision and both
/// payload legs are observable. This helper intentionally shares the same
/// authoritative [`should_attempt_copy_fallback`] policy.
pub async fn server_side_copy_with_fallback(
    provider: &mut dyn StorageProvider,
    from: &str,
    to: &str,
) -> Result<CopyOutcome, ProviderError> {
    if provider.supports_server_copy() {
        match provider.server_copy(from, to).await {
            Ok(()) => return Ok(CopyOutcome::ServerSide),
            Err(e) if should_attempt_copy_fallback(&e) => {
                tracing::warn!(
                    target: "copy_fallback",
                    error = %e,
                    "server_copy failed with recoverable error, falling back to download→upload"
                );
                copy_via_download_upload(provider, from, to).await?;
                return Ok(CopyOutcome::Fallback {
                    note: format!("server_copy rejected: {}", e),
                });
            }
            Err(e) => return Err(e),
        }
    }

    copy_via_download_upload(provider, from, to).await?;
    Ok(CopyOutcome::Fallback {
        note: "provider does not advertise server_copy".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ProviderType, RemoteEntry, StorageProvider};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn err_other(msg: &str) -> ProviderError {
        ProviderError::Other(msg.to_string())
    }

    #[test]
    fn fallback_on_not_supported() {
        assert!(should_attempt_copy_fallback(&ProviderError::NotSupported(
            "server_copy".into()
        )));
    }

    #[test]
    fn fallback_on_capability_keyword_in_server_error() {
        assert!(should_attempt_copy_fallback(&ProviderError::ServerError(
            "HTTP 501 Not Implemented".into()
        )));
        assert!(should_attempt_copy_fallback(&ProviderError::ServerError(
            "Method Not Allowed".into()
        )));
        assert!(should_attempt_copy_fallback(&ProviderError::ServerError(
            "Cross-bucket copy requires explicit grant".into()
        )));
        assert!(should_attempt_copy_fallback(&ProviderError::ServerError(
            "S3 cross-region copy not allowed".into()
        )));
    }

    #[test]
    fn fallback_on_capability_keyword_in_other() {
        assert!(should_attempt_copy_fallback(&err_other(
            "server-side copy not supported across accounts"
        )));
    }

    #[test]
    fn no_fallback_on_auth_quota_or_missing() {
        assert!(!should_attempt_copy_fallback(
            &ProviderError::AuthenticationFailed("expired".into())
        ));
        assert!(!should_attempt_copy_fallback(
            &ProviderError::PermissionDenied("403".into())
        ));
        assert!(!should_attempt_copy_fallback(&ProviderError::NotFound(
            "/missing".into()
        )));
        assert!(!should_attempt_copy_fallback(
            &ProviderError::AlreadyExists("/dst".into())
        ));
        assert!(!should_attempt_copy_fallback(&ProviderError::NotConnected));
        assert!(!should_attempt_copy_fallback(&ProviderError::Cancelled));
        assert!(!should_attempt_copy_fallback(&ProviderError::InvalidPath(
            "bad".into()
        )));
    }

    #[test]
    fn no_fallback_on_transient_or_io_errors() {
        assert!(!should_attempt_copy_fallback(&ProviderError::Timeout));
        assert!(!should_attempt_copy_fallback(&ProviderError::NetworkError(
            "EHOSTUNREACH".into()
        )));
        assert!(!should_attempt_copy_fallback(
            &ProviderError::ConnectionLost("RST".into())
        ));
        assert!(!should_attempt_copy_fallback(&ProviderError::IoError(
            std::io::Error::other("disk full")
        )));
    }

    #[test]
    fn no_fallback_on_generic_server_error_without_keyword() {
        // A bare "500 Internal Server Error" is ambiguous — could be a
        // transient backend hiccup or an auth glitch — so do not silently
        // start streaming the file through the client. Let it surface.
        assert!(!should_attempt_copy_fallback(&ProviderError::ServerError(
            "500 Internal Server Error".into()
        )));
    }

    /// Minimal in-memory provider stub for the integration tests.
    /// Stores object bytes by remote path; `server_copy` behaviour is
    /// scripted per test via the `server_copy_outcome` slot.
    struct MockCopyProvider {
        supports_copy: bool,
        // Each call to `server_copy` consumes one value from the front
        // of the queue. If the queue is empty, server_copy returns Ok.
        server_copy_outcome: Mutex<Vec<Result<(), ProviderError>>>,
        download_count: Mutex<u32>,
        upload_count: Mutex<u32>,
        // (from, to) recorded for each server_copy that returned Ok or
        // erred at the entry, before any fallback ran.
        copy_calls: Mutex<Vec<(String, String)>>,
        // Simulated object store: path -> file contents.
        files: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl MockCopyProvider {
        fn new(supports_copy: bool, outcomes: Vec<Result<(), ProviderError>>) -> Self {
            let mut files = std::collections::HashMap::new();
            files.insert("/src.bin".to_string(), b"hello aeroftp".to_vec());
            Self {
                supports_copy,
                server_copy_outcome: Mutex::new(outcomes),
                download_count: Mutex::new(0),
                upload_count: Mutex::new(0),
                copy_calls: Mutex::new(Vec::new()),
                files: Mutex::new(files),
            }
        }
    }

    #[async_trait]
    impl StorageProvider for MockCopyProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::Ftp
        }
        fn display_name(&self) -> String {
            "mock-copy".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _p: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(vec![])
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            remote: &str,
            local: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            *self.download_count.lock().unwrap() += 1;
            let files = self.files.lock().unwrap();
            let data = files
                .get(remote)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote.to_string()))?;
            std::fs::write(local, &data).map_err(ProviderError::IoError)?;
            Ok(())
        }
        async fn download_to_bytes(&mut self, remote: &str) -> Result<Vec<u8>, ProviderError> {
            let files = self.files.lock().unwrap();
            files
                .get(remote)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote.to_string()))
        }
        async fn upload(
            &mut self,
            local: &str,
            remote: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            *self.upload_count.lock().unwrap() += 1;
            let data = std::fs::read(local).map_err(ProviderError::IoError)?;
            self.files.lock().unwrap().insert(remote.to_string(), data);
            Ok(())
        }
        async fn mkdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _f: &str, _t: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(&mut self, _p: &str) -> Result<RemoteEntry, ProviderError> {
            Err(ProviderError::NotSupported("stat".into()))
        }
        async fn size(&mut self, _p: &str) -> Result<u64, ProviderError> {
            Ok(0)
        }
        async fn exists(&mut self, _p: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("mock".into())
        }
        fn supports_server_copy(&self) -> bool {
            self.supports_copy
        }
        async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
            self.copy_calls
                .lock()
                .unwrap()
                .push((from.to_string(), to.to_string()));
            let mut q = self.server_copy_outcome.lock().unwrap();
            if q.is_empty() {
                // Default: succeed and propagate the file in our in-memory
                // store, so callers can assert dst exists.
                let mut files = self.files.lock().unwrap();
                if let Some(data) = files.get(from).cloned() {
                    files.insert(to.to_string(), data);
                }
                Ok(())
            } else {
                q.remove(0)
            }
        }
    }

    #[tokio::test]
    async fn server_side_path_used_when_supported_and_ok() {
        let mut p = MockCopyProvider::new(true, vec![]);
        let out = server_side_copy_with_fallback(&mut p, "/src.bin", "/dst.bin")
            .await
            .unwrap();
        assert!(matches!(out, CopyOutcome::ServerSide));
        assert_eq!(*p.download_count.lock().unwrap(), 0);
        assert_eq!(*p.upload_count.lock().unwrap(), 0);
        assert_eq!(p.copy_calls.lock().unwrap().len(), 1);
        assert!(p.files.lock().unwrap().contains_key("/dst.bin"));
    }

    #[tokio::test]
    async fn fallback_used_when_provider_does_not_support_server_copy() {
        let mut p = MockCopyProvider::new(false, vec![]);
        let out = server_side_copy_with_fallback(&mut p, "/src.bin", "/dst.bin")
            .await
            .unwrap();
        match out {
            CopyOutcome::Fallback { note } => assert!(note.contains("does not advertise")),
            CopyOutcome::ServerSide => panic!("expected fallback"),
        }
        assert_eq!(*p.download_count.lock().unwrap(), 1);
        assert_eq!(*p.upload_count.lock().unwrap(), 1);
        assert!(p.copy_calls.lock().unwrap().is_empty());
        assert!(p.files.lock().unwrap().contains_key("/dst.bin"));
    }

    #[tokio::test]
    async fn fallback_used_when_server_copy_returns_not_supported() {
        let mut p = MockCopyProvider::new(
            true,
            vec![Err(ProviderError::NotSupported("cross-bucket".into()))],
        );
        let out = server_side_copy_with_fallback(&mut p, "/src.bin", "/dst.bin")
            .await
            .unwrap();
        match out {
            CopyOutcome::Fallback { note } => assert!(note.contains("cross-bucket")),
            CopyOutcome::ServerSide => panic!("expected fallback"),
        }
        assert_eq!(*p.download_count.lock().unwrap(), 1);
        assert_eq!(*p.upload_count.lock().unwrap(), 1);
        assert_eq!(p.copy_calls.lock().unwrap().len(), 1);
        assert!(p.files.lock().unwrap().contains_key("/dst.bin"));
    }

    #[tokio::test]
    async fn fallback_used_on_recoverable_server_error() {
        let mut p = MockCopyProvider::new(
            true,
            vec![Err(ProviderError::ServerError(
                "HTTP 501 Not Implemented".into(),
            ))],
        );
        let out = server_side_copy_with_fallback(&mut p, "/src.bin", "/dst.bin")
            .await
            .unwrap();
        assert!(matches!(out, CopyOutcome::Fallback { .. }));
        assert_eq!(*p.download_count.lock().unwrap(), 1);
        assert_eq!(*p.upload_count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn auth_error_propagates_without_fallback() {
        let mut p = MockCopyProvider::new(
            true,
            vec![Err(ProviderError::AuthenticationFailed("expired".into()))],
        );
        let err = server_side_copy_with_fallback(&mut p, "/src.bin", "/dst.bin")
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));
        assert_eq!(*p.download_count.lock().unwrap(), 0);
        assert_eq!(*p.upload_count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn not_found_propagates_without_fallback() {
        let mut p =
            MockCopyProvider::new(true, vec![Err(ProviderError::NotFound("/missing".into()))]);
        let err = server_side_copy_with_fallback(&mut p, "/src.bin", "/dst.bin")
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::NotFound(_)));
        assert_eq!(*p.download_count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn generic_500_does_not_trigger_fallback() {
        let mut p = MockCopyProvider::new(
            true,
            vec![Err(ProviderError::ServerError(
                "500 Internal Server Error".into(),
            ))],
        );
        let err = server_side_copy_with_fallback(&mut p, "/src.bin", "/dst.bin")
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::ServerError(_)));
        assert_eq!(*p.download_count.lock().unwrap(), 0);
    }
}
