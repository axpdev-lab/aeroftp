//! End-to-end session driver for the Strada C native rsync prototype.
//!
//! The driver composes the stable pieces owned by other modules:
//!
//! - transport (`transport::RemoteShellTransport`) — open a byte stream
//! - codec    (`protocol::NativeFrameCodec`)       — encode / decode messages
//! - session  (`session::NativeRsyncSession`)      — validated state transitions
//! - roles    (`remote_command::RemoteCommandSpec`) — capture-parity command shape
//! - engine   (`engine_adapter::DeltaEngineAdapter`) — sigs / delta / apply
//!
//! Nothing here talks to real SSH or real rsync wire format yet. The driver
//! only exercises the contracts against a mock transport so every collaborator
//! is verified in one pipeline before we attempt live transport.
//!
//! ## Two entry modes per direction
//!
//! Upload:
//!   - `drive_upload(spec, UploadPlan)` — caller pre-computes deltas. Useful
//!     for unit tests or cached plans.
//!   - `drive_upload_with_engine(spec, file_meta, source_data, adapter)` —
//!     the driver receives the remote signatures and delegates delta
//!     computation to the adapter internally. Typical production path.
//!
//! Download:
//!   - `drive_download(spec, DownloadPlan)` — caller pre-computes signatures.
//!   - `drive_download_with_engine(spec, destination_data, adapter)` — the
//!     driver computes signatures, sends them, collects the remote delta,
//!     and applies it to rebuild the file. `DriveOutcome.reconstructed` is
//!     `Some(bytes)` for this path.
//!
//! ## Message order (both modes)
//!
//! Upload (local = sender, remote = receiver):
//!
//! ```text
//!   L → R  Hello(Sender)
//!   L ← R  Hello(Receiver)
//!   L → R  FileMetadata
//!   L ← R  SignatureBatch(block_size, [blocks])
//!   L → R  DeltaBatch      (terminated by EndOfFile)
//!   L ← R  Summary
//!   L → R  Done
//! ```
//!
//! Download (local = receiver, remote = sender):
//!
//! ```text
//!   L → R  Hello(Receiver)
//!   L ← R  Hello(Sender)
//!   L ← R  FileMetadata
//!   L → R  SignatureBatch(block_size, [blocks])
//!   L ← R  DeltaBatch      (terminated by EndOfFile)
//!   L ← R  Summary
//!   L → R  Done
//! ```
//!
//! Byte accounting: `SessionStats::{bytes_sent, bytes_received}` reflect the
//! actual wire bytes observed at the codec-envelope level and will not match
//! rsync's own `Summary` counters (which are the real rsync wire bytes).
//! `SessionStats::{literal_bytes, matched_bytes}` are copied from the Summary
//! frame and ARE the rsync parity target.

use crate::rsync_native_proto::engine_adapter::{
    engine_ops_to_wire, DeltaEngineAdapter, DeltaInstructionConversionError, EngineDeltaOp,
    EngineSignatureBlock,
};
use crate::rsync_native_proto::protocol::{
    DeltaInstruction, FileMetadataMessage, FrameCodec, HelloMessage, MessageType, NativeFrameCodec,
    SignatureBatchMessage, SignatureBlock, WireMessage,
};
use crate::rsync_native_proto::remote_command::RemoteCommandSpec;
use crate::rsync_native_proto::session::{NativeRsyncSession, SessionState};
use crate::rsync_native_proto::transport::{BidirectionalByteStream, RemoteShellTransport};
use crate::rsync_native_proto::types::{
    FeatureFlag, NativeRsyncError, NativeRsyncErrorKind, ProtocolVersion, SessionRole, SessionStats,
};

/// Upload with caller-computed delta. The last element of
/// `delta_instructions` MUST be `DeltaInstruction::EndOfFile`.
#[derive(Debug, Clone)]
pub struct UploadPlan {
    pub file_meta: FileMetadataMessage,
    pub delta_instructions: Vec<DeltaInstruction>,
}

/// Download with caller-computed signatures. `block_size` must match the
/// one used to compute `basis_signatures`.
#[derive(Debug, Clone)]
pub struct DownloadPlan {
    pub block_size: u32,
    pub basis_signatures: Vec<SignatureBlock>,
}

#[derive(Debug, Clone)]
pub struct DriveOutcome {
    pub final_state: SessionState,
    pub stats: SessionStats,
    pub engine_signatures: Vec<EngineSignatureBlock>,
    pub engine_delta_ops: Vec<EngineDeltaOp>,
    /// Rebuilt destination file bytes. `Some` only after a successful
    /// `drive_download_with_engine`. `None` for all other paths.
    pub reconstructed: Option<Vec<u8>>,
    /// Block size agreed for this session (from the SignatureBatch frame).
    /// Zero until signatures have been exchanged.
    pub block_size: u32,
}

pub struct SessionDriver<T: RemoteShellTransport> {
    pub session: NativeRsyncSession<T>,
    pub codec: NativeFrameCodec,
    cancel_requested: bool,
}

impl<T: RemoteShellTransport> SessionDriver<T> {
    pub fn new(session: NativeRsyncSession<T>, codec: NativeFrameCodec) -> Self {
        Self {
            session,
            codec,
            cancel_requested: false,
        }
    }

    pub async fn cancel(&mut self) -> Result<(), NativeRsyncError> {
        self.cancel_requested = true;
        self.session.cancel();
        self.session.transport.cancel().await
    }

    // --- public drive entry points ---------------------------------------

    pub async fn drive_upload(
        &mut self,
        spec: RemoteCommandSpec,
        plan: UploadPlan,
    ) -> Result<DriveOutcome, NativeRsyncError> {
        Self::validate_upload_plan(&plan)?;
        self.drive_internal(spec, DrivePlan::UploadCaller(plan))
            .await
    }

    pub async fn drive_download(
        &mut self,
        spec: RemoteCommandSpec,
        plan: DownloadPlan,
    ) -> Result<DriveOutcome, NativeRsyncError> {
        self.drive_internal(spec, DrivePlan::DownloadCaller(plan))
            .await
    }

    pub async fn drive_upload_with_engine(
        &mut self,
        spec: RemoteCommandSpec,
        file_meta: FileMetadataMessage,
        source_data: Vec<u8>,
        adapter: &dyn DeltaEngineAdapter,
    ) -> Result<DriveOutcome, NativeRsyncError> {
        self.drive_internal(
            spec,
            DrivePlan::UploadEngine {
                file_meta,
                source_data,
                adapter,
            },
        )
        .await
    }

    pub async fn drive_download_with_engine(
        &mut self,
        spec: RemoteCommandSpec,
        destination_data: Vec<u8>,
        adapter: &dyn DeltaEngineAdapter,
    ) -> Result<DriveOutcome, NativeRsyncError> {
        self.drive_internal(
            spec,
            DrivePlan::DownloadEngine {
                destination_data,
                adapter,
            },
        )
        .await
    }

    fn validate_upload_plan(plan: &UploadPlan) -> Result<(), NativeRsyncError> {
        match plan.delta_instructions.last() {
            Some(DeltaInstruction::EndOfFile) => Ok(()),
            Some(_) => Err(NativeRsyncError::invalid_frame(
                "UploadPlan.delta_instructions must end with DeltaInstruction::EndOfFile",
            )),
            None => Err(NativeRsyncError::invalid_frame(
                "UploadPlan.delta_instructions is empty; at least EndOfFile is required",
            )),
        }
    }

    // --- internal pipeline -----------------------------------------------

    async fn drive_internal<'a>(
        &mut self,
        spec: RemoteCommandSpec,
        plan: DrivePlan<'a>,
    ) -> Result<DriveOutcome, NativeRsyncError> {
        if self.cancel_requested || self.session.state.is_terminal() {
            return Ok(self.terminal_outcome());
        }
        let role = plan.role();

        // 1. Probe.
        let probe = self.session.transport.probe().await.inspect_err(|_| {
            self.session.fail();
        })?;
        if !probe.supports_remote_shell {
            self.session.fail();
            return Err(NativeRsyncError::new(
                NativeRsyncErrorKind::NegotiationFailed,
                "remote transport does not support remote-shell mode",
            ));
        }
        self.guarded_transition(SessionState::Probed)?;

        // 2. Open stream.
        let mut stream = self
            .session
            .transport
            .open_stream(spec.to_exec_request())
            .await
            .inspect_err(|_| {
                self.session.fail();
            })?;

        // 3. Hello exchange (+ version + role checks).
        self.phase_hello(&mut stream, role, &probe.remote_banner)
            .await?;

        // 4. File metadata.
        self.phase_file_metadata(&mut stream, role, plan.file_meta())
            .await?;
        self.guarded_transition(SessionState::FileListPrepared)?;

        // 5. Signature exchange.
        let (block_size, engine_sigs) = self.phase_signatures(&mut stream, role, &plan).await?;

        // 6. Delta exchange. Engine-mode computes/applies here.
        let (engine_ops, reconstructed) = self
            .phase_delta(&mut stream, role, &plan, block_size, &engine_sigs)
            .await?;
        self.guarded_transition(SessionState::Transferring)?;

        // 7. Summary frame — authoritative literal/matched counters.
        let summary_msg = self.recv_non_error(&mut stream).await?;
        let summary = match summary_msg {
            WireMessage::Summary(s) => s,
            other => {
                return self
                    .unexpected(MessageType::Summary, &other)
                    .map(|_| unreachable!())
            }
        };
        self.session.record_literal(summary.literal_bytes);
        self.session.record_matched(summary.matched_bytes);

        // 8. Done + shutdown.
        self.send(&mut stream, &WireMessage::Done).await?;
        if let Err(e) = stream.shutdown().await {
            self.session.fail();
            return Err(e);
        }
        self.session.finalize()?;

        Ok(DriveOutcome {
            final_state: self.session.state,
            stats: self.session.stats.clone(),
            engine_signatures: engine_sigs,
            engine_delta_ops: engine_ops,
            reconstructed,
            block_size,
        })
    }

    async fn phase_hello(
        &mut self,
        stream: &mut T::Stream,
        role: Role,
        remote_banner: &str,
    ) -> Result<(), NativeRsyncError> {
        let local_hello = WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: role.local_role(),
            features: vec![FeatureFlag::DeltaTransfer, FeatureFlag::PreserveTimes],
        });
        self.send(stream, &local_hello).await?;

        let remote_msg = self.recv_non_error(stream).await?;
        let remote_hello = match remote_msg {
            WireMessage::Hello(h) => h,
            other => {
                return self
                    .unexpected(MessageType::Hello, &other)
                    .map(|_| unreachable!())
            }
        };
        if remote_hello.role != role.expected_remote_role() {
            self.session.fail();
            return Err(NativeRsyncError::new(
                NativeRsyncErrorKind::NegotiationFailed,
                format!(
                    "expected remote role {:?}, got {:?}",
                    role.expected_remote_role(),
                    remote_hello.role
                ),
            ));
        }
        self.session
            .mark_negotiated(&remote_hello, remote_banner.to_string())
            .inspect_err(|_| {
                self.session.fail();
            })?;
        Ok(())
    }

    async fn phase_file_metadata(
        &mut self,
        stream: &mut T::Stream,
        role: Role,
        outgoing: Option<&FileMetadataMessage>,
    ) -> Result<FileMetadataMessage, NativeRsyncError> {
        match role {
            Role::LocalSender => {
                let fm = outgoing
                    .cloned()
                    .expect("sender must provide file metadata");
                self.send(stream, &WireMessage::FileMetadata(fm.clone()))
                    .await?;
                Ok(fm)
            }
            Role::LocalReceiver => {
                let msg = self.recv_non_error(stream).await?;
                match msg {
                    WireMessage::FileMetadata(fm) => Ok(fm),
                    other => self
                        .unexpected(MessageType::FileMetadata, &other)
                        .map(|_| unreachable!()),
                }
            }
        }
    }

    async fn phase_signatures<'a>(
        &mut self,
        stream: &mut T::Stream,
        role: Role,
        plan: &DrivePlan<'a>,
    ) -> Result<(u32, Vec<EngineSignatureBlock>), NativeRsyncError> {
        match role {
            Role::LocalSender => {
                // Receive remote signatures.
                let msg = self.recv_non_error(stream).await?;
                let batch = match msg {
                    WireMessage::SignatureBatch(b) => b,
                    other => {
                        return self
                            .unexpected(MessageType::SignatureBatch, &other)
                            .map(|_| unreachable!())
                    }
                };
                let engine: Vec<EngineSignatureBlock> = batch
                    .blocks
                    .into_iter()
                    .map(EngineSignatureBlock::from)
                    .collect();
                Ok((batch.block_size, engine))
            }
            Role::LocalReceiver => {
                // Produce and send basis signatures. Source is either the
                // caller-provided DownloadPlan or the engine-mode adapter.
                let (block_size, engine_sigs) = self.build_receiver_signatures(plan)?;
                let wire_blocks: Vec<SignatureBlock> = engine_sigs
                    .iter()
                    .cloned()
                    .map(SignatureBlock::from)
                    .collect();
                let msg = WireMessage::SignatureBatch(SignatureBatchMessage {
                    block_size,
                    blocks: wire_blocks,
                });
                self.send(stream, &msg).await?;
                Ok((block_size, engine_sigs))
            }
        }
    }

    fn build_receiver_signatures<'a>(
        &mut self,
        plan: &DrivePlan<'a>,
    ) -> Result<(u32, Vec<EngineSignatureBlock>), NativeRsyncError> {
        match plan {
            DrivePlan::DownloadCaller(p) => {
                let engine: Vec<EngineSignatureBlock> = p
                    .basis_signatures
                    .iter()
                    .cloned()
                    .map(EngineSignatureBlock::from)
                    .collect();
                Ok((p.block_size, engine))
            }
            DrivePlan::DownloadEngine {
                destination_data,
                adapter,
            } => {
                let bs_usize = adapter.compute_block_size(destination_data.len() as u64);
                let sigs = adapter.build_signatures(destination_data, bs_usize);
                let block_size = u32::try_from(bs_usize).map_err(|_| {
                    self.session.fail();
                    NativeRsyncError::invalid_frame(format!(
                        "engine block_size {bs_usize} exceeds u32 wire range"
                    ))
                })?;
                Ok((block_size, sigs))
            }
            _ => unreachable!("phase_signatures receiver called with non-download plan"),
        }
    }

    async fn phase_delta<'a>(
        &mut self,
        stream: &mut T::Stream,
        role: Role,
        plan: &DrivePlan<'a>,
        block_size: u32,
        engine_sigs: &[EngineSignatureBlock],
    ) -> Result<(Vec<EngineDeltaOp>, Option<Vec<u8>>), NativeRsyncError> {
        match role {
            Role::LocalSender => {
                let instructions =
                    self.build_sender_delta_instructions(plan, block_size, engine_sigs)?;
                self.send(stream, &WireMessage::DeltaBatch(instructions.clone()))
                    .await?;
                let ops = self.convert_delta_batch(instructions)?;
                Ok((ops, None))
            }
            Role::LocalReceiver => {
                let msg = self.recv_non_error(stream).await?;
                let batch = match msg {
                    WireMessage::DeltaBatch(b) => b,
                    other => {
                        return self
                            .unexpected(MessageType::DeltaBatch, &other)
                            .map(|_| unreachable!())
                    }
                };
                let ops = self.convert_delta_batch(batch)?;
                // Engine mode: apply delta to reconstruct the file.
                let reconstructed = match plan {
                    DrivePlan::DownloadEngine {
                        destination_data,
                        adapter,
                    } => {
                        let bytes = adapter
                            .apply_delta(destination_data, &ops, block_size as usize)
                            .map_err(|e| {
                                self.session.fail();
                                NativeRsyncError::invalid_frame(format!("apply_delta failed: {e}"))
                            })?;
                        Some(bytes)
                    }
                    DrivePlan::DownloadCaller(_) => None,
                    _ => unreachable!("phase_delta receiver called with non-download plan"),
                };
                Ok((ops, reconstructed))
            }
        }
    }

    fn build_sender_delta_instructions<'a>(
        &mut self,
        plan: &DrivePlan<'a>,
        block_size: u32,
        engine_sigs: &[EngineSignatureBlock],
    ) -> Result<Vec<DeltaInstruction>, NativeRsyncError> {
        match plan {
            DrivePlan::UploadCaller(p) => Ok(p.delta_instructions.clone()),
            DrivePlan::UploadEngine {
                source_data,
                adapter,
                ..
            } => {
                let delta_plan =
                    adapter.compute_delta(source_data, engine_sigs, block_size as usize);
                Ok(engine_ops_to_wire(delta_plan.ops))
            }
            _ => unreachable!("phase_delta sender called with non-upload plan"),
        }
    }

    // --- low-level helpers -----------------------------------------------

    fn terminal_outcome(&self) -> DriveOutcome {
        DriveOutcome {
            final_state: self.session.state,
            stats: self.session.stats.clone(),
            engine_signatures: Vec::new(),
            engine_delta_ops: Vec::new(),
            reconstructed: None,
            block_size: 0,
        }
    }

    fn guarded_transition(&mut self, next: SessionState) -> Result<(), NativeRsyncError> {
        self.session.transition_to(next).inspect_err(|_| {
            self.session.fail();
        })
    }

    async fn send(
        &mut self,
        stream: &mut T::Stream,
        msg: &WireMessage,
    ) -> Result<(), NativeRsyncError> {
        let bytes = self.codec.encode(msg).inspect_err(|_| {
            self.session.fail();
        })?;
        stream.write_frame(&bytes).await.inspect_err(|_| {
            self.session.fail();
        })?;
        self.session.record_sent(bytes.len() as u64);
        Ok(())
    }

    async fn recv_non_error(
        &mut self,
        stream: &mut T::Stream,
    ) -> Result<WireMessage, NativeRsyncError> {
        let bytes = stream.read_frame().await.inspect_err(|_| {
            self.session.fail();
        })?;
        self.session.record_received(bytes.len() as u64);
        let msg = self.codec.decode(&bytes).inspect_err(|_| {
            self.session.fail();
        })?;
        if let WireMessage::Error(e) = msg {
            self.session.fail();
            return Err(NativeRsyncError::remote(e.code, e.message));
        }
        Ok(msg)
    }

    fn unexpected(
        &mut self,
        expected: MessageType,
        got: &WireMessage,
    ) -> Result<(), NativeRsyncError> {
        self.session.fail();
        Err(NativeRsyncError::unexpected_message(format!(
            "expected {:?}, got {:?}",
            expected,
            got.message_type()
        )))
    }

    fn convert_delta_batch(
        &mut self,
        batch: Vec<DeltaInstruction>,
    ) -> Result<Vec<EngineDeltaOp>, NativeRsyncError> {
        let mut out = Vec::with_capacity(batch.len());
        let mut saw_end = false;
        for (idx, ins) in batch.into_iter().enumerate() {
            if saw_end {
                self.session.fail();
                return Err(NativeRsyncError::invalid_frame(format!(
                    "DeltaInstruction at index {idx} follows EndOfFile marker"
                )));
            }
            match EngineDeltaOp::try_from(ins) {
                Ok(op) => out.push(op),
                Err(DeltaInstructionConversionError::EndOfFileIsFramingMarker) => {
                    saw_end = true;
                }
            }
        }
        if !saw_end {
            self.session.fail();
            return Err(NativeRsyncError::invalid_frame(
                "delta batch missing EndOfFile terminator",
            ));
        }
        Ok(out)
    }
}

// --- internal plan enum ---------------------------------------------------

enum DrivePlan<'a> {
    UploadCaller(UploadPlan),
    UploadEngine {
        file_meta: FileMetadataMessage,
        source_data: Vec<u8>,
        adapter: &'a dyn DeltaEngineAdapter,
    },
    DownloadCaller(DownloadPlan),
    DownloadEngine {
        destination_data: Vec<u8>,
        adapter: &'a dyn DeltaEngineAdapter,
    },
}

impl<'a> DrivePlan<'a> {
    fn role(&self) -> Role {
        match self {
            DrivePlan::UploadCaller(_) | DrivePlan::UploadEngine { .. } => Role::LocalSender,
            DrivePlan::DownloadCaller(_) | DrivePlan::DownloadEngine { .. } => Role::LocalReceiver,
        }
    }

    fn file_meta(&self) -> Option<&FileMetadataMessage> {
        match self {
            DrivePlan::UploadCaller(p) => Some(&p.file_meta),
            DrivePlan::UploadEngine { file_meta, .. } => Some(file_meta),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Role {
    LocalSender,
    LocalReceiver,
}

impl Role {
    fn local_role(self) -> SessionRole {
        match self {
            Role::LocalSender => SessionRole::Sender,
            Role::LocalReceiver => SessionRole::Receiver,
        }
    }

    fn expected_remote_role(self) -> SessionRole {
        match self {
            Role::LocalSender => SessionRole::Receiver,
            Role::LocalReceiver => SessionRole::Sender,
        }
    }
}
