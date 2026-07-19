// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Typed transfer errors for the DAG executor and AIMD controller.
//!
//! DAG-P0-03 replaces `NodeOutcome::Failed(String)` with
//! [`TransferError`] so retryability, congestion class, cancel, and
//! `Retry-After` are machine-safe fields rather than substrings of a
//! presentation message. Substring classification lives only at the
//! adapter boundary ([`TransferError::from_provider`] /
//! [`TransferError::from_message`]); the controller matches on
//! [`TransferErrorKind`] alone.

use std::fmt;
use std::time::Duration;

use super::adaptive::parse_embedded_retry_after;
use crate::providers::ProviderError;

/// Machine-safe class of a transfer-node failure.
///
/// The AIMD controller maps only the congestion subset of these kinds to
/// [`super::adaptive::CongestionEvent`]; auth/not-found/cancel never shrink
/// concurrency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferErrorKind {
    /// HTTP 429 / explicit rate limit.
    RateLimited,
    /// HTTP 503 / service unavailable under load.
    ServiceUnavailable,
    /// Request or operation timed out (includes gateway timeout).
    Timeout,
    /// Server refused for too many connections (e.g. FTP 421).
    MaxConnections,
    /// Connection reset / dropped mid-flight.
    ConnectionReset,
    /// Other network / transport failure (not congestion by itself).
    Network,
    /// Authentication / credential failure.
    Auth,
    /// Path or object not found.
    NotFound,
    /// Permission / ACL denial.
    PermissionDenied,
    /// Quota or hard storage limit.
    QuotaExceeded,
    /// User or graph cancel.
    Cancelled,
    /// Local disk / IO failure.
    LocalIo,
    /// Remote / server failure that is not a congestion signal.
    RemoteIo,
    /// Provider session not connected.
    NotConnected,
    /// Resource lease could not be acquired.
    ResourceAcquire,
    /// Unclassified failure.
    Unknown,
}

/// Scope at which the failure is meaningful for policy (retry, abort, AIMD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FailureScope {
    /// One multipart / range part.
    Part,
    /// One file transfer.
    File,
    /// Endpoint / session.
    Endpoint,
    /// Whole graph / job.
    #[default]
    Job,
}

/// How (or whether) a caller should retry after this error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RetryDirective {
    /// Deterministic failure; never retry the same work.
    Never,
    /// Safe to retry immediately (idempotent, transient blip).
    Immediate,
    /// Use the engine default backoff.
    #[default]
    BackoffDefault,
    /// Wait until [`TransferError::retry_after`] (or default if absent).
    AfterHint,
}

/// Typed failure carried by [`super::executor::NodeOutcome::Failed`].
///
/// `message` is presentation-only (logs, GUI, CLI). Controllers must not
/// parse it: use `kind`, `retry`, `retry_after`, and `idempotent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferError {
    pub kind: TransferErrorKind,
    pub scope: FailureScope,
    pub retry: RetryDirective,
    pub retry_after: Option<Duration>,
    pub idempotent: bool,
    pub provider_code: Option<String>,
    pub message: String,
}

impl TransferError {
    /// Build an error with the given kind and presentation message.
    pub fn new(kind: TransferErrorKind, message: impl Into<String>) -> Self {
        let retry = default_retry_for(kind);
        Self {
            kind,
            scope: FailureScope::Job,
            retry,
            retry_after: None,
            idempotent: default_idempotent_for(kind),
            provider_code: None,
            message: message.into(),
        }
    }

    /// User/graph cancel with the stable presentation string used by the
    /// single-file and range paths.
    pub fn cancelled() -> Self {
        Self::new(TransferErrorKind::Cancelled, "Transfer cancelled by user")
    }

    /// Resource-manager acquire failure (node never entered its action).
    pub fn resource_acquire(message: impl Into<String>) -> Self {
        Self::new(TransferErrorKind::ResourceAcquire, message)
    }

    /// Synthetic / structural failure with only a presentation string.
    ///
    /// Classifies congestion and cancel keywords once at this boundary so the
    /// controller can stay substring-free. Prefer [`Self::from_provider`] when
    /// a [`ProviderError`] is available.
    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let retry_after = parse_embedded_retry_after(&message);
        let kind = classify_raw_message(&message);
        let mut err = Self::new(kind, message);
        err.retry_after = retry_after;
        if retry_after.is_some() && matches!(kind, TransferErrorKind::RateLimited) {
            err.retry = RetryDirective::AfterHint;
        }
        err
    }

    /// Adapter: map a provider error into a typed transfer error.
    ///
    /// Known [`ProviderError`] variants map by discriminant. String-bearing
    /// variants (e.g. `TransferFailed`, `ServerError`) are classified once
    /// here — including extraction of an embedded `Retry-After` marker —
    /// so the executor/AIMD path never re-parses text.
    pub fn from_provider(err: &ProviderError) -> Self {
        let message = err.to_string();
        let retry_after = parse_embedded_retry_after(&message);

        let kind = match err {
            ProviderError::Timeout => TransferErrorKind::Timeout,
            ProviderError::Cancelled => TransferErrorKind::Cancelled,
            ProviderError::NotConnected => TransferErrorKind::NotConnected,
            ProviderError::AuthenticationFailed(_) => TransferErrorKind::Auth,
            ProviderError::NotFound(_) => TransferErrorKind::NotFound,
            ProviderError::PermissionDenied(_) => TransferErrorKind::PermissionDenied,
            ProviderError::FileTooLarge(_) => TransferErrorKind::QuotaExceeded,
            ProviderError::RestrictedChar { .. } => TransferErrorKind::PermissionDenied,
            ProviderError::ReadOnly(_) => TransferErrorKind::PermissionDenied,
            ProviderError::InvalidPath(_) | ProviderError::InvalidConfig(_) => {
                TransferErrorKind::NotFound
            }
            ProviderError::AlreadyExists(_) | ProviderError::DirectoryNotEmpty(_) => {
                TransferErrorKind::RemoteIo
            }
            ProviderError::NotSupported(_) => TransferErrorKind::RemoteIo,
            ProviderError::ParseError(_) => TransferErrorKind::RemoteIo,
            ProviderError::IoError(_) => TransferErrorKind::LocalIo,
            ProviderError::ConnectionLost(_) => TransferErrorKind::ConnectionReset,
            ProviderError::ConnectionFailed(s) => classify_raw_message(s),
            ProviderError::NetworkError(s) => classify_network_message(s),
            ProviderError::TransferFailed(s)
            | ProviderError::ServerError(s)
            | ProviderError::Other(s)
            | ProviderError::Unknown(s) => classify_raw_message(s),
        };

        let mut te = Self::new(kind, message);
        te.retry_after = retry_after;
        if retry_after.is_some()
            && matches!(
                kind,
                TransferErrorKind::RateLimited | TransferErrorKind::ServiceUnavailable
            )
        {
            te.retry = RetryDirective::AfterHint;
        }
        te
    }

    /// Whether this failure is in the AIMD congestion trigger set (D2).
    pub fn is_congestion(&self) -> bool {
        matches!(
            self.kind,
            TransferErrorKind::RateLimited
                | TransferErrorKind::ServiceUnavailable
                | TransferErrorKind::Timeout
                | TransferErrorKind::MaxConnections
                | TransferErrorKind::ConnectionReset
        )
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransferError {}

impl From<ProviderError> for TransferError {
    fn from(err: ProviderError) -> Self {
        Self::from_provider(&err)
    }
}

fn default_retry_for(kind: TransferErrorKind) -> RetryDirective {
    match kind {
        TransferErrorKind::Cancelled
        | TransferErrorKind::Auth
        | TransferErrorKind::NotFound
        | TransferErrorKind::PermissionDenied
        | TransferErrorKind::QuotaExceeded
        | TransferErrorKind::ResourceAcquire => RetryDirective::Never,
        TransferErrorKind::RateLimited | TransferErrorKind::ServiceUnavailable => {
            RetryDirective::AfterHint
        }
        TransferErrorKind::Timeout
        | TransferErrorKind::MaxConnections
        | TransferErrorKind::ConnectionReset
        | TransferErrorKind::Network
        | TransferErrorKind::LocalIo
        | TransferErrorKind::RemoteIo
        | TransferErrorKind::NotConnected
        | TransferErrorKind::Unknown => RetryDirective::BackoffDefault,
    }
}

fn default_idempotent_for(kind: TransferErrorKind) -> bool {
    !matches!(
        kind,
        TransferErrorKind::Auth
            | TransferErrorKind::PermissionDenied
            | TransferErrorKind::QuotaExceeded
            | TransferErrorKind::NotFound
            | TransferErrorKind::Cancelled
    )
}

/// Classify a free-form error string once at the adapter boundary.
///
/// This is the only place that may inspect message text for transfer-DAG
/// scheduling decisions. The controller must use [`TransferErrorKind`].
fn classify_raw_message(raw: &str) -> TransferErrorKind {
    let lower = raw.to_lowercase();

    if lower.contains("cancelled by user")
        || lower.contains("transfer cancelled")
        || lower == "transfer cancelled"
    {
        return TransferErrorKind::Cancelled;
    }
    if lower.contains("503") || lower.contains("service unavailable") {
        return TransferErrorKind::ServiceUnavailable;
    }
    if lower.contains("too many connections")
        || lower.contains("max connections")
        || lower.contains("maximum number of connections")
        || lower.contains("421 ")
    {
        return TransferErrorKind::MaxConnections;
    }

    // Reuse the shared sync classifier for the bulk of the taxonomy, then
    // refine the Network bucket into connection-reset when applicable.
    let info = crate::sync::classify_sync_error(raw, None);
    match info.kind {
        crate::sync::SyncErrorKind::RateLimit => TransferErrorKind::RateLimited,
        crate::sync::SyncErrorKind::Timeout => TransferErrorKind::Timeout,
        crate::sync::SyncErrorKind::Auth => TransferErrorKind::Auth,
        crate::sync::SyncErrorKind::PathNotFound => TransferErrorKind::NotFound,
        crate::sync::SyncErrorKind::PermissionDenied => TransferErrorKind::PermissionDenied,
        crate::sync::SyncErrorKind::QuotaExceeded => TransferErrorKind::QuotaExceeded,
        crate::sync::SyncErrorKind::FileLocked => TransferErrorKind::RemoteIo,
        crate::sync::SyncErrorKind::DiskError => TransferErrorKind::LocalIo,
        crate::sync::SyncErrorKind::Network => classify_network_message(raw),
        crate::sync::SyncErrorKind::Unknown => TransferErrorKind::Unknown,
    }
}

fn classify_network_message(raw: &str) -> TransferErrorKind {
    let lower = raw.to_lowercase();
    if lower.contains("reset") {
        TransferErrorKind::ConnectionReset
    } else {
        TransferErrorKind::Network
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::adaptive::embed_retry_after_marker;

    #[test]
    fn from_provider_maps_variants_without_substring() {
        assert_eq!(
            TransferError::from_provider(&ProviderError::Timeout).kind,
            TransferErrorKind::Timeout
        );
        assert_eq!(
            TransferError::from_provider(&ProviderError::Cancelled).kind,
            TransferErrorKind::Cancelled
        );
        assert_eq!(
            TransferError::from_provider(&ProviderError::AuthenticationFailed("x".into())).kind,
            TransferErrorKind::Auth
        );
        assert_eq!(
            TransferError::from_provider(&ProviderError::NotFound("/a".into())).kind,
            TransferErrorKind::NotFound
        );
        assert_eq!(
            TransferError::from_provider(&ProviderError::NotConnected).kind,
            TransferErrorKind::NotConnected
        );
    }

    #[test]
    fn from_provider_extracts_retry_after_marker() {
        let marker = embed_retry_after_marker(45);
        let pe = ProviderError::TransferFailed(format!("HTTP 429 Too Many Requests{marker}"));
        let te = TransferError::from_provider(&pe);
        assert_eq!(te.kind, TransferErrorKind::RateLimited);
        assert_eq!(te.retry_after, Some(Duration::from_secs(45)));
        assert_eq!(te.retry, RetryDirective::AfterHint);
    }

    #[test]
    fn from_message_classifies_congestion_set() {
        assert_eq!(
            TransferError::from_message("HTTP 429 Too Many Requests").kind,
            TransferErrorKind::RateLimited
        );
        assert_eq!(
            TransferError::from_message("503 Service Unavailable").kind,
            TransferErrorKind::ServiceUnavailable
        );
        assert_eq!(
            TransferError::from_message("operation timed out after 30s").kind,
            TransferErrorKind::Timeout
        );
        assert_eq!(
            TransferError::from_message("421 too many connections from your IP").kind,
            TransferErrorKind::MaxConnections
        );
        assert_eq!(
            TransferError::from_message("connection reset by peer").kind,
            TransferErrorKind::ConnectionReset
        );
        assert_eq!(
            TransferError::from_message("504 Gateway Timeout").kind,
            TransferErrorKind::Timeout
        );
        assert_eq!(
            TransferError::from_message("500 Internal Server Error").kind,
            TransferErrorKind::Unknown
        );
        assert_eq!(
            TransferError::from_message("404 not found").kind,
            TransferErrorKind::NotFound
        );
        assert_eq!(
            TransferError::from_message("Transfer cancelled by user").kind,
            TransferErrorKind::Cancelled
        );
    }

    #[test]
    fn cancelled_helper_is_stable() {
        let e = TransferError::cancelled();
        assert_eq!(e.kind, TransferErrorKind::Cancelled);
        assert_eq!(e.message, "Transfer cancelled by user");
        assert_eq!(e.retry, RetryDirective::Never);
        assert!(!e.is_congestion());
    }
}
