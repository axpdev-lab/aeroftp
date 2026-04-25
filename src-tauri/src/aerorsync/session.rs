//! Session state machine for the Strada C native rsync prototype.
//!
//! This module owns state transitions only. Protocol framing lives in
//! `protocol.rs`; transport I/O lives in `transport.rs`; decisions on
//! skip/full/delta live in `planner.rs`.

use crate::aerorsync::protocol::HelloMessage;
use crate::aerorsync::transport::RemoteShellTransport;
use crate::aerorsync::types::{
    AerorsyncConfig, AerorsyncError, FeatureFlag, ProtocolVersion, SessionRole, SessionStats,
};

/// Explicit phase enum for the native session state machine.
///
/// Matches the 9-step minimal state machine documented in the wrapper
/// transcript baseline (section 7):
///   Created → Probed → Negotiated → FileListPrepared → Transferring →
///   Finalized (or Cancelled, or Failed at any active phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Created,
    Probed,
    Negotiated,
    FileListPrepared,
    Transferring,
    Finalized,
    Cancelled,
    Failed,
}

impl SessionState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            SessionState::Finalized | SessionState::Cancelled | SessionState::Failed
        )
    }

    /// Legal forward transitions. Terminal states have no forward moves.
    /// Cancel/Fail are permitted from any non-terminal state via explicit
    /// `cancel` / `fail` methods, not through `transition_to`.
    pub fn legal_next(self) -> &'static [SessionState] {
        use SessionState::*;
        match self {
            Created => &[Probed],
            Probed => &[Negotiated],
            Negotiated => &[FileListPrepared],
            FileListPrepared => &[Transferring],
            Transferring => &[Finalized],
            Finalized | Cancelled | Failed => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedSession {
    pub protocol: ProtocolVersion,
    pub role: SessionRole,
    pub features: Vec<FeatureFlag>,
    pub remote_banner: String,
}

pub struct AerorsyncSession<T: RemoteShellTransport> {
    pub transport: T,
    pub config: AerorsyncConfig,
    pub state: SessionState,
    pub negotiated: Option<NegotiatedSession>,
    pub stats: SessionStats,
}

impl<T: RemoteShellTransport> AerorsyncSession<T> {
    pub fn new(transport: T, config: AerorsyncConfig) -> Self {
        Self {
            transport,
            config,
            state: SessionState::Created,
            negotiated: None,
            stats: SessionStats::default(),
        }
    }

    /// Validated forward transition. Returns `IllegalStateTransition` for any
    /// move that is not in `legal_next`. Terminal transitions (cancel / fail)
    /// are handled by `cancel` and `fail` instead.
    pub fn transition_to(&mut self, next: SessionState) -> Result<(), AerorsyncError> {
        if self.state.is_terminal() {
            return Err(AerorsyncError::illegal_transition(format!(
                "{:?} is terminal; cannot move to {:?}",
                self.state, next
            )));
        }
        if !self.state.legal_next().contains(&next) {
            return Err(AerorsyncError::illegal_transition(format!(
                "{:?} → {:?} is not a legal forward transition",
                self.state, next
            )));
        }
        self.state = next;
        Ok(())
    }

    pub fn mark_negotiated(
        &mut self,
        hello: &HelloMessage,
        remote_banner: String,
    ) -> Result<(), AerorsyncError> {
        if !hello.protocol.is_supported() {
            return Err(AerorsyncError::unsupported_version(format!(
                "remote reported {} (supported: {}..={})",
                hello.protocol,
                ProtocolVersion::MIN_SUPPORTED.as_u32(),
                ProtocolVersion::MAX_SUPPORTED.as_u32(),
            )));
        }
        // Probed → Negotiated
        self.transition_to(SessionState::Negotiated)?;
        self.negotiated = Some(NegotiatedSession {
            protocol: hello.protocol,
            role: hello.role,
            features: hello.features.clone(),
            remote_banner,
        });
        Ok(())
    }

    pub fn record_sent(&mut self, bytes: u64) {
        self.stats.bytes_sent = self.stats.bytes_sent.saturating_add(bytes);
    }

    pub fn record_received(&mut self, bytes: u64) {
        self.stats.bytes_received = self.stats.bytes_received.saturating_add(bytes);
    }

    pub fn record_literal(&mut self, bytes: u64) {
        self.stats.literal_bytes = self.stats.literal_bytes.saturating_add(bytes);
    }

    pub fn record_matched(&mut self, bytes: u64) {
        self.stats.matched_bytes = self.stats.matched_bytes.saturating_add(bytes);
    }

    pub fn cancel(&mut self) {
        if !self.state.is_terminal() {
            self.state = SessionState::Cancelled;
        }
    }

    pub fn fail(&mut self) {
        if !self.state.is_terminal() {
            self.state = SessionState::Failed;
        }
    }

    pub fn finalize(&mut self) -> Result<(), AerorsyncError> {
        self.transition_to(SessionState::Finalized)
    }
}
