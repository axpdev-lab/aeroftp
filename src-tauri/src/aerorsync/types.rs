//! Core types for the Strada C native rsync prototype.
//!
//! This is the stable vocabulary layer. Other modules depend on these shapes
//! but never reach back into protocol or transport concerns.

use std::fmt;

use crate::aerorsync::fallback_policy::{classify_fallback, FallbackVerdict};

use serde::{Deserialize, Serialize};

/// rsync wire protocol version we target in the first native subset.
///
/// The prototype is intentionally pinned: wrapper captures show the field
/// `rsync version 3.2.7, protocol version 31`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProtocolVersion(pub u32);

impl ProtocolVersion {
    pub const CURRENT: Self = Self(31);
    pub const MIN_SUPPORTED: Self = Self(31);
    pub const MAX_SUPPORTED: Self = Self(31);

    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn is_supported(self) -> bool {
        self >= Self::MIN_SUPPORTED && self <= Self::MAX_SUPPORTED
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "protocol version {}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRole {
    Sender,
    Receiver,
}

impl SessionRole {
    pub fn as_flag_bit(self) -> u16 {
        match self {
            SessionRole::Sender => 0x0001,
            SessionRole::Receiver => 0x0002,
        }
    }

    pub fn is_remote_sender(self) -> bool {
        matches!(self, SessionRole::Sender)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferStrategy {
    Skip,
    FullCopy,
    Delta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureFlag {
    PreserveTimes,
    DeltaTransfer,
    IncrementalFileList,
    StructuredErrors,
    ResumeMarkers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AerorsyncConfig {
    pub protocol: ProtocolVersion,
    pub min_delta_file_size: u64,
    pub max_frame_size: usize,
    pub io_timeout_ms: u64,
    pub allow_compression: bool,
    pub allow_preserve_times: bool,
}

impl Default for AerorsyncConfig {
    fn default() -> Self {
        Self {
            protocol: ProtocolVersion::CURRENT,
            min_delta_file_size: 1_048_576,
            max_frame_size: 1024 * 1024,
            io_timeout_ms: 30_000,
            allow_compression: false,
            allow_preserve_times: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub modified_unix_secs: i64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStats {
    pub files_seen: u64,
    pub files_delta: u64,
    pub files_full_copy: u64,
    pub files_skipped: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub literal_bytes: u64,
    pub matched_bytes: u64,
    /// Number of baseline blocks reused by decoded or emitted CopyRun
    /// operations during this session.
    pub copy_blocks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AerorsyncErrorKind {
    UnsupportedVersion,
    InvalidFrame,
    TransportFailure,
    NegotiationFailed,
    PlannerRejected,
    IllegalStateTransition,
    /// The remote emitted a typed `WireMessage::Error` frame mid-session.
    /// The detail carries the remote-provided code and message verbatim.
    RemoteError,
    /// The remote emitted a message that is valid in isolation but is not
    /// allowed at this phase of the protocol (e.g. Summary before Hello).
    UnexpectedMessage,
    Cancelled,
    /// The remote SSH host key did not satisfy the active
    /// `SshHostKeyPolicy`. Never fall back to `AcceptAny` on failure.
    HostKeyRejected,
    Internal,
}

/// Machine-readable sub-classification carried by transport-produced
/// errors alongside [`AerorsyncErrorKind::TransportFailure`].
///
/// Y-RSC.2: the driver used to recognise a clean remote close by
/// substring-matching the error detail, so a transport rewording could
/// silently change protocol behaviour. Transports now stamp the class at
/// construction time and the driver matches on it structurally; the
/// human-readable `detail` text is free to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportErrorClass {
    /// The remote closed the byte stream cleanly: EOF with exit status 0
    /// on the libssh2 leg, or a scripted inbound exhaustion on the mock
    /// transports. Carries no indication of data loss by itself.
    CleanEof,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AerorsyncError {
    pub kind: AerorsyncErrorKind,
    pub detail: String,
    /// Structured transport sub-classification (Y-RSC.2). `None` for all
    /// non-transport errors and for transport failures with no special
    /// class. Serde-defaulted so payloads serialized before this field
    /// existed still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_class: Option<TransportErrorClass>,
}

impl AerorsyncError {
    pub fn new(kind: AerorsyncErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            transport_class: None,
        }
    }

    pub fn invalid_frame(detail: impl Into<String>) -> Self {
        Self::new(AerorsyncErrorKind::InvalidFrame, detail)
    }

    pub fn unsupported_version(detail: impl Into<String>) -> Self {
        Self::new(AerorsyncErrorKind::UnsupportedVersion, detail)
    }

    pub fn illegal_transition(detail: impl Into<String>) -> Self {
        Self::new(AerorsyncErrorKind::IllegalStateTransition, detail)
    }

    pub fn transport(detail: impl Into<String>) -> Self {
        Self::new(AerorsyncErrorKind::TransportFailure, detail)
    }

    /// Transport failure for a clean remote close (EOF, exit status 0).
    ///
    /// Same `kind` and `Display` output as [`AerorsyncError::transport`];
    /// only the machine-readable [`TransportErrorClass::CleanEof`] marker
    /// differs, so `kind`-based policies downstream (fallback matrix,
    /// teardown tolerance) are unaffected. The download driver matches on
    /// [`AerorsyncError::is_clean_transport_eof`] to decide the
    /// identical-baseline no-op path.
    pub fn transport_clean_eof(detail: impl Into<String>) -> Self {
        Self {
            transport_class: Some(TransportErrorClass::CleanEof),
            ..Self::new(AerorsyncErrorKind::TransportFailure, detail)
        }
    }

    /// True when this error is a transport-level clean EOF stamped by the
    /// producing transport via [`AerorsyncError::transport_clean_eof`].
    ///
    /// Classification is purely structural: the `detail` wording plays no
    /// role, so transports can reword messages without changing protocol
    /// behaviour.
    pub fn is_clean_transport_eof(&self) -> bool {
        self.kind == AerorsyncErrorKind::TransportFailure
            && self.transport_class == Some(TransportErrorClass::CleanEof)
    }

    pub fn remote(code: u16, message: impl Into<String>) -> Self {
        Self::new(
            AerorsyncErrorKind::RemoteError,
            format!("remote error {}: {}", code, message.into()),
        )
    }

    pub fn unexpected_message(detail: impl Into<String>) -> Self {
        Self::new(AerorsyncErrorKind::UnexpectedMessage, detail)
    }

    pub fn cancelled(detail: impl Into<String>) -> Self {
        Self::new(AerorsyncErrorKind::Cancelled, detail)
    }

    pub fn host_key_rejected(detail: impl Into<String>) -> Self {
        Self::new(AerorsyncErrorKind::HostKeyRejected, detail)
    }

    /// Translate a terminal out-of-band `AerorsyncEvent` into the
    /// matching typed error.
    ///
    /// Intended call site: the S8i real-wire driver, when its `EventSink`
    /// observes the first terminal event and must abort the session with
    /// a typed reason rather than a generic "transport failure".
    ///
    /// # Contract
    ///
    /// - The caller MUST ensure `event.is_terminal()` is `true`. Passing a
    ///   non-terminal event is a programming bug: we do not panic
    ///   (matches the "never crash prod" policy of `events.rs`) but we
    ///   fold the event into `Internal` with an explicit diagnostic so
    ///   the mistake surfaces in tests or logs.
    /// - Textual payload is preserved verbatim via the `detail` field so
    ///   the post-mortem logger / UI toast sees what rsync actually said.
    /// - `ErrorExit { Some(code != 0) }` produces a `RemoteError` with the
    ///   exit code rendered into the detail string; code 0 and empty
    ///   payload are non-terminal by policy and land in the `Internal`
    ///   fallback branch.
    pub fn from_oob_event(event: &crate::aerorsync::events::AerorsyncEvent) -> Self {
        use crate::aerorsync::events::AerorsyncEvent;
        match event {
            AerorsyncEvent::Error { message } => Self::new(
                AerorsyncErrorKind::RemoteError,
                format!("remote error: {message}"),
            ),
            AerorsyncEvent::ErrorXfer { message } => Self::new(
                AerorsyncErrorKind::RemoteError,
                format!("remote xfer error: {message}"),
            ),
            AerorsyncEvent::ErrorSocket { message } => Self::new(
                AerorsyncErrorKind::TransportFailure,
                format!("remote socket error: {message}"),
            ),
            AerorsyncEvent::ErrorExit { code } => match code {
                Some(c) if *c != 0 => Self::new(
                    AerorsyncErrorKind::RemoteError,
                    format!("remote rsync exited with code {c}"),
                ),
                _ => Self::new(
                    AerorsyncErrorKind::Internal,
                    format!(
                        "from_oob_event called on non-terminal ErrorExit({code:?}) \
                        : caller should have filtered this via is_terminal()"
                    ),
                ),
            },
            other => Self::new(
                AerorsyncErrorKind::Internal,
                format!(
                    "from_oob_event called on non-terminal event {other:?} \
                    : caller should have filtered this via is_terminal()"
                ),
            ),
        }
    }
}

impl fmt::Display for AerorsyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for AerorsyncError {}

/// Error kinds a reconnect can plausibly clear.
///
/// Single source of truth for the retry policy. The application-side
/// envelope classifier (`rsync_over_ssh::is_transient_native_envelope`)
/// derives its own needles from this list, so the two can never drift.
pub const TRANSIENT_KINDS: &[AerorsyncErrorKind] = &[
    AerorsyncErrorKind::TransportFailure,
    AerorsyncErrorKind::NegotiationFailed,
    AerorsyncErrorKind::UnsupportedVersion,
];

/// Detail substrings that veto a retry even when the kind is transient.
///
/// Copied verbatim from the application classifier this crate replaced.
/// Compared against the lowercased detail: a host key or fingerprint
/// mention, a protocol-bug marker, or a deterministic checksum
/// negotiation refusal means reconnecting would repeat the same failure.
pub const TRANSIENT_DENY_DETAILS: &[&str] = &[
    "hostkeyrejected",
    "host key",
    "fingerprint",
    "invalidframe",
    "illegalstatetransition",
    "plannerrejected",
    "unexpectedmessage",
    "internal",
    "remoteerror",
    "negotiation chose file checksum",
    "checksum negotiation found no common algorithm",
];

/// Detail substrings that make a soft failure worth one reconnect.
///
/// Copied verbatim from the application classifier this crate replaced,
/// where they described a dropped SSH channel on the classic rsync path.
/// They live here because the module's own streaming writer produces
/// them too: a soft detail carries the `Display` of a `std::io::Error`,
/// and a write to a network-backed target fails with "broken pipe",
/// "connection reset by peer" or "connection timed out" like any other
/// transport drop. Matching a substring is not the end state: the
/// structural error class that replaces it is Phase D work.
///
/// Applies to [`TransferError::Soft`] only, exactly as the application
/// applied it to `TransferFailed` only.
pub const TRANSIENT_SOFT_DETAILS: &[&str] = &[
    "connection reset by peer",
    "connection closed by remote",
    "broken pipe",
    "channel closed",
    "unexpected eof",
    "connection timed out",
    "network is unreachable",
];

/// Outcome carrier of every transfer entry point of the module.
///
/// The application adapter renders it into its own error type; the
/// module never names that type. Not `Clone` and not `Serialize`:
/// [`TransferError::Io`] owns a `std::io::Error`, which is neither.
/// [`AerorsyncError`] stays `Clone + Serialize` and travels inside
/// [`TransferError::Native`].
#[derive(Debug)]
pub enum TransferError {
    /// Nothing reached the destination and nothing was committed: the
    /// caller may retry through another transport. `detail` is
    /// user-visible verbatim.
    Soft { detail: String },
    /// Surface to the user, never retry silently. `detail` verbatim.
    Hard { detail: String },
    /// Local I/O before anything was touched.
    Io(std::io::Error),
    /// Below the caller's minimum size gate.
    TooSmall { size: u64, threshold: u64 },
    /// A driver error that still has to go through [`classify_fallback`]
    /// with the commit flag observed at the failure site.
    ///
    /// [`classify_fallback`]: crate::aerorsync::fallback_policy::classify_fallback
    Native {
        error: AerorsyncError,
        committed: bool,
    },
}

impl TransferError {
    /// Fallback verdict of a driver error, `None` for the other variants.
    ///
    /// The adapter needs the verdict to pick its own error variant; the
    /// module needs it to know that a cancelled transfer is never
    /// retried.
    pub fn verdict(&self) -> Option<FallbackVerdict> {
        match self {
            TransferError::Native { error, committed } => {
                Some(classify_fallback(error, *committed))
            }
            _ => None,
        }
    }

    /// Whether a batch may reconnect and retry this failure.
    ///
    /// Counter-intuitive but deliberate: `committed` does not enter the
    /// decision. The application classifier this replaced inspected the
    /// rendered envelope string, and both envelopes (the pre-commit
    /// `TransferFailed` one and the post-commit `HardRejection` one) went
    /// through the same kind allowlist and detail denylist, so a
    /// post-commit transport drop was retried exactly like a pre-commit
    /// one. The batch retry boundary is what makes that safe: every file
    /// gets its own temporary file and an atomic rename, so a retry never
    /// observes a partial commit. Only a cancelled transfer is excluded,
    /// because a cancel is a user decision, not a channel drop.
    pub fn is_transient_channel_drop(&self) -> bool {
        match self {
            TransferError::Io(io_err) => matches!(
                io_err.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
            ),
            TransferError::Native { error, .. } => {
                if self.verdict() == Some(FallbackVerdict::Cancel) {
                    return false;
                }
                if !TRANSIENT_KINDS.contains(&error.kind) {
                    return false;
                }
                let lower = error.detail.to_ascii_lowercase();
                !TRANSIENT_DENY_DETAILS
                    .iter()
                    .any(|needle| lower.contains(needle))
            }
            // A soft failure the module decided on its own. Its detail
            // often carries the `Display` of a `std::io::Error`, so a
            // transport drop on the local side reads exactly like one on
            // the wire: same needles, same answer as before.
            TransferError::Soft { detail } => {
                let lower = detail.to_ascii_lowercase();
                TRANSIENT_SOFT_DETAILS
                    .iter()
                    .any(|needle| lower.contains(needle))
            }
            // A hard rejection is never retried on its text: the
            // application only ever inspected the envelope tag on this
            // path, so a "broken pipe" inside a hard detail stays hard.
            // A size gate is not a failure to retry at all.
            TransferError::Hard { .. } | TransferError::TooSmall { .. } => false,
        }
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransferError::Soft { detail } => write!(f, "soft: {detail}"),
            TransferError::Hard { detail } => write!(f, "hard: {detail}"),
            TransferError::Io(io) => write!(f, "io: {io}"),
            TransferError::TooSmall { size, threshold } => {
                write!(f, "too small: {size} bytes, gate at {threshold}")
            }
            TransferError::Native { error, committed } => {
                write!(f, "native ({error}), committed={committed}")
            }
        }
    }
}

impl std::error::Error for TransferError {}

/// Everything a successful transfer entry point reports back.
///
/// The adapter turns it into the application statistics type; the module
/// only fills it.
#[derive(Debug, Clone, Default)]
pub struct TransferReport {
    pub session: SessionStats,
    pub total_size: u64,
    pub duration_ms: u64,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::events::AerorsyncEvent;

    const ALL_KINDS: [AerorsyncErrorKind; 11] = [
        AerorsyncErrorKind::UnsupportedVersion,
        AerorsyncErrorKind::InvalidFrame,
        AerorsyncErrorKind::TransportFailure,
        AerorsyncErrorKind::NegotiationFailed,
        AerorsyncErrorKind::PlannerRejected,
        AerorsyncErrorKind::IllegalStateTransition,
        AerorsyncErrorKind::RemoteError,
        AerorsyncErrorKind::UnexpectedMessage,
        AerorsyncErrorKind::Cancelled,
        AerorsyncErrorKind::HostKeyRejected,
        AerorsyncErrorKind::Internal,
    ];

    /// The retry decision, case by case, inherited from the application
    /// classifier this predicate replaced.
    ///
    /// `rsync_over_ssh::is_transient_for_reconnect` reached its verdict by
    /// re-reading the rendered envelope string, and 29 tests pinned it:
    /// 17 on the two envelope shapes and 12 on the plain variants. It is
    /// gone; every one of its cases is transcribed here against the
    /// carrier that produced the string in the first place, with the
    /// expected answer written out rather than computed, so the table
    /// reads as the policy and not as a second implementation of it.
    ///
    /// Two of its cases have no counterpart on this side, and that is the
    /// point of the type: an unknown kind label ("SomeBrandNewKind") is
    /// unrepresentable, because the kind is an enum and a new one has to
    /// be added to `TRANSIENT_KINDS` deliberately, and an authentication
    /// error like a missing key never leaves a transfer entry point.
    #[test]
    fn transient_channel_drop_cases_from_the_retired_app_classifier() {
        fn native(kind: AerorsyncErrorKind, detail: &str, committed: bool) -> TransferError {
            TransferError::Native {
                error: AerorsyncError::new(kind, detail),
                committed,
            }
        }

        // The post-commit envelope, once `HardRejection`.
        let post_commit: [(AerorsyncErrorKind, &str, bool); 8] = [
            (
                AerorsyncErrorKind::TransportFailure,
                "next_data_frame: remote closed mid file list",
                true,
            ),
            (AerorsyncErrorKind::NegotiationFailed, "kex blip", true),
            (
                AerorsyncErrorKind::NegotiationFailed,
                "negotiation chose file checksum \"none\", which this client does not implement; falling back",
                false,
            ),
            (
                AerorsyncErrorKind::HostKeyRejected,
                "fingerprint mismatch",
                false,
            ),
            (
                AerorsyncErrorKind::InvalidFrame,
                "unexpected opcode 0x42",
                false,
            ),
            // Stands in for the retired "unknown kind label" case: a kind
            // outside the transient list is never retried.
            (AerorsyncErrorKind::PlannerRejected, "mystery", false),
            // The denylist wins even under a transient kind.
            (
                AerorsyncErrorKind::TransportFailure,
                "host key verification failed",
                false,
            ),
            // Mixed case in the detail: the match lowercases first.
            (
                AerorsyncErrorKind::TransportFailure,
                "Remote Closed",
                true,
            ),
        ];
        // The pre-commit envelope, once `TransferFailed { exit: -1 }`.
        let pre_commit: [(AerorsyncErrorKind, &str, bool); 9] = [
            (
                AerorsyncErrorKind::TransportFailure,
                "next_data_frame: remote closed mid file list",
                true,
            ),
            (
                AerorsyncErrorKind::TransportFailure,
                "russh channel_open_session: Channel send error",
                true,
            ),
            (
                AerorsyncErrorKind::NegotiationFailed,
                "initial handshake blip",
                true,
            ),
            (
                AerorsyncErrorKind::NegotiationFailed,
                "negotiation chose file checksum \"none\", which this client does not implement; falling back",
                false,
            ),
            (
                AerorsyncErrorKind::NegotiationFailed,
                "checksum negotiation found no common algorithm (client \"sha1\" vs server \"md5\"); falling back",
                false,
            ),
            (
                AerorsyncErrorKind::HostKeyRejected,
                "fingerprint mismatch",
                false,
            ),
            (
                AerorsyncErrorKind::InvalidFrame,
                "unexpected opcode 0xff",
                false,
            ),
            (AerorsyncErrorKind::PlannerRejected, "mystery", false),
            (
                AerorsyncErrorKind::TransportFailure,
                "Channel Send Error",
                true,
            ),
        ];
        let mut checked = 0usize;
        for (kind, detail, expected) in post_commit {
            assert_eq!(
                native(kind, detail, true).is_transient_channel_drop(),
                expected,
                "post-commit {kind:?} {detail:?}"
            );
            checked += 1;
        }
        for (kind, detail, expected) in pre_commit {
            assert_eq!(
                native(kind, detail, false).is_transient_channel_drop(),
                expected,
                "pre-commit {kind:?} {detail:?}"
            );
            checked += 1;
        }

        // The plain variants, once `Io`, `TransferFailed` without an
        // envelope, a bare `HardRejection`, and `Cancelled`.
        for (io_kind, expected) in [
            (std::io::ErrorKind::BrokenPipe, true),
            (std::io::ErrorKind::ConnectionReset, true),
            (std::io::ErrorKind::UnexpectedEof, true),
            (std::io::ErrorKind::TimedOut, true),
            (std::io::ErrorKind::WouldBlock, true),
            (std::io::ErrorKind::ConnectionAborted, true),
            (std::io::ErrorKind::PermissionDenied, false),
            (std::io::ErrorKind::NotFound, false),
        ] {
            assert_eq!(
                TransferError::Io(std::io::Error::from(io_kind)).is_transient_channel_drop(),
                expected,
                "io {io_kind:?}"
            );
            checked += 1;
        }
        for (detail, expected) in [
            ("rsync error: connection reset by peer\n", true),
            ("rsync error: Permission denied (publickey)\n", false),
            ("Host key verification failed.\n", false),
            ("", false),
            ("Connection Reset By Peer", true),
            (
                "native fallback: atomic write failed at write (target untouched): broken pipe",
                true,
            ),
        ] {
            assert_eq!(
                TransferError::Soft {
                    detail: detail.into()
                }
                .is_transient_channel_drop(),
                expected,
                "soft {detail:?}"
            );
            checked += 1;
        }
        // A hard rejection is never retried on its text, even when the
        // text is one that would make a soft failure transient.
        for detail in [
            "host key mismatch",
            "broken pipe",
            "connection reset by peer",
        ] {
            assert!(
                !TransferError::Hard {
                    detail: detail.into()
                }
                .is_transient_channel_drop(),
                "hard {detail:?}"
            );
            checked += 1;
        }
        assert!(
            !native(AerorsyncErrorKind::Cancelled, "user abort", false).is_transient_channel_drop(),
            "a cancel is a decision, not a drop"
        );
        assert!(
            !TransferError::TooSmall {
                size: 1,
                threshold: 2
            }
            .is_transient_channel_drop(),
            "a size gate is not a failure to retry"
        );
        checked += 2;

        assert_eq!(
            checked, 36,
            "the inherited case table shrank: {checked} cases checked"
        );
    }

    /// The carrier does not re-decide the fallback policy: it asks
    /// `classify_fallback`, for every kind and both commit states.
    #[test]
    fn transfer_error_verdict_matches_classify_fallback() {
        for kind in ALL_KINDS {
            for committed in [false, true] {
                let error = AerorsyncError::new(kind, "detail");
                let carried = TransferError::Native {
                    error: error.clone(),
                    committed,
                };
                assert_eq!(
                    carried.verdict(),
                    Some(classify_fallback(&error, committed)),
                    "verdict drifted for {kind:?} committed={committed}"
                );
            }
        }
        assert_eq!(TransferError::Soft { detail: "s".into() }.verdict(), None);
        assert_eq!(TransferError::Hard { detail: "h".into() }.verdict(), None);
        assert_eq!(
            TransferError::TooSmall {
                size: 1,
                threshold: 2
            }
            .verdict(),
            None
        );
        assert_eq!(
            TransferError::Io(std::io::Error::other("x")).verdict(),
            None
        );
    }

    #[test]
    fn from_oob_event_error_maps_to_remote_error_with_message() {
        let ev = AerorsyncEvent::Error {
            message: "boom".to_string(),
        };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::RemoteError);
        assert!(err.detail.contains("boom"));
    }

    #[test]
    fn from_oob_event_error_xfer_maps_to_remote_error() {
        let ev = AerorsyncEvent::ErrorXfer {
            message: "xfer".into(),
        };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::RemoteError);
        assert!(err.detail.contains("xfer"));
    }

    #[test]
    fn from_oob_event_error_socket_maps_to_transport_failure() {
        // Socket-level failures are transport failures, not semantic
        // remote errors: the remote rsync never got to say anything.
        let ev = AerorsyncEvent::ErrorSocket {
            message: "conn reset".into(),
        };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::TransportFailure);
        assert!(err.detail.contains("conn reset"));
    }

    #[test]
    fn from_oob_event_error_exit_nonzero_carries_code() {
        let ev = AerorsyncEvent::ErrorExit { code: Some(23) };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::RemoteError);
        assert!(err.detail.contains("23"), "missing code: {}", err.detail);
    }

    #[test]
    fn from_oob_event_error_exit_zero_is_caller_bug_falls_to_internal() {
        let ev = AerorsyncEvent::ErrorExit { code: Some(0) };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::Internal);
        assert!(err.detail.contains("non-terminal"));
    }

    #[test]
    fn from_oob_event_non_terminal_warning_is_caller_bug_falls_to_internal() {
        let ev = AerorsyncEvent::Warning {
            message: "w".into(),
        };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::Internal);
    }

    #[test]
    fn from_oob_event_unknown_is_caller_bug_falls_to_internal() {
        // A future opcode we do not recognise is NOT terminal per events.rs
        // policy: calling from_oob_event on it is a bug. Pin the fallback.
        let ev = AerorsyncEvent::Unknown {
            tag: 77,
            payload: vec![1, 2, 3],
        };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::Internal);
    }

    #[test]
    fn transport_clean_eof_sets_structured_class_and_keeps_display() {
        // Y-RSC.2: the clean-EOF constructor differs from the plain one
        // only by the machine-readable class; kind and Display stay
        // byte-identical so kind-based policies and log texts are
        // unchanged.
        let clean = AerorsyncError::transport_clean_eof("remote closed (exit 0): bye");
        let plain = AerorsyncError::transport("remote closed (exit 0): bye");
        assert_eq!(clean.kind, AerorsyncErrorKind::TransportFailure);
        assert!(clean.is_clean_transport_eof());
        assert_eq!(clean.transport_class, Some(TransportErrorClass::CleanEof));
        assert_eq!(clean.to_string(), plain.to_string());
    }

    #[test]
    fn plain_transport_error_with_clean_eof_wording_is_not_classified() {
        // The historical magic substrings must carry no weight: only the
        // structural marker classifies. This pins "structure, not text".
        for detail in [
            "read_bytes: remote closed (exit 0): ",
            "mock raw inbound exhausted: simulated remote close",
        ] {
            let err = AerorsyncError::transport(detail);
            assert!(
                !err.is_clean_transport_eof(),
                "wording alone must never classify: {detail}"
            );
        }
    }

    #[test]
    fn non_transport_kinds_never_classify_as_clean_eof() {
        assert!(!AerorsyncError::cancelled("stop").is_clean_transport_eof());
        assert!(!AerorsyncError::invalid_frame("bad").is_clean_transport_eof());
    }

    #[test]
    fn error_without_transport_class_field_still_deserializes() {
        // Backward compatibility: payloads serialized before Y-RSC.2 lack
        // the `transport_class` field and must decode to `None`.
        let json = r#"{"kind":"TransportFailure","detail":"x"}"#;
        let err: AerorsyncError = serde_json::from_str(json).expect("legacy payload decodes");
        assert_eq!(err.kind, AerorsyncErrorKind::TransportFailure);
        assert_eq!(err.transport_class, None);
        assert!(!err.is_clean_transport_eof());
    }

    #[test]
    fn clean_eof_class_survives_serde_round_trip() {
        let err = AerorsyncError::transport_clean_eof("peer done");
        let json = serde_json::to_string(&err).expect("serialize");
        let back: AerorsyncError = serde_json::from_str(&json).expect("deserialize");
        assert!(back.is_clean_transport_eof());
        assert_eq!(back.detail, "peer done");
    }
}
