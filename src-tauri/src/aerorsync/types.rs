//! Core types for the Strada C native rsync prototype.
//!
//! This is the stable vocabulary layer. Other modules depend on these shapes
//! but never reach back into protocol or transport concerns.

use std::fmt;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AerorsyncError {
    pub kind: AerorsyncErrorKind,
    pub detail: String,
}

impl AerorsyncError {
    pub fn new(kind: AerorsyncErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
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
    ///   non-terminal event is a programming bug — we do not panic
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
                         — caller should have filtered this via is_terminal()"
                    ),
                ),
            },
            other => Self::new(
                AerorsyncErrorKind::Internal,
                format!(
                    "from_oob_event called on non-terminal event {other:?} \
                     — caller should have filtered this via is_terminal()"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::events::AerorsyncEvent;

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
        // remote errors — the remote rsync never got to say anything.
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
        // policy — calling from_oob_event on it is a bug. Pin the fallback.
        let ev = AerorsyncEvent::Unknown {
            tag: 77,
            payload: vec![1, 2, 3],
        };
        let err = AerorsyncError::from_oob_event(&ev);
        assert_eq!(err.kind, AerorsyncErrorKind::Internal);
    }
}
