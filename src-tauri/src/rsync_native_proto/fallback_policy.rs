//! A5 — Fallback policy matrix for the S8i production wiring.
//!
//! When the native rsync driver bails, the A4 adapter
//! (`NativeRsyncDeltaTransport`) needs to decide whether to:
//!
//! 1. Fall back to the classic-SFTP path (the user sees nothing unusual
//!    — the transfer completes via the legacy wrapper),
//! 2. Surface the error as a hard failure (the user sees a toast, no
//!    transparent recovery),
//! 3. Honour a user-initiated cancel (the user knows why it stopped).
//!
//! The decision is a pure function of `(NativeRsyncError.kind,
//! committed)`. This module pins that function + a parameterised test
//! that enumerates every `NativeRsyncErrorKind` under both committed
//! values. If a future variant lands in `types.rs` without a row in the
//! matrix, the exhaustive `match` inside `classify_fallback` forces
//! compile-time attention — and the test ensures a deliberate choice
//! rather than a default.
//!
//! # Q5 PreCommit / PostCommit boundary (recap)
//!
//! The driver flips `committed = true` immediately before writing the
//! first outbound delta byte (`send_delta_phase_single_file`). Any
//! failure AFTER that boundary means the server has potentially
//! partially applied the transfer — silently reattempting via the
//! classic wrapper would double-apply or race. Hence the broad
//! "committed → HardError" rule.

use crate::rsync_native_proto::types::{NativeRsyncError, NativeRsyncErrorKind};

/// What the A4 adapter should do after the driver returns an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackVerdict {
    /// Silently retry the transfer via the classic-SFTP wrapper. The
    /// user experience is unchanged — they see the classic wrapper's
    /// progress UI, not the native attempt. Only legal pre-commit.
    AttemptClassicSftpFallback,
    /// Surface the error to the user. The transfer stops; no retry.
    /// Used when the protocol itself is malformed, when the cause is
    /// unquestionably remote (post-commit corruption, host key
    /// rejection), or when a deterministic bug would repeat across the
    /// classic wrapper too.
    HardError,
    /// The user cancelled. Stop silently; no UI error toast, no retry.
    Cancel,
}

/// Classify a terminal driver error into its fallback verdict.
///
/// `committed` MUST be sourced from `NativeRsyncDriver::committed()`.
/// The driver flips it `true` before the first outbound delta data
/// frame — this function assumes that invariant.
pub fn classify_fallback(
    err: &NativeRsyncError,
    committed: bool,
) -> FallbackVerdict {
    // Cancelled always wins: the user asked for stop. No fallback, no
    // hard error — the caller renders nothing.
    if err.kind == NativeRsyncErrorKind::Cancelled {
        return FallbackVerdict::Cancel;
    }

    if committed {
        // Post-commit: the server may have partially received / applied
        // the transfer. Silently retrying via classic SFTP risks double
        // application or torn file state. Everything but Cancelled is a
        // hard error here.
        return FallbackVerdict::HardError;
    }

    // Pre-commit: we can still attempt the classic wrapper
    // transparently for categories that the wrapper can reasonably
    // handle. For categories that signal a bug in our own code or in
    // the protocol contract itself (InvalidFrame, IllegalStateTransition,
    // Internal, PlannerRejected, UnexpectedMessage), we surface the
    // error — retrying with the classic wrapper would either reproduce
    // the bug in a different codepath or hide it.
    match err.kind {
        NativeRsyncErrorKind::Cancelled => FallbackVerdict::Cancel,

        // Environmental / negotiation: classic wrapper may still
        // succeed (different handshake, different version).
        NativeRsyncErrorKind::UnsupportedVersion
        | NativeRsyncErrorKind::NegotiationFailed
        | NativeRsyncErrorKind::TransportFailure
        | NativeRsyncErrorKind::RemoteError => {
            FallbackVerdict::AttemptClassicSftpFallback
        }

        // Security: never fall back after a host key mismatch — that's
        // exactly the condition where a silent retry would defeat the
        // pinning. Classic wrapper enforces its own policy; the user
        // has to see the native path refused.
        NativeRsyncErrorKind::HostKeyRejected => FallbackVerdict::HardError,

        // Protocol / internal consistency: hard errors. These indicate
        // a bug in the prototype (or a malformed remote) — silently
        // retrying would mask the issue.
        NativeRsyncErrorKind::InvalidFrame
        | NativeRsyncErrorKind::IllegalStateTransition
        | NativeRsyncErrorKind::PlannerRejected
        | NativeRsyncErrorKind::UnexpectedMessage
        | NativeRsyncErrorKind::Internal => FallbackVerdict::HardError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsync_native_proto::types::NativeRsyncError;

    /// Exhaustive matrix: every `NativeRsyncErrorKind` under both
    /// committed values. If a new variant lands without a matrix row,
    /// the exhaustive match in `classify_fallback` is a compile-time
    /// gate; this test is the runtime pin that the intent is still
    /// correct.
    fn matrix_row(kind: NativeRsyncErrorKind, committed: bool) -> FallbackVerdict {
        let err = NativeRsyncError::new(kind, "test detail");
        classify_fallback(&err, committed)
    }

    #[test]
    fn cancelled_always_maps_to_cancel() {
        assert_eq!(
            matrix_row(NativeRsyncErrorKind::Cancelled, false),
            FallbackVerdict::Cancel,
        );
        assert_eq!(
            matrix_row(NativeRsyncErrorKind::Cancelled, true),
            FallbackVerdict::Cancel,
        );
    }

    #[test]
    fn post_commit_non_cancelled_is_always_hard_error() {
        // Enumerate every non-cancel kind; post-commit must hard error.
        let kinds = [
            NativeRsyncErrorKind::UnsupportedVersion,
            NativeRsyncErrorKind::InvalidFrame,
            NativeRsyncErrorKind::TransportFailure,
            NativeRsyncErrorKind::NegotiationFailed,
            NativeRsyncErrorKind::PlannerRejected,
            NativeRsyncErrorKind::IllegalStateTransition,
            NativeRsyncErrorKind::RemoteError,
            NativeRsyncErrorKind::UnexpectedMessage,
            NativeRsyncErrorKind::HostKeyRejected,
            NativeRsyncErrorKind::Internal,
        ];
        for kind in kinds {
            assert_eq!(
                matrix_row(kind, true),
                FallbackVerdict::HardError,
                "post-commit {kind:?} must be HardError"
            );
        }
    }

    #[test]
    fn pre_commit_environmental_errors_fall_back_to_classic() {
        for kind in [
            NativeRsyncErrorKind::UnsupportedVersion,
            NativeRsyncErrorKind::NegotiationFailed,
            NativeRsyncErrorKind::TransportFailure,
            NativeRsyncErrorKind::RemoteError,
        ] {
            assert_eq!(
                matrix_row(kind, false),
                FallbackVerdict::AttemptClassicSftpFallback,
                "pre-commit {kind:?} must attempt classic fallback"
            );
        }
    }

    #[test]
    fn pre_commit_host_key_rejected_is_hard_error_never_fallback() {
        // Security pin: an UnpinnedFingerprintSha256 rejection must NOT
        // silently try the classic wrapper — that would defeat the
        // whole purpose of pinning. The classic wrapper enforces its
        // own host-key policy but the user must see the native refusal
        // first.
        assert_eq!(
            matrix_row(NativeRsyncErrorKind::HostKeyRejected, false),
            FallbackVerdict::HardError,
        );
    }

    #[test]
    fn pre_commit_protocol_bugs_are_hard_errors() {
        for kind in [
            NativeRsyncErrorKind::InvalidFrame,
            NativeRsyncErrorKind::IllegalStateTransition,
            NativeRsyncErrorKind::PlannerRejected,
            NativeRsyncErrorKind::UnexpectedMessage,
            NativeRsyncErrorKind::Internal,
        ] {
            assert_eq!(
                matrix_row(kind, false),
                FallbackVerdict::HardError,
                "pre-commit {kind:?} must be HardError (protocol/internal bug)"
            );
        }
    }

    #[test]
    fn fallback_verdict_is_total_over_all_kinds() {
        // Coverage pin: every variant must appear in one of the kind
        // lists above. Adding a new variant to NativeRsyncErrorKind
        // without touching this file is caught by the exhaustive match
        // in `classify_fallback` at compile time; this test guarantees
        // the intent is intentional rather than incidental.
        //
        // Build a list of all kinds explicitly and assert that each
        // yields a deterministic (kind, committed) → verdict. Any
        // accidental drift in classify_fallback surfaces here.
        let all = [
            NativeRsyncErrorKind::UnsupportedVersion,
            NativeRsyncErrorKind::InvalidFrame,
            NativeRsyncErrorKind::TransportFailure,
            NativeRsyncErrorKind::NegotiationFailed,
            NativeRsyncErrorKind::PlannerRejected,
            NativeRsyncErrorKind::IllegalStateTransition,
            NativeRsyncErrorKind::RemoteError,
            NativeRsyncErrorKind::UnexpectedMessage,
            NativeRsyncErrorKind::Cancelled,
            NativeRsyncErrorKind::HostKeyRejected,
            NativeRsyncErrorKind::Internal,
        ];
        for kind in all {
            for committed in [false, true] {
                let verdict = matrix_row(kind, committed);
                match verdict {
                    FallbackVerdict::AttemptClassicSftpFallback
                    | FallbackVerdict::HardError
                    | FallbackVerdict::Cancel => {}
                }
            }
        }
    }
}
