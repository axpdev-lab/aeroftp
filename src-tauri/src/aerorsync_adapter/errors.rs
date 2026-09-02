//! Maps from the module's crate-owned carriers onto the application
//! error, statistics and capability types.
//!
//! Only this file knows the envelope format `delta_sync_rsync` re-reads,
//! and only this file decides which `RsyncError` variant a driver
//! failure becomes. The module hands over a typed carrier and the
//! commit flag it observed; the fallback policy that turns the two into
//! a verdict stays in the module, where the protocol knowledge is.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::aerorsync::fallback_policy::{classify_fallback, FallbackVerdict};
use crate::aerorsync::transport::TransportProbe;
use crate::aerorsync::types::{AerorsyncError, AerorsyncErrorKind, TransferError, TransferReport};
use crate::rsync_over_ssh::{RsyncCapability, RsyncError, RsyncStats};

/// Render a crate-owned [`TransferReport`] into the application
/// statistics type. Same arithmetic as the `build_stats` it replaces:
/// the speedup is the total size over the bytes actually sent, and 1.0
/// when nothing was sent (never a division by zero).
pub fn report_to_rsync_stats(report: TransferReport) -> RsyncStats {
    let speedup = if report.session.bytes_sent > 0 {
        report.total_size as f64 / report.session.bytes_sent as f64
    } else {
        1.0
    };
    RsyncStats {
        bytes_sent: report.session.bytes_sent,
        bytes_received: report.session.bytes_received,
        total_size: report.total_size,
        speedup,
        duration_ms: report.duration_ms,
        copy_blocks: report.session.copy_blocks,
        warnings: report.warnings,
    }
}

/// Render a crate-owned [`TransferError`] into the application error
/// type. Every string is the one the module produced, verbatim; only
/// the `Native` arm adds anything, and it adds the same envelope
/// `map_native_error_to_rsync` has always added.
pub fn to_rsync_error(err: TransferError) -> RsyncError {
    match err {
        TransferError::Soft { detail } => RsyncError::TransferFailed {
            exit: -1,
            stderr: detail,
        },
        TransferError::Hard { detail } => RsyncError::HardRejection(detail),
        TransferError::Io(io) => RsyncError::Io(io),
        TransferError::TooSmall { size, threshold } => RsyncError::TooSmall { size, threshold },
        TransferError::Native { error, committed } => map_native_error_to_rsync(error, committed),
    }
}

/// Translate a typed `AerorsyncError` into the production `RsyncError`
/// by consulting the fallback policy matrix. The resulting variant drives
/// downstream semantics through `delta_sync_rsync::transfer_with_delta`:
///
/// - `FallbackVerdict::Cancel` → `RsyncError::Cancelled` →
///   `DeltaSyncResult::fallback` (`transfer_with_delta` folds it into
///   the generic-fallback catch-all; the sync loop surfaces it as a
///   cancelled transfer).
/// - `FallbackVerdict::AttemptClassicSftpFallback` →
///   `RsyncError::TransferFailed { exit: -1, stderr: ... }` →
///   `DeltaSyncResult::fallback` → classic SFTP transparently.
/// - `FallbackVerdict::HardError` → `RsyncError::HardRejection(...)` →
///   `DeltaSyncResult::hard_error` → surfaced to the user, classic
///   fallback suppressed. This is the R4 solution.
fn map_native_error_to_rsync(err: AerorsyncError, committed: bool) -> RsyncError {
    match classify_fallback(&err, committed) {
        FallbackVerdict::Cancel => RsyncError::Cancelled,
        FallbackVerdict::AttemptClassicSftpFallback => RsyncError::TransferFailed {
            exit: -1,
            stderr: format!("native fallback ({:?}): {}", err.kind, err.detail),
        },
        FallbackVerdict::HardError => RsyncError::HardRejection(format!(
            "native hard rejection ({:?}): {}",
            err.kind, err.detail
        )),
    }
}

/// Render a failed probe as the application error.
///
/// The rule is the application's, not the module's, and it is kept
/// verbatim: a host key rejection is surfaced as a hard rejection so the
/// user sees why the native path refused, and everything else collapses
/// into "remote not available", which is the verdict the probe cache in
/// `delta_sync_rsync` memoises for five minutes.
pub fn probe_error_to_rsync(err: AerorsyncError) -> RsyncError {
    if err.kind == AerorsyncErrorKind::HostKeyRejected {
        return map_native_error_to_rsync(err, false);
    }
    RsyncError::RemoteNotAvailable
}

/// Render the module's probe result as the application capability
/// record. The banner and the negotiated protocol number are what the
/// probe cache in `delta_sync_rsync` memoises.
pub fn probe_to_capability(probe: TransportProbe) -> RsyncCapability {
    RsyncCapability {
        version: probe.remote_banner,
        protocol: probe.protocol.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::types::SessionStats;

    #[test]
    fn map_cancel_maps_to_rsync_cancelled_regardless_of_committed() {
        for committed in [false, true] {
            let err = AerorsyncError::cancelled("user abort");
            let rs = map_native_error_to_rsync(err, committed);
            assert!(
                matches!(rs, RsyncError::Cancelled),
                "committed={committed} → expected RsyncError::Cancelled, got {rs:?}"
            );
        }
    }

    #[test]
    fn map_pre_commit_environmental_errors_land_in_transfer_failed_minus_one() {
        let kinds = [
            AerorsyncErrorKind::UnsupportedVersion,
            AerorsyncErrorKind::NegotiationFailed,
            AerorsyncErrorKind::TransportFailure,
            AerorsyncErrorKind::RemoteError,
        ];
        for kind in kinds {
            let err = AerorsyncError::new(kind, "env");
            let rs = map_native_error_to_rsync(err, false);
            match rs {
                RsyncError::TransferFailed { exit, stderr } => {
                    assert_eq!(exit, -1, "pre-commit {kind:?} must use sentinel -1");
                    assert!(stderr.contains("native fallback"));
                    assert!(stderr.contains(&format!("{kind:?}")));
                }
                other => panic!("pre-commit {kind:?} → expected TransferFailed, got {other:?}"),
            }
        }
    }

    #[test]
    fn map_pre_commit_host_key_rejected_is_hard_rejection() {
        // R4 pin: HostKeyRejected MUST produce HardRejection even pre-commit,
        // so `transfer_with_delta` routes it to `hard_error` and the user
        // sees the failure: no silent classic fallback.
        let err = AerorsyncError::host_key_rejected("fingerprint mismatch");
        let rs = map_native_error_to_rsync(err, false);
        match rs {
            RsyncError::HardRejection(msg) => {
                assert!(msg.contains("HostKeyRejected"));
                assert!(msg.contains("fingerprint mismatch"));
            }
            other => panic!("expected HardRejection, got {other:?}"),
        }
    }

    #[test]
    fn map_probe_host_key_rejected_is_hard_rejection() {
        let err = AerorsyncError::host_key_rejected("probe fingerprint mismatch");
        let rs = probe_error_to_rsync(err);
        match rs {
            RsyncError::HardRejection(msg) => {
                assert!(msg.contains("HostKeyRejected"));
                assert!(msg.contains("probe fingerprint mismatch"));
            }
            other => panic!("probe HostKeyRejected must be hard, got {other:?}"),
        }
    }

    #[test]
    fn map_probe_environmental_error_is_remote_not_available() {
        let err = AerorsyncError::transport("rsync missing");
        let rs = probe_error_to_rsync(err);
        assert!(matches!(rs, RsyncError::RemoteNotAvailable));
    }

    #[test]
    fn map_post_commit_non_cancel_is_always_hard_rejection() {
        let kinds = [
            AerorsyncErrorKind::UnsupportedVersion,
            AerorsyncErrorKind::InvalidFrame,
            AerorsyncErrorKind::TransportFailure,
            AerorsyncErrorKind::NegotiationFailed,
            AerorsyncErrorKind::PlannerRejected,
            AerorsyncErrorKind::IllegalStateTransition,
            AerorsyncErrorKind::RemoteError,
            AerorsyncErrorKind::UnexpectedMessage,
            AerorsyncErrorKind::HostKeyRejected,
            AerorsyncErrorKind::Internal,
        ];
        for kind in kinds {
            let err = AerorsyncError::new(kind, "post-commit");
            let rs = map_native_error_to_rsync(err, true);
            match rs {
                RsyncError::HardRejection(msg) => {
                    assert!(
                        msg.contains(&format!("{kind:?}")),
                        "post-commit {kind:?} message missing kind tag: {msg}"
                    );
                }
                other => panic!("post-commit {kind:?} → expected HardRejection, got {other:?}"),
            }
        }
    }

    #[test]
    fn map_pre_commit_protocol_bugs_are_hard_rejection() {
        let kinds = [
            AerorsyncErrorKind::InvalidFrame,
            AerorsyncErrorKind::IllegalStateTransition,
            AerorsyncErrorKind::PlannerRejected,
            AerorsyncErrorKind::UnexpectedMessage,
            AerorsyncErrorKind::Internal,
        ];
        for kind in kinds {
            let err = AerorsyncError::new(kind, "proto-bug");
            let rs = map_native_error_to_rsync(err, false);
            match rs {
                RsyncError::HardRejection(_) => {}
                other => panic!("pre-commit {kind:?} → expected HardRejection, got {other:?}"),
            }
        }
    }

    #[test]
    fn build_stats_handles_zero_bytes_sent_without_div_by_zero() {
        let ss = SessionStats::default();
        let stats = report_to_rsync_stats(TransferReport {
            session: ss,
            total_size: 100,
            duration_ms: 50,
            warnings: vec!["w1".into()],
        });
        assert_eq!(stats.bytes_sent, 0);
        assert_eq!(stats.total_size, 100);
        assert_eq!(stats.speedup, 1.0);
        assert_eq!(stats.duration_ms, 50);
        assert_eq!(stats.warnings, vec!["w1".to_string()]);
    }

    #[test]
    fn build_stats_speedup_is_total_over_bytes_sent_when_nonzero() {
        let ss = SessionStats {
            bytes_sent: 25,
            bytes_received: 10,
            ..SessionStats::default()
        };
        let stats = report_to_rsync_stats(TransferReport {
            session: ss,
            total_size: 100,
            duration_ms: 200,
            warnings: Vec::new(),
        });
        assert!((stats.speedup - 4.0).abs() < 1e-9);
        assert_eq!(stats.bytes_sent, 25);
        assert_eq!(stats.bytes_received, 10);
    }

    /// Every string the module hands the application, pinned whole.
    ///
    /// The module now speaks [`TransferError`] and the boundary renders
    /// it. This is the only place the rendering is described, so the
    /// assertions are on entire strings, never on substrings, and the
    /// match is exhaustive by construction: a new variant stops
    /// compiling here first.
    #[test]
    fn to_rsync_error_is_exhaustive_and_keeps_every_string() {
        match to_rsync_error(TransferError::Soft {
            detail: "soft text".into(),
        }) {
            RsyncError::TransferFailed { exit, stderr } => {
                assert_eq!(exit, -1, "the soft envelope keeps the -1 sentinel");
                assert_eq!(stderr, "soft text");
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
        match to_rsync_error(TransferError::Hard {
            detail: "hard text".into(),
        }) {
            RsyncError::HardRejection(msg) => assert_eq!(msg, "hard text"),
            other => panic!("expected HardRejection, got {other:?}"),
        }
        match to_rsync_error(TransferError::Io(std::io::Error::from(
            std::io::ErrorKind::BrokenPipe,
        ))) {
            RsyncError::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::BrokenPipe),
            other => panic!("expected Io, got {other:?}"),
        }
        match to_rsync_error(TransferError::TooSmall {
            size: 7,
            threshold: 9,
        }) {
            RsyncError::TooSmall { size, threshold } => {
                assert_eq!((size, threshold), (7, 9));
            }
            other => panic!("expected TooSmall, got {other:?}"),
        }
        // Native, pre-commit: the fallback envelope, whole.
        match to_rsync_error(TransferError::Native {
            error: AerorsyncError::new(AerorsyncErrorKind::TransportFailure, "channel closed"),
            committed: false,
        }) {
            RsyncError::TransferFailed { exit, stderr } => {
                assert_eq!(exit, -1);
                assert_eq!(stderr, "native fallback (TransportFailure): channel closed");
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
        // Native, post-commit: the hard rejection envelope, whole.
        match to_rsync_error(TransferError::Native {
            error: AerorsyncError::new(AerorsyncErrorKind::TransportFailure, "channel closed"),
            committed: true,
        }) {
            RsyncError::HardRejection(msg) => {
                assert_eq!(
                    msg,
                    "native hard rejection (TransportFailure): channel closed"
                );
            }
            other => panic!("expected HardRejection, got {other:?}"),
        }
        // Native, cancelled: no envelope at all.
        assert!(matches!(
            to_rsync_error(TransferError::Native {
                error: AerorsyncError::new(AerorsyncErrorKind::Cancelled, "user asked to stop"),
                committed: false,
            }),
            RsyncError::Cancelled
        ));
        // The three atomic-write branches, whole strings.
        let soft = to_rsync_error(
            crate::aerorsync::delta_transport_impl::map_write_atomic_error(
                crate::aerorsync::streaming_writer::WriteAtomicError::PostOpen {
                    stage: "write",
                    source: std::io::Error::other("disk full"),
                },
            ),
        );
        match soft {
            RsyncError::TransferFailed { exit, stderr } => {
                assert_eq!(exit, -1);
                assert_eq!(
                    stderr,
                    "native fallback: atomic write failed at write (target untouched): disk full"
                );
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
        match to_rsync_error(
            crate::aerorsync::delta_transport_impl::map_write_atomic_error(
                crate::aerorsync::streaming_writer::WriteAtomicError::PostOpen {
                    stage: "acl",
                    source: std::io::Error::other("no acl"),
                },
            ),
        ) {
            RsyncError::HardRejection(msg) => {
                assert_eq!(msg, "atomic write failed at acl (target untouched): no acl");
            }
            other => panic!("expected HardRejection, got {other:?}"),
        }
        match to_rsync_error(
            crate::aerorsync::delta_transport_impl::map_write_atomic_error(
                crate::aerorsync::streaming_writer::WriteAtomicError::PostOpen {
                    stage: "rename",
                    source: std::io::Error::other("EXDEV"),
                },
            ),
        ) {
            RsyncError::HardRejection(msg) => {
                assert_eq!(msg, "atomic write failed at rename: EXDEV");
            }
            other => panic!("expected HardRejection, got {other:?}"),
        }
    }
}
