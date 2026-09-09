//! Typed S3 delta outcomes. These never enter the native rsync string classifier.
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::ProviderError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeltaOperation {
    Head,
    Create,
    Put,
    Copy,
    Complete,
}
impl DeltaOperation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Head => "HeadObject",
            Self::Create => "CreateMultipartUpload",
            Self::Put => "UploadPart",
            Self::Copy => "UploadPartCopy",
            Self::Complete => "CompleteMultipartUpload",
        }
    }
}

#[derive(Debug)]
pub(crate) enum S3DeltaError {
    SourceChanged,
    Provider(ProviderError),
    Response {
        status: u16,
        code: Option<String>,
        operation: DeltaOperation,
        message: String,
    },
}

impl From<ProviderError> for S3DeltaError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl S3DeltaError {
    pub(crate) fn into_provider_error(self) -> ProviderError {
        match self {
            Self::SourceChanged => ProviderError::TransferFailed("source_changed".into()),
            Self::Provider(error) => error,
            Self::Response { message, .. } => ProviderError::TransferFailed(message),
        }
    }

    /// Exact typed variants/status/XML Code values, never rendered-text needles.
    pub(crate) fn is_hard(&self) -> bool {
        match self {
            Self::Provider(
                ProviderError::AuthenticationFailed(_) | ProviderError::PermissionDenied(_),
            ) => true,
            Self::Response { status, code, .. } => {
                matches!(status, 401 | 403)
                    || matches!(
                        code.as_deref(),
                        Some(
                            "AccessDenied"
                                | "InvalidAccessKeyId"
                                | "SignatureDoesNotMatch"
                                | "ExpiredToken"
                                | "InvalidToken"
                                | "TokenRefreshRequired"
                        )
                    )
            }
            _ => false,
        }
    }

    pub(crate) fn fallback_reason(&self) -> &'static str {
        match self {
            Self::SourceChanged => "source_changed",
            Self::Response { status: 412, .. } => "etag_mismatch",
            Self::Response {
                code: Some(code), ..
            } if code == "PreconditionFailed" => "etag_mismatch",
            Self::Response {
                status: 501,
                operation: DeltaOperation::Copy,
                ..
            } => "backend_rejected_range_copy",
            Self::Response {
                code: Some(code),
                operation: DeltaOperation::Copy,
                ..
            } if matches!(
                code.as_str(),
                "NotImplemented" | "XNotImplemented" | "NotSupported" | "InvalidRange"
            ) =>
            {
                "backend_rejected_range_copy"
            }
            Self::Provider(ProviderError::NotConnected) => "not_connected",
            Self::Provider(ProviderError::IoError(_)) => "local_read_failed",
            _ => "s3_delta_failed",
        }
    }
}

#[derive(Debug)]
pub(crate) struct S3DeltaCompleted {
    pub wire_bytes: u64,
    pub etag: Option<String>,
    pub copy_parts: u64,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub(crate) enum S3DeltaOutcome {
    Refused(&'static str),
    Uploaded {
        wire_bytes: u64,
        total_size: u64,
        copy_parts: u64,
        duration_ms: u64,
    },
}

// Negative capability memory is small, expiring and account/endpoint-scoped.
// Only explicit copy-operation unsupported responses enter it.
static RANGE_REJECTIONS: std::sync::LazyLock<
    std::sync::Mutex<
        std::collections::HashMap<crate::transfer_dag::EndpointIdentity, std::time::Instant>,
    >,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
const RANGE_REJECTION_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const RANGE_REJECTION_LIMIT: usize = 128;

pub(crate) fn range_copy_rejected(identity: &crate::transfer_dag::EndpointIdentity) -> bool {
    let mut cache = RANGE_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|_, at| at.elapsed() < RANGE_REJECTION_TTL);
    cache.contains_key(identity)
}

pub(crate) fn remember_range_rejection(
    identity: crate::transfer_dag::EndpointIdentity,
    error: &S3DeltaError,
) {
    let unsupported = match error {
        S3DeltaError::Response {
            status: 501,
            operation: DeltaOperation::Copy,
            ..
        } => true,
        S3DeltaError::Response {
            status,
            code: Some(code),
            operation: DeltaOperation::Copy,
            ..
        } if (*status == 200 || (400..500).contains(status)) && !matches!(*status, 412 | 429) => {
            matches!(
                code.as_str(),
                "NotImplemented" | "XNotImplemented" | "NotSupported"
            )
        }
        _ => false,
    };
    if !unsupported || error.is_hard() {
        return;
    }
    let mut cache = RANGE_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|_, at| at.elapsed() < RANGE_REJECTION_TTL);
    if cache.len() >= RANGE_REJECTION_LIMIT {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(identity, std::time::Instant::now());
}

pub(crate) enum DeltaFailureDecision {
    Hard,
    Fallback(&'static str),
}

/// Single pure policy entry point, shared by adapter and capability memory.
pub(crate) fn classify_delta_failure(error: &S3DeltaError) -> DeltaFailureDecision {
    if error.is_hard() {
        DeltaFailureDecision::Hard
    } else {
        DeltaFailureDecision::Fallback(error.fallback_reason())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn failure(status: u16, code: &str) -> S3DeltaError {
        S3DeltaError::Response {
            status,
            code: Some(code.into()),
            operation: DeltaOperation::Copy,
            message: "native fallback (TransportFailure): internal misleading prose".into(),
        }
    }
    #[test]
    fn s3_adapter_classifier_uses_status_and_xml_code_not_prose() {
        for (status, code, hard, reason) in [
            (401, "", true, ""),
            (403, "", true, ""),
            (200, "AccessDenied", true, ""),
            (200, "ExpiredToken", true, ""),
            (200, "InvalidAccessKeyId", true, ""),
            (200, "SignatureDoesNotMatch", true, ""),
            (200, "TokenRefreshRequired", true, ""),
            (412, "PreconditionFailed", false, "etag_mismatch"),
            (200, "PreconditionFailed", false, "etag_mismatch"),
            (500, "InternalError", false, "s3_delta_failed"),
            (200, "InternalError", false, "s3_delta_failed"),
            (503, "SlowDown", false, "s3_delta_failed"),
            (400, "XNotImplemented", false, "backend_rejected_range_copy"),
        ] {
            match classify_delta_failure(&failure(status, code)) {
                DeltaFailureDecision::Hard => assert!(hard, "{status} {code}"),
                DeltaFailureDecision::Fallback(got) => {
                    assert!(!hard, "{status} {code}");
                    assert_eq!(got, reason);
                }
            }
        }
        assert!(matches!(
            classify_delta_failure(&ProviderError::Timeout.into()),
            DeltaFailureDecision::Fallback(_)
        ));
    }
    #[test]
    fn s3_adapter_negative_memory_does_not_cache_preconditions_or_transient_errors() {
        for (status, code) in [
            (412, "NotImplemented"),
            (429, "NotImplemented"),
            (500, "NotImplemented"),
            (503, "SlowDown"),
            (403, "NotImplemented"),
        ] {
            let identity = crate::transfer_dag::EndpointIdentity::new(
                "s3",
                format!("negative-test-{status}"),
                "test",
            );
            remember_range_rejection(identity.clone(), &failure(status, code));
            assert!(!range_copy_rejected(&identity));
        }
        let identity =
            crate::transfer_dag::EndpointIdentity::new("s3", "negative-test-supported", "test");
        remember_range_rejection(identity.clone(), &failure(400, "XNotImplemented"));
        assert!(range_copy_rejected(&identity));
        RANGE_REJECTIONS.lock().unwrap().insert(
            identity.clone(),
            std::time::Instant::now() - RANGE_REJECTION_TTL,
        );
        assert!(!range_copy_rejected(&identity));
    }
}
