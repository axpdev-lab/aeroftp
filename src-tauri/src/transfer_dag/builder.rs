// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `TransferDagBuilder`: shared construction of production transfer graphs.
//!
//! DAG-ENGINE phase 1 converges the single-file download and upload paths,
//! GUI and CLI, onto one [`TransferDag`] shape so every surface schedules the
//! same node graph through [`execute_dag`](super::executor::execute_dag).
//!
//! The single-file graph is a linear chain. A single file transfer is
//! inherently sequential (discover before acquire before transfer before
//! verify ...), so routing it through the DAG buys not parallelism but
//! structural uniformity with the batch and sync graphs (phase 2) and a
//! single observability surface, instead of a hand-rolled orchestration per
//! call site.
//!
//! Node responsibilities, in phase 1:
//!
//! - `DiscoverRemote` / `DiscoverLocal`: resolve the transfer size (a remote
//!   `size()` call, or a local `metadata()` read).
//! - `AcquireResource`: structural anchor. A no-op in phase 1; phase 3 hooks
//!   the resume-checkpoint fetch here.
//! - `DownloadFile` / `UploadFile`: the real I/O. The only node that carries a
//!   scarce directional file resource ([`ResourceRequest::download_file`] /
//!   [`ResourceRequest::upload_file`]).
//! - `VerifyChecksum`: structural anchor. A no-op in phase 1 (the legacy
//!   single-file path does not verify); phase 3 makes it real behind the
//!   `server_checksum` capability.
//! - `PreserveMetadata`: restore the remote mtime on a downloaded file. A
//!   no-op on the upload direction.
//! - `CommitTemp`: structural anchor. A no-op in phase 1 because every
//!   provider's own `download` / `upload` already performs the atomic
//!   `.aerotmp` finalize internally (since v3.0.5); phase 3 may host an
//!   explicit resume-checkpoint commit here.
//! - `EmitProgress`: emit the terminal completion event.
//!
//! Temp-file cleanup on failure is intentionally NOT a graph node. The
//! ready-frontier executor aborts on the first failed node and fail-fast
//! cancels the in-flight set; it never dispatches a node whose dependency
//! failed, so a `CleanupTemp` node could not run on the failure path.
//! Cleanup stays an RAII concern of the transfer runner (the provider's own
//! `.aerotmp` guard), which is the honest place for it.

use super::capabilities::TransferCapabilities;
use super::graph::{TransferDag, TransferNodeKind};
use super::resources::{multipart_part_byte_len, ResourceRequest};

/// Multipart chunk size used when a provider advertises `multipart_upload` but
/// does not state a `preferred_chunk_size`. 8 MiB balances part count against
/// per-part request overhead.
const DEFAULT_MULTIPART_CHUNK_SIZE: u64 = 8 * 1024 * 1024;

/// Hard cap on the number of `UploadPart` nodes a multipart graph may carry.
/// Mirrors the S3 multipart-upload limit so a pathological chunk size cannot
/// produce a graph with hundreds of thousands of nodes.
const MAX_MULTIPART_PARTS: usize = 10_000;

/// Transfer direction for [`TransferDagBuilder::single_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Download,
    Upload,
}

/// A built single-file transfer graph plus the id of every node, so a runner
/// can bind each [`TransferNodeKind`] to its action without re-scanning the
/// graph. Every kind appears exactly once in a single-file graph, so a runner
/// may equally match on [`TransferNodeKind`] directly; the ids are kept for
/// callers and tests that prefer an explicit handle.
#[derive(Debug, Clone)]
pub struct SingleFileDag {
    /// The graph, ready for [`execute_dag`](super::executor::execute_dag).
    pub dag: TransferDag,
    /// The direction this graph was built for.
    pub direction: TransferDirection,
    /// `DiscoverRemote` (download) or `DiscoverLocal` (upload).
    pub discover: usize,
    /// `AcquireResource`.
    pub acquire: usize,
    /// `DownloadFile` (download) or `UploadFile` (upload): the I/O node.
    pub transfer: usize,
    /// `VerifyChecksum`.
    pub verify: usize,
    /// `PreserveMetadata`.
    pub preserve_metadata: usize,
    /// `CommitTemp`.
    pub commit: usize,
    /// `EmitProgress`: the terminal node.
    pub emit_progress: usize,
}

/// One file to include in a batch transfer graph.
///
/// The builder only needs a stable key and direction. Runtime paths, retry
/// policy, and provider handles stay with the runner that binds node ids to
/// real I/O. `key` is intentionally opaque here: GUI batch callers may use
/// a transfer entry id, while sync callers should use the relative path plus
/// action when a plain relative path is ambiguous.
///
/// `file_size` is observational for the legacy `from_batch` shape (which
/// ignores it) and structural for the shaped `from_batch_shaped` shape: an
/// upload large enough to span more than one preferred chunk on a
/// multipart-capable provider fans out into N `UploadPart` nodes. Defaults
/// to `0` so existing call sites that only know the file key keep producing
/// the same single-transfer-core graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDagItem {
    pub key: String,
    pub direction: TransferDirection,
    pub file_size: u64,
}

impl BatchDagItem {
    pub fn new(key: impl Into<String>, direction: TransferDirection) -> Self {
        Self {
            key: key.into(),
            direction,
            file_size: 0,
        }
    }

    /// Build a batch entry pre-populated with the source file's byte length.
    ///
    /// The size matters only when the batch graph is built via
    /// [`TransferDagBuilder::from_batch_shaped`] with a capability set that
    /// advertises multipart upload: it is the input the shaping profile uses
    /// to decide the part count.
    pub fn with_size(key: impl Into<String>, direction: TransferDirection, file_size: u64) -> Self {
        Self {
            key: key.into(),
            direction,
            file_size,
        }
    }
}

/// Node ids for one file sub-DAG inside a batch graph.
///
/// `transfer` is the primary transfer node id: for the legacy
/// [`TransferDagBuilder::from_batch`] shape and the single-core variant of
/// the shaped [`TransferDagBuilder::from_batch_shaped`] shape it is the only
/// transfer node and equals `transfer_nodes[0]`. For the multipart fan-out
/// variant `transfer_nodes` carries every `UploadPart` node id in part-number
/// order (transfer_nodes[0] = part 1, ...), and `transfer` points to the
/// first part so existing callers that bind one node id keep working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchFileDag {
    pub key: String,
    pub direction: TransferDirection,
    pub discover: usize,
    pub acquire: usize,
    pub transfer: usize,
    /// Full transfer core: one node for single-core, N for multipart fan-out.
    pub transfer_nodes: Vec<usize>,
    pub verify: usize,
    pub preserve_metadata: usize,
    pub commit: usize,
    /// Terminal node for the file. Fase 2 journal observers attach durable
    /// per-file completion to this id, never to the raw transfer node.
    pub emit_progress: usize,
}

/// A built multi-file transfer graph plus per-file terminal metadata.
#[derive(Debug, Clone)]
pub struct BatchDag {
    pub dag: TransferDag,
    pub files: Vec<BatchFileDag>,
}

/// A planned sync action that can be represented in the transfer DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDagAction {
    Upload,
    Download,
    DeleteLocal,
    DeleteRemote,
    Skip,
    KeepBoth,
}

impl SyncDagAction {
    fn transfer_direction(self) -> Option<TransferDirection> {
        match self {
            Self::Upload => Some(TransferDirection::Upload),
            Self::Download => Some(TransferDirection::Download),
            Self::DeleteLocal | Self::DeleteRemote | Self::Skip | Self::KeepBoth => None,
        }
    }
}

/// One entry in a sync plan.
///
/// `file_size` is observational for the legacy `from_sync_plan` shape (which
/// ignores it) and structural for the shaped `from_sync_plan_shaped` shape:
/// a sync entry large enough to span more than one preferred chunk on a
/// multipart-capable provider fans out into N `UploadPart` nodes when the
/// action is `Upload`. Defaults to `0` so existing call sites continue to
/// produce the same single-transfer-core chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDagItem {
    pub key: String,
    pub action: SyncDagAction,
    pub file_size: u64,
}

impl SyncDagItem {
    pub fn new(key: impl Into<String>, action: SyncDagAction) -> Self {
        Self {
            key: key.into(),
            action,
            file_size: 0,
        }
    }

    /// Build a sync entry pre-populated with the source object's byte length.
    pub fn with_size(key: impl Into<String>, action: SyncDagAction, file_size: u64) -> Self {
        Self {
            key: key.into(),
            action,
            file_size,
        }
    }
}

/// Node ids for one transfer-producing sync plan entry.
///
/// `transfer` is the primary transfer node and equals `transfer_nodes[0]`
/// for both the legacy `from_sync_plan` shape and the single-core variant
/// of the shaped path. Under multipart fan-out `transfer_nodes` carries
/// every `UploadPart` node id in part-number order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFileDag {
    pub key: String,
    pub plan_index: usize,
    pub action: SyncDagAction,
    pub direction: TransferDirection,
    pub acquire: usize,
    pub transfer: usize,
    /// Full transfer core: one node for single-core, N for multipart fan-out.
    pub transfer_nodes: Vec<usize>,
    pub verify: usize,
    pub preserve_metadata: usize,
    pub commit: usize,
    /// Terminal node for the sync file. Fase 2 journal observers attach
    /// durable completion to this id.
    pub emit_progress: usize,
}

/// A built sync graph plus global discovery/compare ids and per-file
/// transfer terminal metadata.
#[derive(Debug, Clone)]
pub struct SyncDag {
    pub dag: TransferDag,
    pub discover_local: usize,
    pub discover_remote: usize,
    pub compare: usize,
    pub files: Vec<SyncFileDag>,
}

impl SyncDag {
    /// Terminal-to-journal mapping when the legacy journal entries preserve
    /// the same order as the sync plan passed to [`TransferDagBuilder::from_sync_plan`].
    pub fn journal_terminals_for_plan_order(&self) -> Vec<super::observer::SyncJournalTerminal> {
        self.files
            .iter()
            .map(|file| {
                super::observer::SyncJournalTerminal::new(file.emit_progress, file.plan_index)
            })
            .collect()
    }
}

/// Capability-derived shaping decisions for one file's transfer graph
/// (DAG-ENGINE phase 3). Resolved once from a provider's
/// [`TransferCapabilities`] and the file size; the builder reads it to decide
/// how many transfer nodes to emit and what each one reserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferGraphProfile {
    /// Number of upload parts. `1` is a single `UploadFile`; `> 1` fans the
    /// transfer core out into that many parallel `UploadPart` nodes. Always
    /// `1` on the download direction (multipart is an upload-only concept).
    pub upload_parts: usize,
    /// `true` when the provider can resume the transfer from a checkpoint, so
    /// the `AcquireResource` node fetches the saved offset and the transfer
    /// starts from there. The graph shape is unchanged; the runner reads it.
    pub resume: bool,
    /// Extra `api_slots` every API-bound transfer node reserves. `1` for
    /// providers that advertise `rate_limited_api`, `0` otherwise: it lets the
    /// shared `api_slots` budget (and the AIMD `Api` class) throttle the
    /// API-bound providers without touching transport-bound ones.
    pub api_slots: u16,
    /// Maximum number of multipart chunks the runner may dispatch at once.
    /// `1` preserves the legacy single-stream behavior for providers that
    /// either do not support multipart upload or do not advertise a higher
    /// chunk budget.
    pub max_chunk_slots: u16,
    /// Provider's preferred multipart chunk size (bytes). The runner uses
    /// this verbatim as the per-part byte length so chunks honour the
    /// provider's alignment contract (Google Drive: 256 KiB; OneDrive:
    /// 320 KiB; S3 / B2: any size ≥ 5 MiB). The last part takes whatever
    /// remains of the file. `0` means no preference — the runner falls
    /// back to the `file_size / upload_parts` div_ceil distribution.
    pub preferred_chunk_size: u64,
}

impl TransferGraphProfile {
    /// Resolve the profile for `direction` from a provider's capabilities and
    /// the known `file_size` (used only to size the multipart part count).
    pub fn resolve(
        direction: TransferDirection,
        caps: &TransferCapabilities,
        file_size: u64,
    ) -> Self {
        let api_slots = u16::from(caps.rate_limited_api.is_available());
        let resume = match direction {
            TransferDirection::Download => caps.resume_download,
            TransferDirection::Upload => caps.resume_upload,
        }
        .is_available();

        let chunk_hint = caps
            .preferred_chunk_size
            .filter(|size| *size > 0)
            .unwrap_or(DEFAULT_MULTIPART_CHUNK_SIZE);

        // Honour the provider's multipart_threshold: below it, an upload stays a
        // single PUT exactly like the legacy `upload()` path. `0` means unset, in
        // which case we fall back to the chunk size (any file larger than one
        // part fans out, the pre-fix behaviour). This keeps the DAG's
        // single-vs-multipart decision aligned with the rest of the codebase and
        // removes the orchestration cost on medium uploads (audit DISP-01/CORR-02).
        let multipart_threshold = if caps.multipart_threshold == 0 {
            chunk_hint
        } else {
            caps.multipart_threshold
        };
        let multipart_eligible = direction == TransferDirection::Upload
            && caps.multipart_upload.is_available()
            && file_size >= multipart_threshold;

        let upload_parts = if multipart_eligible {
            let parts = file_size.div_ceil(chunk_hint).max(1);
            (parts as usize).clamp(1, MAX_MULTIPART_PARTS)
        } else {
            1
        };
        let max_chunk_slots = if multipart_eligible {
            caps.max_chunk_slots.unwrap_or(1).max(1)
        } else {
            1
        };
        // `upload_parts` is clamped to `MAX_MULTIPART_PARTS`. When the file is
        // larger than `MAX_MULTIPART_PARTS * chunk_hint` the clamp would leave
        // `parts * chunk_hint < file_size`, and the runner (which reads exactly
        // `part_size` bytes per part) would silently drop the tail. Grow the
        // effective chunk so the parts always cover the whole file. This only
        // changes anything in the clamped case: when unclamped,
        // `div_ceil(file_size, upload_parts) <= chunk_hint`.
        let preferred_chunk_size = if multipart_eligible {
            chunk_hint.max(file_size.div_ceil(upload_parts as u64))
        } else {
            0
        };

        Self {
            upload_parts,
            resume,
            api_slots,
            max_chunk_slots,
            preferred_chunk_size,
        }
    }
}

/// A built capability-shaped single-file graph. Unlike [`SingleFileDag`] the
/// transfer core is a `Vec`: one node for a plain transfer, or N `UploadPart`
/// nodes for a multipart upload. `VerifyChecksum` joins every transfer node so
/// it cannot run until the last part lands.
#[derive(Debug, Clone)]
pub struct ShapedFileDag {
    pub dag: TransferDag,
    pub direction: TransferDirection,
    pub profile: TransferGraphProfile,
    pub discover: usize,
    pub acquire: usize,
    /// The transfer core: one `DownloadFile` / `UploadFile`, or N `UploadPart`.
    pub transfer: Vec<usize>,
    pub verify: usize,
    pub preserve_metadata: usize,
    pub commit: usize,
    pub emit_progress: usize,
}

/// A built fan-out segmented-download graph: one `DownloadRange` node per
/// requested segment, with no inter-segment dependencies. The shared chunk /
/// HTTP / disk-write budget governs how many segments run at once.
#[derive(Debug, Clone)]
pub struct ShapedRangesDag {
    pub dag: TransferDag,
    /// One `DownloadRange` node per segment, in creation order. Callers map
    /// `transfer[i]` to the i-th `(start, end)` pair of their plan.
    pub transfer: Vec<usize>,
}

/// A built capability-shaped copy graph. When the provider advertises
/// `server_side_copy` the transfer core collapses into a single
/// [`TransferNodeKind::ServerSideCopy`] node (the server moves the bytes, no
/// disk I/O); otherwise it degrades honestly to a `DownloadFile` followed by
/// an `UploadFile`.
#[derive(Debug, Clone)]
pub struct CopyDag {
    pub dag: TransferDag,
    /// `true` when the core is a single `ServerSideCopy` node.
    pub server_side: bool,
    pub discover: usize,
    pub acquire: usize,
    /// One `ServerSideCopy` node, or a `DownloadFile` then an `UploadFile`.
    pub copy: Vec<usize>,
    pub verify: usize,
    pub preserve_metadata: usize,
    pub commit: usize,
    pub emit_progress: usize,
}

/// Stateless constructor of shared production transfer graphs.
pub struct TransferDagBuilder;

impl TransferDagBuilder {
    /// Build the single-file transfer graph for `direction`.
    ///
    /// Seven nodes, a linear chain:
    /// `Discover{Remote|Local}` → `AcquireResource` → `{Download|Upload}File`
    /// → `VerifyChecksum` → `PreserveMetadata` → `CommitTemp` → `EmitProgress`.
    ///
    /// Only the transfer node carries a scarce directional resource
    /// ([`ResourceRequest::upload_file`] / [`ResourceRequest::download_file`]);
    /// every other node is a metadata or structural step with no resource
    /// request, so they never contend on the shared semaphores and the graph
    /// cannot deadlock against its own budget.
    pub fn single_file(direction: TransferDirection) -> SingleFileDag {
        let mut dag = TransferDag::default();
        let ids = append_single_file_chain(&mut dag, direction);

        SingleFileDag {
            dag,
            direction,
            discover: ids.discover,
            acquire: ids.acquire,
            transfer: ids.transfer,
            verify: ids.verify,
            preserve_metadata: ids.preserve_metadata,
            commit: ids.commit,
            emit_progress: ids.emit_progress,
        }
    }

    /// Build a multi-file batch graph by merging one single-file sub-DAG per
    /// item into the same [`TransferDag`].
    ///
    /// The sub-DAGs are intentionally independent: their discover/acquire
    /// prefixes can enter the ready frontier together, while the real transfer
    /// nodes all carry a directional file request. A single
    /// [`TransferResourceManager`](super::resources::TransferResourceManager)
    /// shared by the executor is therefore the only file-level concurrency
    /// governor, matching the Fase 2 `file_slots` contract.
    pub fn from_batch(items: &[BatchDagItem]) -> BatchDag {
        let mut dag = TransferDag::default();
        let mut files = Vec::with_capacity(items.len());

        for item in items {
            let ids = append_single_file_chain(&mut dag, item.direction);
            files.push(BatchFileDag {
                key: item.key.clone(),
                direction: item.direction,
                discover: ids.discover,
                acquire: ids.acquire,
                transfer: ids.transfer,
                transfer_nodes: vec![ids.transfer],
                verify: ids.verify,
                preserve_metadata: ids.preserve_metadata,
                commit: ids.commit,
                emit_progress: ids.emit_progress,
            });
        }

        BatchDag { dag, files }
    }

    /// Build a multi-file batch graph with per-file capability shaping.
    ///
    /// Mirrors [`Self::from_batch`] but routes every file through
    /// [`append_shaped_file_chain`], so when the shared `caps` advertise
    /// `multipart_upload` and an entry carries a `file_size` above one
    /// preferred chunk on the upload direction, the file's transfer core
    /// fans out into N `UploadPart` nodes instead of one `UploadFile`.
    ///
    /// All files in a single batch share the same `caps` because every
    /// batch is bound to one provider session; the shape per file still
    /// varies through direction and `file_size`. A `caps` set with
    /// `multipart_upload = Unsupported` (the default) reproduces the exact
    /// graph shape of [`Self::from_batch`].
    pub fn from_batch_shaped(items: &[BatchDagItem], caps: &TransferCapabilities) -> BatchDag {
        let mut dag = TransferDag::default();
        let mut files = Vec::with_capacity(items.len());

        for item in items {
            let ids = append_shaped_file_chain(&mut dag, item.direction, caps, item.file_size);
            files.push(BatchFileDag {
                key: item.key.clone(),
                direction: item.direction,
                discover: ids.discover,
                acquire: ids.acquire,
                transfer: *ids
                    .transfer_nodes
                    .first()
                    .expect("shaped file chain has at least one transfer node"),
                transfer_nodes: ids.transfer_nodes,
                verify: ids.verify,
                preserve_metadata: ids.preserve_metadata,
                commit: ids.commit,
                emit_progress: ids.emit_progress,
            });
        }

        BatchDag { dag, files }
    }

    /// Build a sync-session graph.
    ///
    /// Sync has one global discovery/compare prefix:
    /// `DiscoverLocal` and `DiscoverRemote` run independently, then `Compare`
    /// joins them. Every transfer-producing plan entry hangs below `Compare`
    /// as a per-file chain beginning at `AcquireResource`; delete/skip entries
    /// do not produce file-transfer nodes in this Fase 2 slice.
    pub fn from_sync_plan(plan: &[SyncDagItem]) -> SyncDag {
        let mut dag = TransferDag::default();
        let discover_local = dag.add_node(
            TransferNodeKind::DiscoverLocal,
            vec![],
            ResourceRequest::default(),
        );
        let discover_remote = dag.add_node(
            TransferNodeKind::DiscoverRemote,
            vec![],
            ResourceRequest::default(),
        );
        let compare = dag.add_node(
            TransferNodeKind::Compare,
            vec![discover_local, discover_remote],
            ResourceRequest::default(),
        );
        let mut files = Vec::new();

        for (plan_index, item) in plan.iter().enumerate() {
            let Some(direction) = item.action.transfer_direction() else {
                continue;
            };
            let ids = append_sync_file_chain(&mut dag, direction, compare);
            files.push(SyncFileDag {
                key: item.key.clone(),
                plan_index,
                action: item.action,
                direction,
                acquire: ids.acquire,
                transfer: ids.transfer,
                transfer_nodes: vec![ids.transfer],
                verify: ids.verify,
                preserve_metadata: ids.preserve_metadata,
                commit: ids.commit,
                emit_progress: ids.emit_progress,
            });
        }

        SyncDag {
            dag,
            discover_local,
            discover_remote,
            compare,
            files,
        }
    }

    /// Build a sync-session graph with per-file capability shaping.
    ///
    /// Mirrors [`Self::from_sync_plan`] but routes every transfer-producing
    /// entry through [`append_shaped_sync_file_chain`], so when the shared
    /// `caps` advertise `multipart_upload` and an upload entry's
    /// `file_size` spans more than one preferred chunk the transfer core
    /// fans out into N `UploadPart` nodes. Default caps reproduce the
    /// legacy single-transfer-core shape verbatim, so this is byte-
    /// identical with [`Self::from_sync_plan`] when called with
    /// `TransferCapabilities::default()`.
    pub fn from_sync_plan_shaped(plan: &[SyncDagItem], caps: &TransferCapabilities) -> SyncDag {
        let mut dag = TransferDag::default();
        let discover_local = dag.add_node(
            TransferNodeKind::DiscoverLocal,
            vec![],
            ResourceRequest::default(),
        );
        let discover_remote = dag.add_node(
            TransferNodeKind::DiscoverRemote,
            vec![],
            ResourceRequest::default(),
        );
        let compare = dag.add_node(
            TransferNodeKind::Compare,
            vec![discover_local, discover_remote],
            ResourceRequest::default(),
        );
        let mut files = Vec::new();

        for (plan_index, item) in plan.iter().enumerate() {
            let Some(direction) = item.action.transfer_direction() else {
                continue;
            };
            let ids =
                append_shaped_sync_file_chain(&mut dag, direction, compare, caps, item.file_size);
            files.push(SyncFileDag {
                key: item.key.clone(),
                plan_index,
                action: item.action,
                direction,
                acquire: ids.acquire,
                transfer: *ids
                    .transfer_nodes
                    .first()
                    .expect("shaped sync chain has at least one transfer node"),
                transfer_nodes: ids.transfer_nodes,
                verify: ids.verify,
                preserve_metadata: ids.preserve_metadata,
                commit: ids.commit,
                emit_progress: ids.emit_progress,
            });
        }

        SyncDag {
            dag,
            discover_local,
            discover_remote,
            compare,
            files,
        }
    }

    /// Build a fan-out segmented-download graph.
    ///
    /// Returns one [`TransferNodeKind::DownloadRange`] node per requested
    /// segment, with no dependencies between them. Every node reserves one
    /// `range_chunk` resource so the shared chunk / HTTP / disk-write budget
    /// governs how many segments run at once. The legacy
    /// [`crate::providers::multi_thread::run_ranges_via_graph`] runner
    /// produced this exact shape inline; expressing it here keeps the
    /// builder the single source of truth for every production graph shape
    /// and unblocks the SG-T19 collapse where the manual node construction
    /// goes away.
    pub fn shaped_ranges(segments: usize) -> ShapedRangesDag {
        let mut dag = TransferDag::default();
        let mut transfer = Vec::with_capacity(segments);
        for _ in 0..segments {
            transfer.push(dag.add_node(
                TransferNodeKind::DownloadRange,
                vec![],
                ResourceRequest::range_chunk(),
            ));
        }
        ShapedRangesDag { dag, transfer }
    }

    /// Build a capability-shaped single-file graph (DAG-ENGINE phase 3).
    ///
    /// The graph reads the provider's [`TransferCapabilities`] and the file
    /// size through [`TransferGraphProfile`]:
    ///
    /// - `multipart_upload` available on an upload large enough for more than
    ///   one part: the transfer core fans out into N `UploadPart` nodes, each
    ///   reserving one `chunk_slot`, so the shared chunk budget governs how
    ///   many parts upload at once.
    /// - `rate_limited_api` available: every transfer node also reserves one
    ///   `api_slot`, exposing it to the shared API budget and the AIMD `Api`
    ///   class.
    /// - `resume_*` available: recorded on the profile so the runner's
    ///   `AcquireResource` node fetches the checkpoint; the graph shape is
    ///   unchanged.
    ///
    /// With a capability set that advertises none of the above this produces
    /// the same seven-node linear chain as [`Self::single_file`].
    pub fn shaped_file(
        direction: TransferDirection,
        caps: &TransferCapabilities,
        file_size: u64,
    ) -> ShapedFileDag {
        let profile = TransferGraphProfile::resolve(direction, caps, file_size);
        let mut dag = TransferDag::default();

        let discover_kind = match direction {
            TransferDirection::Download => TransferNodeKind::DiscoverRemote,
            TransferDirection::Upload => TransferNodeKind::DiscoverLocal,
        };
        let discover = dag.add_node(discover_kind, vec![], ResourceRequest::default());
        let acquire = dag.add_node(
            TransferNodeKind::AcquireResource,
            vec![discover],
            ResourceRequest::default(),
        );

        // Single transfer-core helper shared with batch/sync shaped builders
        // so multipart topology (cap=1 chain vs cap>1 fan-out) cannot drift.
        let transfer = append_transfer_core(&mut dag, direction, acquire, &profile, file_size);

        let verify = dag.add_node(
            TransferNodeKind::VerifyChecksum,
            transfer.clone(),
            ResourceRequest::default(),
        );
        let preserve_metadata = dag.add_node(
            TransferNodeKind::PreserveMetadata,
            vec![verify],
            ResourceRequest::default(),
        );
        let commit = dag.add_node(
            TransferNodeKind::CommitTemp,
            vec![preserve_metadata],
            ResourceRequest::default(),
        );
        let emit_progress = dag.add_node(
            TransferNodeKind::EmitProgress,
            vec![commit],
            ResourceRequest::default(),
        );

        ShapedFileDag {
            dag,
            direction,
            profile,
            discover,
            acquire,
            transfer,
            verify,
            preserve_metadata,
            commit,
            emit_progress,
        }
    }

    /// Build a capability-shaped copy graph (DAG-ENGINE phase 3, F3-T02).
    ///
    /// When `caps.server_side_copy` is available the transfer core is a single
    /// [`TransferNodeKind::ServerSideCopy`] node: the server moves the bytes,
    /// so the node reserves only an `api_slot`, never a file or disk slot.
    /// Otherwise the core degrades honestly to a `DownloadFile` followed by an
    /// `UploadFile`, the two transfers a non-server-side copy actually needs.
    pub fn shaped_copy(caps: &TransferCapabilities) -> CopyDag {
        let api_slots = u16::from(caps.rate_limited_api.is_available());
        let server_side = caps.server_side_copy.is_available();
        let mut dag = TransferDag::default();

        let discover = dag.add_node(
            TransferNodeKind::DiscoverRemote,
            vec![],
            ResourceRequest::default(),
        );
        let acquire = dag.add_node(
            TransferNodeKind::AcquireResource,
            vec![discover],
            ResourceRequest::default(),
        );

        let mut copy = Vec::new();
        if server_side {
            // A server-side copy is one API operation; no local disk or buffer.
            copy.push(dag.add_node(
                TransferNodeKind::ServerSideCopy,
                vec![acquire],
                ResourceRequest::server_copy(api_slots),
            ));
        } else {
            let download = dag.add_node(
                TransferNodeKind::DownloadFile,
                vec![acquire],
                transfer_request(TransferDirection::Download, api_slots),
            );
            let upload = dag.add_node(
                TransferNodeKind::UploadFile,
                vec![download],
                transfer_request(TransferDirection::Upload, api_slots),
            );
            copy.push(download);
            copy.push(upload);
        }

        let verify = dag.add_node(
            TransferNodeKind::VerifyChecksum,
            vec![*copy.last().expect("copy core is never empty")],
            ResourceRequest::default(),
        );
        let preserve_metadata = dag.add_node(
            TransferNodeKind::PreserveMetadata,
            vec![verify],
            ResourceRequest::default(),
        );
        let commit = dag.add_node(
            TransferNodeKind::CommitTemp,
            vec![preserve_metadata],
            ResourceRequest::default(),
        );
        let emit_progress = dag.add_node(
            TransferNodeKind::EmitProgress,
            vec![commit],
            ResourceRequest::default(),
        );

        CopyDag {
            dag,
            server_side,
            discover,
            acquire,
            copy,
            verify,
            preserve_metadata,
            commit,
            emit_progress,
        }
    }
}

/// `ResourceRequest` for a whole-file transfer node, directional and optionally
/// rate-limited. Whole-file runners stream through the provider and do not
/// pre-allocate a known full-file buffer, so `buffer_bytes` stays 0.
fn transfer_request(direction: TransferDirection, api_slots: u16) -> ResourceRequest {
    match direction {
        TransferDirection::Upload => ResourceRequest::upload_file().with_api_slots(api_slots),
        TransferDirection::Download => ResourceRequest::download_file().with_api_slots(api_slots),
    }
}

/// `ResourceRequest` for one `UploadPart` node: chunk slot + disk read + the
/// exact maximum `Vec<u8>` this part may allocate (`buffer_bytes`), optionally
/// rate-limited. Sizing must match the runner's `read_chunk` formula via
/// [`multipart_part_byte_len`].
fn part_request(api_slots: u16, buffer_bytes: u64) -> ResourceRequest {
    ResourceRequest::upload_part(buffer_bytes).with_api_slots(api_slots)
}

/// Append the transfer core for one file: a single `UploadFile` /
/// `DownloadFile`, or N `UploadPart` nodes under `acquire_node`.
///
/// This is the **single source of truth** for capability-shaped multipart
/// topology across [`TransferDagBuilder::shaped_file`],
/// [`TransferDagBuilder::from_batch_shaped`], and
/// [`TransferDagBuilder::from_sync_plan_shaped`]:
///
/// - `max_chunk_slots <= 1` (or missing → effective 1): strict part-number
///   chain `acquire → part1 → part2 → … → partN` for ordering-sensitive
///   upload sessions (Drive, OneDrive, OpenDrive, …).
/// - `max_chunk_slots > 1`: fan-out `acquire → {part1, …, partN}` so the
///   executor may overlap parts up to the chunk budget (S3, B2, Azure, …).
///
/// Returns transfer node ids in **part-number order**. Does not invent
/// part numbers from global node ids; callers treat ids as opaque.
fn append_transfer_core(
    dag: &mut TransferDag,
    direction: TransferDirection,
    acquire_node: usize,
    profile: &TransferGraphProfile,
    file_size: u64,
) -> Vec<usize> {
    let mut transfer = Vec::new();
    if direction == TransferDirection::Upload && profile.upload_parts > 1 {
        // Providers that hit one chunk slot at a time (`max_chunk_slots
        // == 1`) usually require monotonic per-part ordering: Drive's
        // resumable session enforces a strictly increasing
        // `Content-Range`; OneDrive's Graph session does the same.
        // Without explicit inter-part dependencies the DAG executor is
        // free to dispatch any ready node, so even with a single
        // chunk slot we could see part 7 land before part 2. Chain
        // the `UploadPart` nodes (N depends on N-1) whenever
        // parallelism is 1 so the runner's lazy `begin` sees parts
        // in order. Backends with `max_chunk_slots > 1` (S3, B2,
        // Dropbox concurrent sessions, Box chunked v2) keep the
        // unconstrained fan-out shape so the runner can dispatch
        // chunk uploads in parallel up to `max_chunk_slots`.
        let serialise = profile.max_chunk_slots <= 1;
        for idx in 0..profile.upload_parts {
            let parent_dep = if serialise && idx > 0 {
                vec![transfer[idx - 1]]
            } else {
                vec![acquire_node]
            };
            let part_bytes = multipart_part_byte_len(
                file_size,
                idx,
                profile.upload_parts,
                profile.preferred_chunk_size,
            );
            transfer.push(dag.add_node(
                TransferNodeKind::UploadPart,
                parent_dep,
                part_request(profile.api_slots, part_bytes),
            ));
        }
    } else {
        let transfer_kind = match direction {
            TransferDirection::Download => TransferNodeKind::DownloadFile,
            TransferDirection::Upload => TransferNodeKind::UploadFile,
        };
        transfer.push(dag.add_node(
            transfer_kind,
            vec![acquire_node],
            transfer_request(direction, profile.api_slots),
        ));
    }
    transfer
}

#[derive(Debug, Clone, Copy)]
struct SingleFileNodeIds {
    discover: usize,
    acquire: usize,
    transfer: usize,
    verify: usize,
    preserve_metadata: usize,
    commit: usize,
    emit_progress: usize,
}

fn append_single_file_chain(
    dag: &mut TransferDag,
    direction: TransferDirection,
) -> SingleFileNodeIds {
    let (discover_kind, transfer_kind) = match direction {
        TransferDirection::Download => (
            TransferNodeKind::DiscoverRemote,
            TransferNodeKind::DownloadFile,
        ),
        TransferDirection::Upload => (
            TransferNodeKind::DiscoverLocal,
            TransferNodeKind::UploadFile,
        ),
    };

    let discover = dag.add_node(discover_kind, vec![], ResourceRequest::default());
    let acquire = dag.add_node(
        TransferNodeKind::AcquireResource,
        vec![discover],
        ResourceRequest::default(),
    );
    let transfer = dag.add_node(transfer_kind, vec![acquire], transfer_request(direction, 0));
    let verify = dag.add_node(
        TransferNodeKind::VerifyChecksum,
        vec![transfer],
        ResourceRequest::default(),
    );
    let preserve_metadata = dag.add_node(
        TransferNodeKind::PreserveMetadata,
        vec![verify],
        ResourceRequest::default(),
    );
    let commit = dag.add_node(
        TransferNodeKind::CommitTemp,
        vec![preserve_metadata],
        ResourceRequest::default(),
    );
    let emit_progress = dag.add_node(
        TransferNodeKind::EmitProgress,
        vec![commit],
        ResourceRequest::default(),
    );

    SingleFileNodeIds {
        discover,
        acquire,
        transfer,
        verify,
        preserve_metadata,
        commit,
        emit_progress,
    }
}

/// Node ids for one shaped sub-DAG appended into a larger graph.
///
/// Mirrors [`SingleFileNodeIds`] but with a `Vec` of transfer nodes so the
/// caller can preserve the multipart fan-out without re-scanning the graph.
#[derive(Debug, Clone)]
struct ShapedFileNodeIds {
    discover: usize,
    acquire: usize,
    transfer_nodes: Vec<usize>,
    verify: usize,
    preserve_metadata: usize,
    commit: usize,
    emit_progress: usize,
}

/// Append one capability-shaped file sub-DAG to `dag`.
///
/// Same node bindings as [`TransferDagBuilder::shaped_file`], but emitted in
/// place onto an existing graph so the batch (and, later, sync) builders can
/// stitch per-file sub-DAGs into a single [`TransferDag`]. Multipart topology
/// comes exclusively from [`append_transfer_core`].
fn append_shaped_file_chain(
    dag: &mut TransferDag,
    direction: TransferDirection,
    caps: &TransferCapabilities,
    file_size: u64,
) -> ShapedFileNodeIds {
    let profile = TransferGraphProfile::resolve(direction, caps, file_size);

    let discover_kind = match direction {
        TransferDirection::Download => TransferNodeKind::DiscoverRemote,
        TransferDirection::Upload => TransferNodeKind::DiscoverLocal,
    };
    let discover = dag.add_node(discover_kind, vec![], ResourceRequest::default());
    let acquire = dag.add_node(
        TransferNodeKind::AcquireResource,
        vec![discover],
        ResourceRequest::default(),
    );

    let transfer_nodes = append_transfer_core(dag, direction, acquire, &profile, file_size);

    let verify = dag.add_node(
        TransferNodeKind::VerifyChecksum,
        transfer_nodes.clone(),
        ResourceRequest::default(),
    );
    let preserve_metadata = dag.add_node(
        TransferNodeKind::PreserveMetadata,
        vec![verify],
        ResourceRequest::default(),
    );
    let commit = dag.add_node(
        TransferNodeKind::CommitTemp,
        vec![preserve_metadata],
        ResourceRequest::default(),
    );
    let emit_progress = dag.add_node(
        TransferNodeKind::EmitProgress,
        vec![commit],
        ResourceRequest::default(),
    );

    ShapedFileNodeIds {
        discover,
        acquire,
        transfer_nodes,
        verify,
        preserve_metadata,
        commit,
        emit_progress,
    }
}

/// Append one capability-shaped sync sub-DAG to `dag` below the global
/// `compare` join node.
///
/// Same transfer-core bindings as [`append_shaped_file_chain`] (via
/// [`append_transfer_core`]) but with `compare` substituted for the absent
/// per-file discover prefix, mirroring how [`append_sync_file_chain`] threads
/// the global sync prefix.
fn append_shaped_sync_file_chain(
    dag: &mut TransferDag,
    direction: TransferDirection,
    compare: usize,
    caps: &TransferCapabilities,
    file_size: u64,
) -> ShapedFileNodeIds {
    let profile = TransferGraphProfile::resolve(direction, caps, file_size);

    let acquire = dag.add_node(
        TransferNodeKind::AcquireResource,
        vec![compare],
        ResourceRequest::default(),
    );

    let transfer_nodes = append_transfer_core(dag, direction, acquire, &profile, file_size);

    let verify = dag.add_node(
        TransferNodeKind::VerifyChecksum,
        transfer_nodes.clone(),
        ResourceRequest::default(),
    );
    let preserve_metadata = dag.add_node(
        TransferNodeKind::PreserveMetadata,
        vec![verify],
        ResourceRequest::default(),
    );
    let commit = dag.add_node(
        TransferNodeKind::CommitTemp,
        vec![preserve_metadata],
        ResourceRequest::default(),
    );
    let emit_progress = dag.add_node(
        TransferNodeKind::EmitProgress,
        vec![commit],
        ResourceRequest::default(),
    );

    ShapedFileNodeIds {
        discover: compare,
        acquire,
        transfer_nodes,
        verify,
        preserve_metadata,
        commit,
        emit_progress,
    }
}

fn append_sync_file_chain(
    dag: &mut TransferDag,
    direction: TransferDirection,
    compare: usize,
) -> SingleFileNodeIds {
    let transfer_kind = match direction {
        TransferDirection::Download => TransferNodeKind::DownloadFile,
        TransferDirection::Upload => TransferNodeKind::UploadFile,
    };

    let acquire = dag.add_node(
        TransferNodeKind::AcquireResource,
        vec![compare],
        ResourceRequest::default(),
    );
    let transfer = dag.add_node(transfer_kind, vec![acquire], transfer_request(direction, 0));
    let verify = dag.add_node(
        TransferNodeKind::VerifyChecksum,
        vec![transfer],
        ResourceRequest::default(),
    );
    let preserve_metadata = dag.add_node(
        TransferNodeKind::PreserveMetadata,
        vec![verify],
        ResourceRequest::default(),
    );
    let commit = dag.add_node(
        TransferNodeKind::CommitTemp,
        vec![preserve_metadata],
        ResourceRequest::default(),
    );
    let emit_progress = dag.add_node(
        TransferNodeKind::EmitProgress,
        vec![commit],
        ResourceRequest::default(),
    );

    SingleFileNodeIds {
        discover: compare,
        acquire,
        transfer,
        verify,
        preserve_metadata,
        commit,
        emit_progress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
    use crate::transfer_dag::graph::TransferNode;
    use crate::transfer_dag::observer::NoopDagObserver;
    use crate::transfer_dag::resources::{TransferBudget, TransferResourceManager};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn download_graph_has_seven_nodes_in_order() {
        let built = TransferDagBuilder::single_file(TransferDirection::Download);
        let nodes = built.dag.nodes();

        assert_eq!(nodes.len(), 7);
        assert_eq!(nodes[0].kind, TransferNodeKind::DiscoverRemote);
        assert_eq!(nodes[1].kind, TransferNodeKind::AcquireResource);
        assert_eq!(nodes[2].kind, TransferNodeKind::DownloadFile);
        assert_eq!(nodes[3].kind, TransferNodeKind::VerifyChecksum);
        assert_eq!(nodes[4].kind, TransferNodeKind::PreserveMetadata);
        assert_eq!(nodes[5].kind, TransferNodeKind::CommitTemp);
        assert_eq!(nodes[6].kind, TransferNodeKind::EmitProgress);
    }

    #[test]
    fn upload_graph_swaps_only_the_discover_and_transfer_kinds() {
        let built = TransferDagBuilder::single_file(TransferDirection::Upload);
        let nodes = built.dag.nodes();

        assert_eq!(nodes.len(), 7);
        assert_eq!(nodes[0].kind, TransferNodeKind::DiscoverLocal);
        assert_eq!(nodes[1].kind, TransferNodeKind::AcquireResource);
        assert_eq!(nodes[2].kind, TransferNodeKind::UploadFile);
        assert_eq!(nodes[3].kind, TransferNodeKind::VerifyChecksum);
        assert_eq!(nodes[4].kind, TransferNodeKind::PreserveMetadata);
        assert_eq!(nodes[5].kind, TransferNodeKind::CommitTemp);
        assert_eq!(nodes[6].kind, TransferNodeKind::EmitProgress);
    }

    #[test]
    fn nodes_form_a_strict_linear_chain() {
        let built = TransferDagBuilder::single_file(TransferDirection::Download);
        let nodes = built.dag.nodes();

        assert_eq!(nodes[0].depends_on, Vec::<usize>::new());
        for (i, node) in nodes.iter().enumerate().skip(1) {
            assert_eq!(
                node.depends_on,
                vec![i - 1],
                "node {i} must depend solely on node {}",
                i - 1
            );
        }
    }

    #[test]
    fn named_ids_match_node_positions() {
        let built = TransferDagBuilder::single_file(TransferDirection::Download);

        assert_eq!(built.discover, 0);
        assert_eq!(built.acquire, 1);
        assert_eq!(built.transfer, 2);
        assert_eq!(built.verify, 3);
        assert_eq!(built.preserve_metadata, 4);
        assert_eq!(built.commit, 5);
        assert_eq!(built.emit_progress, 6);
    }

    #[test]
    fn only_the_transfer_node_carries_a_scarce_resource() {
        let built = TransferDagBuilder::single_file(TransferDirection::Upload);
        let nodes = built.dag.nodes();

        for node in nodes {
            if node.id == built.transfer {
                assert_eq!(node.resources, ResourceRequest::upload_file());
            } else {
                assert_eq!(
                    node.resources,
                    ResourceRequest::default(),
                    "node {} ({:?}) must not reserve a resource",
                    node.id,
                    node.kind
                );
            }
        }
    }

    #[test]
    fn single_file_requests_are_directional() {
        let up = TransferDagBuilder::single_file(TransferDirection::Upload);
        let down = TransferDagBuilder::single_file(TransferDirection::Download);
        assert_eq!(
            up.dag.nodes()[up.transfer].resources,
            ResourceRequest::upload_file()
        );
        assert_eq!(
            down.dag.nodes()[down.transfer].resources,
            ResourceRequest::download_file()
        );
        assert_eq!(up.dag.nodes()[up.transfer].resources.disk_write_slots, 0);
        assert_eq!(down.dag.nodes()[down.transfer].resources.disk_read_slots, 0);
    }

    #[tokio::test]
    async fn built_graph_runs_to_completion_on_the_executor() {
        // The graph must be schedulable end-to-end: a noop runner over a
        // minimal budget completes all seven nodes in dependency order.
        let built = TransferDagBuilder::single_file(TransferDirection::Download);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_node: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Completed })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));

        let summary = execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("single-file graph must schedule cleanly");

        assert_eq!(summary.nodes_completed, 7);
        assert_eq!(summary.nodes_failed, 0);
    }

    #[test]
    fn empty_batch_builds_empty_graph() {
        let built = TransferDagBuilder::from_batch(&[]);

        assert!(built.dag.nodes().is_empty());
        assert!(built.files.is_empty());
    }

    #[test]
    fn batch_merges_one_subdag_per_item() {
        let built = TransferDagBuilder::from_batch(&[
            BatchDagItem::new("a.txt", TransferDirection::Upload),
            BatchDagItem::new("b.txt", TransferDirection::Download),
        ]);
        let nodes = built.dag.nodes();

        assert_eq!(nodes.len(), 14);
        assert_eq!(built.files.len(), 2);
        assert_eq!(built.files[0].key, "a.txt");
        assert_eq!(built.files[0].direction, TransferDirection::Upload);
        assert_eq!(built.files[0].discover, 0);
        assert_eq!(built.files[0].transfer, 2);
        assert_eq!(built.files[0].emit_progress, 6);
        assert_eq!(
            nodes[built.files[0].discover].kind,
            TransferNodeKind::DiscoverLocal
        );
        assert_eq!(
            nodes[built.files[0].transfer].kind,
            TransferNodeKind::UploadFile
        );

        assert_eq!(built.files[1].key, "b.txt");
        assert_eq!(built.files[1].direction, TransferDirection::Download);
        assert_eq!(built.files[1].discover, 7);
        assert_eq!(built.files[1].transfer, 9);
        assert_eq!(built.files[1].emit_progress, 13);
        assert_eq!(
            nodes[built.files[1].discover].kind,
            TransferNodeKind::DiscoverRemote
        );
        assert_eq!(
            nodes[built.files[1].transfer].kind,
            TransferNodeKind::DownloadFile
        );
    }

    #[test]
    fn batch_subdags_are_independent_but_transfer_nodes_are_scarce() {
        let built = TransferDagBuilder::from_batch(&[
            BatchDagItem::new("one", TransferDirection::Upload),
            BatchDagItem::new("two", TransferDirection::Upload),
            BatchDagItem::new("three", TransferDirection::Upload),
        ]);
        let nodes = built.dag.nodes();

        for file in &built.files {
            assert!(nodes[file.discover].depends_on.is_empty());
            assert_eq!(nodes[file.acquire].depends_on, vec![file.discover]);
            assert_eq!(nodes[file.transfer].depends_on, vec![file.acquire]);
            assert_eq!(
                nodes[file.transfer].resources,
                ResourceRequest::upload_file()
            );
            assert_eq!(nodes[file.emit_progress].depends_on, vec![file.commit]);
        }
    }

    #[tokio::test]
    async fn batch_file_slots_serialize_real_transfer_nodes() {
        let built = TransferDagBuilder::from_batch(&[
            BatchDagItem::new("one", TransferDirection::Download),
            BatchDagItem::new("two", TransferDirection::Download),
            BatchDagItem::new("three", TransferDirection::Download),
        ]);
        let in_flight_transfers = Arc::new(AtomicUsize::new(0));
        let peak_transfers = Arc::new(AtomicUsize::new(0));
        let runner: Arc<dyn DagNodeRunner> = {
            let in_flight_transfers = Arc::clone(&in_flight_transfers);
            let peak_transfers = Arc::clone(&peak_transfers);
            Arc::new(move |node: TransferNode| -> NodeFuture {
                let in_flight_transfers = Arc::clone(&in_flight_transfers);
                let peak_transfers = Arc::clone(&peak_transfers);
                Box::pin(async move {
                    if matches!(
                        node.kind,
                        TransferNodeKind::DownloadFile | TransferNodeKind::UploadFile
                    ) {
                        let now = in_flight_transfers.fetch_add(1, Ordering::SeqCst) + 1;
                        peak_transfers.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        in_flight_transfers.fetch_sub(1, Ordering::SeqCst);
                    }
                    NodeOutcome::Completed
                })
            })
        };
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));

        let summary = execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("batch graph must schedule cleanly");

        assert_eq!(summary.nodes_completed, 21);
        assert_eq!(peak_transfers.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn batch_file_slots_allow_transfer_overlap_when_budget_allows() {
        let built = TransferDagBuilder::from_batch(&[
            BatchDagItem::new("one", TransferDirection::Upload),
            BatchDagItem::new("two", TransferDirection::Upload),
            BatchDagItem::new("three", TransferDirection::Upload),
        ]);
        let in_flight_transfers = Arc::new(AtomicUsize::new(0));
        let peak_transfers = Arc::new(AtomicUsize::new(0));
        let runner: Arc<dyn DagNodeRunner> = {
            let in_flight_transfers = Arc::clone(&in_flight_transfers);
            let peak_transfers = Arc::clone(&peak_transfers);
            Arc::new(move |node: TransferNode| -> NodeFuture {
                let in_flight_transfers = Arc::clone(&in_flight_transfers);
                let peak_transfers = Arc::clone(&peak_transfers);
                Box::pin(async move {
                    if matches!(
                        node.kind,
                        TransferNodeKind::DownloadFile | TransferNodeKind::UploadFile
                    ) {
                        let now = in_flight_transfers.fetch_add(1, Ordering::SeqCst) + 1;
                        peak_transfers.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        in_flight_transfers.fetch_sub(1, Ordering::SeqCst);
                    }
                    NodeOutcome::Completed
                })
            })
        };
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(2));

        execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("batch graph must schedule cleanly");

        assert_eq!(peak_transfers.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn shaped_batch_with_default_caps_matches_legacy_shape() {
        // Default `TransferCapabilities` advertise nothing: the shaped
        // batch builder must reproduce the same node count and the same
        // single-transfer-core shape as `from_batch`.
        let items = vec![
            BatchDagItem::with_size("a.txt", TransferDirection::Upload, 0),
            BatchDagItem::with_size("b.txt", TransferDirection::Download, 0),
        ];
        let legacy = TransferDagBuilder::from_batch(&items);
        let shaped =
            TransferDagBuilder::from_batch_shaped(&items, &TransferCapabilities::default());

        assert_eq!(legacy.dag.nodes().len(), shaped.dag.nodes().len());
        assert_eq!(legacy.files.len(), shaped.files.len());
        for (l, s) in legacy.files.iter().zip(shaped.files.iter()) {
            assert_eq!(l.transfer_nodes, s.transfer_nodes);
            assert_eq!(s.transfer_nodes.len(), 1);
            assert_eq!(s.transfer, s.transfer_nodes[0]);
        }
    }

    #[test]
    fn shaped_batch_with_multipart_caps_fans_out_uploads() {
        // A multipart-capable cap set with max_chunk_slots > 1 + 24 MiB
        // upload over 8 MiB chunks produces 3 `UploadPart` nodes for the
        // upload item and a single `DownloadFile` for the download item.
        // All three part nodes share the same `acquire` predecessor (fan-out).
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(8 * 1024 * 1024),
            max_chunk_slots: Some(4),
            multipart_threshold: 0, // unset: fan out at chunk size
            ..TransferCapabilities::default()
        };
        let items = vec![
            BatchDagItem::with_size("big.bin", TransferDirection::Upload, 24 * 1024 * 1024),
            BatchDagItem::with_size("small.txt", TransferDirection::Download, 1024),
        ];
        let built = TransferDagBuilder::from_batch_shaped(&items, &caps);

        let upload = &built.files[0];
        assert_eq!(upload.transfer_nodes.len(), 3);
        assert_eq!(upload.transfer, upload.transfer_nodes[0]);
        let nodes = built.dag.nodes();
        for &part_id in &upload.transfer_nodes {
            assert_eq!(nodes[part_id].kind, TransferNodeKind::UploadPart);
            assert_eq!(nodes[part_id].depends_on, vec![upload.acquire]);
        }
        // `verify` must wait for every `UploadPart` node before it can run.
        let mut expected_deps = upload.transfer_nodes.clone();
        expected_deps.sort_unstable();
        let mut actual_deps = nodes[upload.verify].depends_on.clone();
        actual_deps.sort_unstable();
        assert_eq!(actual_deps, expected_deps);

        let download = &built.files[1];
        assert_eq!(download.transfer_nodes.len(), 1);
        assert_eq!(
            nodes[download.transfer_nodes[0]].kind,
            TransferNodeKind::DownloadFile
        );
    }

    #[test]
    fn shaped_ranges_emits_one_download_range_node_per_segment() {
        let built = TransferDagBuilder::shaped_ranges(4);
        let nodes = built.dag.nodes();
        assert_eq!(built.transfer.len(), 4);
        assert_eq!(nodes.len(), 4);
        for &id in &built.transfer {
            assert_eq!(nodes[id].kind, TransferNodeKind::DownloadRange);
            assert!(
                nodes[id].depends_on.is_empty(),
                "segments are independent: no inter-range dependencies"
            );
            assert_eq!(nodes[id].resources, ResourceRequest::range_chunk());
        }
        // Node ids match creation order, so callers can index their range
        // plan by `node.id` (the segmented-download runner relies on this).
        for (i, &id) in built.transfer.iter().enumerate() {
            assert_eq!(id, i);
        }
    }

    #[test]
    fn shaped_ranges_zero_segments_produces_empty_graph() {
        let built = TransferDagBuilder::shaped_ranges(0);
        assert!(built.transfer.is_empty());
        assert!(built.dag.nodes().is_empty());
    }

    #[test]
    fn shaped_sync_plan_with_default_caps_matches_legacy_shape() {
        // Default caps reproduce the legacy `from_sync_plan` graph byte-
        // identically: same node count, same single-core transfer shape per
        // file, same global discover/compare prefix.
        let items = vec![
            SyncDagItem::with_size("a.txt", SyncDagAction::Upload, 0),
            SyncDagItem::with_size("b.txt", SyncDagAction::Download, 0),
        ];
        let legacy = TransferDagBuilder::from_sync_plan(&items);
        let shaped =
            TransferDagBuilder::from_sync_plan_shaped(&items, &TransferCapabilities::default());
        assert_eq!(legacy.dag.nodes().len(), shaped.dag.nodes().len());
        assert_eq!(shaped.files.len(), 2);
        for (l, s) in legacy.files.iter().zip(shaped.files.iter()) {
            assert_eq!(s.transfer_nodes.len(), 1);
            assert_eq!(s.transfer, s.transfer_nodes[0]);
            assert_eq!(l.transfer, s.transfer);
        }
    }

    #[test]
    fn shaped_sync_plan_fans_out_multipart_uploads() {
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(8 * 1024 * 1024),
            max_chunk_slots: Some(4),
            multipart_threshold: 0, // unset: fan out at chunk size
            ..TransferCapabilities::default()
        };
        let items = vec![
            SyncDagItem::with_size("big.bin", SyncDagAction::Upload, 24 * 1024 * 1024),
            SyncDagItem::with_size("small.txt", SyncDagAction::Download, 16 * 1024 * 1024),
        ];
        let built = TransferDagBuilder::from_sync_plan_shaped(&items, &caps);
        assert_eq!(built.files.len(), 2);

        let upload = &built.files[0];
        assert_eq!(upload.transfer_nodes.len(), 3);
        let nodes = built.dag.nodes();
        for &id in &upload.transfer_nodes {
            assert_eq!(nodes[id].kind, TransferNodeKind::UploadPart);
            assert_eq!(nodes[id].depends_on, vec![upload.acquire]);
        }

        // Multipart is upload-only; the download item stays a single
        // `DownloadFile` even on a multipart-capable cap set.
        let download = &built.files[1];
        assert_eq!(download.transfer_nodes.len(), 1);
        assert_eq!(
            nodes[download.transfer_nodes[0]].kind,
            TransferNodeKind::DownloadFile
        );
    }

    #[test]
    fn sync_plan_builds_global_discover_compare_prefix() {
        let built = TransferDagBuilder::from_sync_plan(&[]);
        let nodes = built.dag.nodes();

        assert_eq!(nodes.len(), 3);
        assert_eq!(built.discover_local, 0);
        assert_eq!(built.discover_remote, 1);
        assert_eq!(built.compare, 2);
        assert_eq!(
            nodes[built.discover_local].kind,
            TransferNodeKind::DiscoverLocal
        );
        assert_eq!(
            nodes[built.discover_remote].kind,
            TransferNodeKind::DiscoverRemote
        );
        assert_eq!(nodes[built.compare].kind, TransferNodeKind::Compare);
        assert_eq!(
            nodes[built.compare].depends_on,
            vec![built.discover_local, built.discover_remote]
        );
        assert!(built.files.is_empty());
    }

    #[test]
    fn sync_plan_adds_transfer_chains_under_compare() {
        let built = TransferDagBuilder::from_sync_plan(&[
            SyncDagItem::new("upload:a.txt", SyncDagAction::Upload),
            SyncDagItem::new("download:b.txt", SyncDagAction::Download),
            SyncDagItem::new("skip:c.txt", SyncDagAction::Skip),
            SyncDagItem::new("delete:d.txt", SyncDagAction::DeleteRemote),
        ]);
        let nodes = built.dag.nodes();

        assert_eq!(built.files.len(), 2);
        assert_eq!(nodes.len(), 15);

        let upload = &built.files[0];
        assert_eq!(upload.key, "upload:a.txt");
        assert_eq!(upload.plan_index, 0);
        assert_eq!(upload.action, SyncDagAction::Upload);
        assert_eq!(upload.direction, TransferDirection::Upload);
        assert_eq!(nodes[upload.acquire].depends_on, vec![built.compare]);
        assert_eq!(nodes[upload.transfer].kind, TransferNodeKind::UploadFile);
        assert_eq!(
            nodes[upload.transfer].resources,
            ResourceRequest::upload_file()
        );
        assert_eq!(nodes[upload.emit_progress].depends_on, vec![upload.commit]);

        let download = &built.files[1];
        assert_eq!(download.key, "download:b.txt");
        assert_eq!(download.plan_index, 1);
        assert_eq!(download.action, SyncDagAction::Download);
        assert_eq!(download.direction, TransferDirection::Download);
        assert_eq!(nodes[download.acquire].depends_on, vec![built.compare]);
        assert_eq!(
            nodes[download.transfer].kind,
            TransferNodeKind::DownloadFile
        );
        assert_eq!(
            nodes[download.transfer].resources,
            ResourceRequest::download_file()
        );
        assert_eq!(
            nodes[download.emit_progress].depends_on,
            vec![download.commit]
        );
    }

    #[test]
    fn sync_plan_exposes_terminal_to_legacy_journal_indices() {
        let built = TransferDagBuilder::from_sync_plan(&[
            SyncDagItem::new("upload:a.txt", SyncDagAction::Upload),
            SyncDagItem::new("skip:b.txt", SyncDagAction::Skip),
            SyncDagItem::new("download:c.txt", SyncDagAction::Download),
        ]);

        let terminals = built.journal_terminals_for_plan_order();

        assert_eq!(terminals.len(), 2);
        assert_eq!(terminals[0].node_id, built.files[0].emit_progress);
        assert_eq!(terminals[0].entry_index, 0);
        assert_eq!(terminals[1].node_id, built.files[1].emit_progress);
        assert_eq!(terminals[1].entry_index, 2);
    }

    #[tokio::test]
    async fn sync_file_slots_limit_only_transfer_nodes_after_compare() {
        let built = TransferDagBuilder::from_sync_plan(&[
            SyncDagItem::new("one", SyncDagAction::Upload),
            SyncDagItem::new("two", SyncDagAction::Download),
            SyncDagItem::new("three", SyncDagAction::Upload),
        ]);
        let in_flight_transfers = Arc::new(AtomicUsize::new(0));
        let peak_transfers = Arc::new(AtomicUsize::new(0));
        let runner: Arc<dyn DagNodeRunner> = {
            let in_flight_transfers = Arc::clone(&in_flight_transfers);
            let peak_transfers = Arc::clone(&peak_transfers);
            Arc::new(move |node: TransferNode| -> NodeFuture {
                let in_flight_transfers = Arc::clone(&in_flight_transfers);
                let peak_transfers = Arc::clone(&peak_transfers);
                Box::pin(async move {
                    if matches!(
                        node.kind,
                        TransferNodeKind::DownloadFile | TransferNodeKind::UploadFile
                    ) {
                        let now = in_flight_transfers.fetch_add(1, Ordering::SeqCst) + 1;
                        peak_transfers.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        in_flight_transfers.fetch_sub(1, Ordering::SeqCst);
                    }
                    NodeOutcome::Completed
                })
            })
        };
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));

        let summary = execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("sync graph must schedule cleanly");

        assert_eq!(summary.nodes_completed, 21);
        assert_eq!(peak_transfers.load(Ordering::SeqCst), 1);
    }

    // ---- Phase 3: capability-shaped graphs --------------------------------

    use crate::transfer_dag::Capability;

    fn caps_multipart(chunk_size: u64) -> TransferCapabilities {
        TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(chunk_size),
            max_chunk_slots: Some(4),
            multipart_threshold: 0, // unset: fan out at chunk size
            ..TransferCapabilities::default()
        }
    }

    #[test]
    fn shaped_download_is_the_classic_seven_node_chain() {
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Download,
            &TransferCapabilities::default(),
            64 * 1024 * 1024,
        );
        let nodes = built.dag.nodes();

        assert_eq!(nodes.len(), 7);
        assert_eq!(built.transfer.len(), 1);
        assert_eq!(
            nodes[built.transfer[0]].kind,
            TransferNodeKind::DownloadFile
        );
        assert_eq!(built.profile.upload_parts, 1);
        assert!(!built.profile.resume);
        assert_eq!(built.profile.api_slots, 0);
        assert_eq!(built.profile.max_chunk_slots, 1);
        assert_eq!(nodes[built.verify].depends_on, built.transfer);
    }

    #[test]
    fn shaped_upload_without_multipart_is_a_single_upload_node() {
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &TransferCapabilities::default(),
            500 * 1024 * 1024,
        );

        assert_eq!(built.transfer.len(), 1);
        assert_eq!(
            built.dag.nodes()[built.transfer[0]].kind,
            TransferNodeKind::UploadFile
        );
    }

    #[test]
    fn shaped_upload_with_multipart_fans_out_upload_parts() {
        // 5 MiB file, 1 MiB parts: five parallel UploadPart nodes.
        let file_size = 5 * 1024 * 1024;
        let chunk = 1024 * 1024;
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart(chunk),
            file_size,
        );
        let nodes = built.dag.nodes();

        assert_eq!(built.profile.upload_parts, 5);
        assert_eq!(built.profile.max_chunk_slots, 4);
        assert_eq!(built.transfer.len(), 5);
        for (idx, &part) in built.transfer.iter().enumerate() {
            assert_eq!(nodes[part].kind, TransferNodeKind::UploadPart);
            assert_eq!(nodes[part].depends_on, vec![built.acquire]);
            assert_eq!(nodes[part].resources.chunk_slots, 1);
            assert_eq!(nodes[part].resources.disk_read_slots, 1);
            assert_eq!(nodes[part].resources.disk_write_slots, 0);
            assert_eq!(
                nodes[part].resources.buffer_bytes,
                multipart_part_byte_len(file_size, idx, 5, chunk)
            );
        }
        // VerifyChecksum joins every part: it cannot run until the last lands.
        assert_eq!(nodes[built.verify].depends_on, built.transfer);
        // discover + acquire + 5 parts + verify + preserve + commit + emit.
        assert_eq!(nodes.len(), 11);
    }

    #[test]
    fn shaped_multipart_accounts_short_tail_and_no_disk_write() {
        let file_size = 25 * 1024 * 1024;
        let chunk = 8 * 1024 * 1024;
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart(chunk),
            file_size,
        );
        let nodes = built.dag.nodes();
        let parts = built.profile.upload_parts;
        assert_eq!(parts, 4);
        let last = built.transfer[parts - 1];
        assert_eq!(
            nodes[last].resources.buffer_bytes,
            file_size - (parts as u64 - 1) * chunk
        );
        assert!(nodes[last].resources.buffer_bytes < chunk);
        for &id in &built.transfer {
            assert_eq!(nodes[id].resources.disk_write_slots, 0);
            assert_eq!(nodes[id].resources.disk_read_slots, 1);
        }
    }

    #[test]
    fn shaped_download_and_range_and_copy_requests_are_directional() {
        let down = TransferDagBuilder::shaped_file(
            TransferDirection::Download,
            &TransferCapabilities::default(),
            1024,
        );
        let tr = &down.dag.nodes()[down.transfer[0]].resources;
        assert_eq!(*tr, ResourceRequest::download_file());
        assert_eq!(tr.disk_read_slots, 0);
        assert_eq!(tr.disk_write_slots, 1);

        let ranges = TransferDagBuilder::shaped_ranges(2);
        for &id in &ranges.transfer {
            let r = &ranges.dag.nodes()[id].resources;
            assert_eq!(*r, ResourceRequest::range_chunk());
            assert_eq!(r.disk_read_slots, 0);
            assert_eq!(r.buffer_bytes, 0);
        }

        let server = TransferDagBuilder::shaped_copy(&TransferCapabilities {
            server_side_copy: Capability::Supported,
            ..TransferCapabilities::default()
        });
        let sc = &server.dag.nodes()[server.copy[0]].resources;
        assert_eq!(sc.disk_read_slots, 0);
        assert_eq!(sc.disk_write_slots, 0);
        assert_eq!(sc.file_slots, 0);
        assert!(sc.api_slots >= 1);

        let fallback = TransferDagBuilder::shaped_copy(&TransferCapabilities::default());
        assert_eq!(
            fallback.dag.nodes()[fallback.copy[0]].resources,
            ResourceRequest::download_file()
        );
        assert_eq!(
            fallback.dag.nodes()[fallback.copy[1]].resources,
            ResourceRequest::upload_file()
        );
    }

    #[test]
    fn shaped_upload_with_single_chunk_slot_chains_upload_parts() {
        // OpenDrive-style backends require in-order chunks. When the provider
        // caps chunk parallelism at 1, each UploadPart must depend on the
        // previous part, not merely compete for one semaphore permit.
        let mut caps = caps_multipart(1024 * 1024);
        caps.max_chunk_slots = Some(1);

        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 3 * 1024 * 1024);
        let nodes = built.dag.nodes();

        assert_eq!(built.profile.upload_parts, 3);
        assert_eq!(built.profile.max_chunk_slots, 1);
        assert_eq!(built.transfer.len(), 3);
        assert_eq!(nodes[built.transfer[0]].depends_on, vec![built.acquire]);
        assert_eq!(nodes[built.transfer[1]].depends_on, vec![built.transfer[0]]);
        assert_eq!(nodes[built.transfer[2]].depends_on, vec![built.transfer[1]]);
        assert_eq!(nodes[built.verify].depends_on, built.transfer);
    }

    #[test]
    fn shaped_upload_grows_chunk_when_parts_clamp_to_cover_the_tail() {
        // A file larger than MAX_MULTIPART_PARTS * chunk_hint would otherwise
        // clamp upload_parts and leave the tail unscheduled (the runner reads
        // exactly preferred_chunk_size bytes per part). The shaping must grow
        // the effective chunk so the parts always cover the whole file.
        const MIB: u64 = 1024 * 1024;
        let chunk = MIB;
        let file_size = 20_000 * MIB; // > MAX_MULTIPART_PARTS (10_000) * 1 MiB
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart(chunk),
            file_size,
        );

        // Parts are clamped to the hard cap, but the chunk grew to compensate.
        assert_eq!(built.profile.upload_parts, MAX_MULTIPART_PARTS);
        assert!(
            built.profile.preferred_chunk_size > chunk,
            "chunk must grow past the hint when clamped"
        );
        // The invariant that prevents tail loss: parts * chunk >= file_size.
        let covered = built.profile.upload_parts as u64 * built.profile.preferred_chunk_size;
        assert!(
            covered >= file_size,
            "parts ({}) * chunk ({}) = {} must cover file_size {}",
            built.profile.upload_parts,
            built.profile.preferred_chunk_size,
            covered,
            file_size
        );
    }

    #[test]
    fn shaped_upload_keeps_provider_chunk_when_not_clamped() {
        // The clamp-compensation must be a no-op in the common case: when the
        // part count fits under the cap, the provider's advertised chunk size
        // is honoured verbatim (alignment contracts depend on it).
        let chunk = 8 * 1024 * 1024;
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart(chunk),
            80 * 1024 * 1024, // 10 parts, well under the cap
        );
        assert_eq!(built.profile.upload_parts, 10);
        assert_eq!(built.profile.preferred_chunk_size, chunk);
    }

    #[test]
    fn shaped_upload_multipart_collapses_when_the_file_fits_one_chunk() {
        // 512 KiB file, 1 MiB parts: a single UploadFile, no fan-out.
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart(1024 * 1024),
            512 * 1024,
        );

        assert_eq!(built.profile.upload_parts, 1);
        assert_eq!(built.transfer.len(), 1);
        assert_eq!(
            built.dag.nodes()[built.transfer[0]].kind,
            TransferNodeKind::UploadFile
        );
    }

    #[test]
    fn shaped_upload_below_multipart_threshold_stays_single_put() {
        // A multipart-capable provider that declares a 200 MiB threshold must
        // keep a 100 MiB upload as a single UploadFile node, exactly like the
        // legacy upload() single-PUT decision. Before the fix the DAG fanned
        // out any file larger than one chunk regardless of threshold (audit
        // DISP-01 / CORR-02).
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(16 * 1024 * 1024),
            max_chunk_slots: Some(4),
            multipart_threshold: 200 * 1024 * 1024,
            ..TransferCapabilities::default()
        };
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 100 * 1024 * 1024);
        assert_eq!(
            built.profile.upload_parts, 1,
            "below threshold => single PUT"
        );
        assert_eq!(built.transfer.len(), 1);
        assert_eq!(
            built.dag.nodes()[built.transfer[0]].kind,
            TransferNodeKind::UploadFile
        );
    }

    #[test]
    fn shaped_upload_at_or_above_multipart_threshold_fans_out() {
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(16 * 1024 * 1024),
            max_chunk_slots: Some(4),
            multipart_threshold: 200 * 1024 * 1024,
            ..TransferCapabilities::default()
        };
        // 256 MiB >= 200 MiB threshold: fans out into ceil(256/16) = 16 parts.
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 256 * 1024 * 1024);
        assert_eq!(built.profile.upload_parts, 16);
        assert_eq!(built.transfer.len(), 16);
    }

    #[test]
    fn shaped_upload_threshold_zero_falls_back_to_chunk_size() {
        // multipart_threshold == 0 means "unset": preserve the historical
        // "fan out any file larger than one part" behaviour.
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(8 * 1024 * 1024),
            max_chunk_slots: Some(4),
            multipart_threshold: 0,
            ..TransferCapabilities::default()
        };
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 24 * 1024 * 1024);
        assert_eq!(built.profile.upload_parts, 3);
    }

    #[test]
    fn shaped_profile_marks_resume_from_capabilities() {
        let caps = TransferCapabilities {
            resume_download: Capability::Supported,
            ..TransferCapabilities::default()
        };
        let download = TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 1024);
        assert!(download.profile.resume);
        // resume_upload is not advertised: the upload profile stays plain.
        let upload = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 1024);
        assert!(!upload.profile.resume);
    }

    #[test]
    fn shaped_profile_reserves_api_slot_for_rate_limited_providers() {
        let caps = TransferCapabilities {
            rate_limited_api: Capability::Supported,
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 1024);

        assert_eq!(built.profile.api_slots, 1);
        assert_eq!(built.dag.nodes()[built.transfer[0]].resources.api_slots, 1);
    }

    #[test]
    fn shaped_copy_collapses_into_one_server_side_copy_node() {
        let caps = TransferCapabilities {
            server_side_copy: Capability::Supported,
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_copy(&caps);
        let nodes = built.dag.nodes();

        assert!(built.server_side);
        assert_eq!(built.copy.len(), 1);
        assert_eq!(nodes[built.copy[0]].kind, TransferNodeKind::ServerSideCopy);
        // A server-side copy reserves no file or disk slot, only an api slot.
        assert_eq!(nodes[built.copy[0]].resources.file_slots, 0);
        assert_eq!(nodes[built.copy[0]].resources.api_slots, 1);
        // discover + acquire + copy + verify + preserve + commit + emit.
        assert_eq!(nodes.len(), 7);
    }

    #[test]
    fn shaped_copy_degrades_to_download_then_upload() {
        let built = TransferDagBuilder::shaped_copy(&TransferCapabilities::default());
        let nodes = built.dag.nodes();

        assert!(!built.server_side);
        assert_eq!(built.copy.len(), 2);
        assert_eq!(nodes[built.copy[0]].kind, TransferNodeKind::DownloadFile);
        assert_eq!(nodes[built.copy[1]].kind, TransferNodeKind::UploadFile);
        // The two transfers are a strict chain: upload after download.
        assert_eq!(nodes[built.copy[1]].depends_on, vec![built.copy[0]]);
        assert_eq!(nodes[built.verify].depends_on, vec![built.copy[1]]);
    }

    #[tokio::test]
    async fn shaped_multipart_graph_runs_to_completion_on_the_executor() {
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart(1024 * 1024),
            4 * 1024 * 1024,
        );
        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_node: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Completed })
        });
        // chunk_slots sized for the four parts to overlap.
        let manager = TransferResourceManager::new(TransferBudget {
            chunk_slots: 4,
            ..TransferBudget::from_file_slots(1)
        });

        let summary = execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("multipart graph must schedule cleanly");

        // discover + acquire + 4 parts + verify + preserve + commit + emit.
        assert_eq!(summary.nodes_completed, 10);
        assert_eq!(summary.nodes_failed, 0);
    }

    // ---- DAG-P0-07: serial multipart topology on all shaped surfaces ------

    /// Assert a strict part-number chain under `acquire` for one file's
    /// transfer core: part 0 → acquire, part N>0 → only part N-1, verify joins
    /// every part. Node ids are treated as opaque builder outputs.
    fn assert_serial_part_chain(
        nodes: &[TransferNode],
        acquire: usize,
        transfer_nodes: &[usize],
        verify: usize,
    ) {
        assert!(
            transfer_nodes.len() > 1,
            "serial chain gate needs multipart (>1 part)"
        );
        assert_eq!(nodes[transfer_nodes[0]].depends_on, vec![acquire]);
        for idx in 1..transfer_nodes.len() {
            assert_eq!(
                nodes[transfer_nodes[idx]].depends_on,
                vec![transfer_nodes[idx - 1]],
                "part {idx} must depend only on previous part"
            );
        }
        assert_eq!(nodes[verify].depends_on, transfer_nodes.to_vec());
    }

    /// Assert fan-out: every part depends directly on that file's acquire.
    fn assert_fanout_parts(nodes: &[TransferNode], acquire: usize, transfer_nodes: &[usize]) {
        assert!(transfer_nodes.len() > 1);
        for &part in transfer_nodes {
            assert_eq!(
                nodes[part].depends_on,
                vec![acquire],
                "cap>1 fan-out: every part depends only on acquire"
            );
        }
    }

    /// P0-06 invariants that must hold on every shaped surface for multipart.
    fn assert_p0_06_part_accounting(
        nodes: &[TransferNode],
        transfer_nodes: &[usize],
        file_size: u64,
        parts: usize,
        chunk: u64,
        api_slots: u16,
    ) {
        for (idx, &part) in transfer_nodes.iter().enumerate() {
            let r = &nodes[part].resources;
            assert_eq!(nodes[part].kind, TransferNodeKind::UploadPart);
            assert_eq!(r.disk_read_slots, 1, "upload part is disk-read only");
            assert_eq!(r.disk_write_slots, 0, "upload part never disk-write");
            assert_eq!(r.chunk_slots, 1);
            assert_eq!(r.api_slots, api_slots);
            assert_eq!(
                r.buffer_bytes,
                multipart_part_byte_len(file_size, idx, parts, chunk)
            );
        }
    }

    fn caps_multipart_serial(chunk_size: u64) -> TransferCapabilities {
        let mut caps = caps_multipart(chunk_size);
        caps.max_chunk_slots = Some(1);
        caps
    }

    /// Missing `max_chunk_slots` resolves to effective cap=1 (serial topology).
    fn caps_multipart_missing_slots(chunk_size: u64) -> TransferCapabilities {
        let mut caps = caps_multipart(chunk_size);
        caps.max_chunk_slots = None;
        caps
    }

    #[test]
    fn p0_07_shaped_file_cap1_strict_part_chain() {
        let chunk = 1024 * 1024;
        let file_size = 3 * chunk;
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart_serial(chunk),
            file_size,
        );
        let nodes = built.dag.nodes();
        assert_eq!(built.profile.max_chunk_slots, 1);
        assert_serial_part_chain(nodes, built.acquire, &built.transfer, built.verify);
        assert_p0_06_part_accounting(nodes, &built.transfer, file_size, 3, chunk, 0);
    }

    #[test]
    fn p0_07_batch_shaped_cap1_per_file_serial_no_cross_file_deps() {
        let chunk = 8 * 1024 * 1024;
        let file_size = 24 * 1024 * 1024; // 3 parts each
        let caps = caps_multipart_serial(chunk);
        let items = vec![
            BatchDagItem::with_size("a.bin", TransferDirection::Upload, file_size),
            BatchDagItem::with_size("b.bin", TransferDirection::Upload, file_size),
        ];
        let built = TransferDagBuilder::from_batch_shaped(&items, &caps);
        assert_eq!(built.files.len(), 2);
        let nodes = built.dag.nodes();

        let a_parts: std::collections::HashSet<usize> =
            built.files[0].transfer_nodes.iter().copied().collect();
        let b_parts: std::collections::HashSet<usize> =
            built.files[1].transfer_nodes.iter().copied().collect();
        assert!(a_parts.is_disjoint(&b_parts));

        for file in &built.files {
            assert_eq!(file.transfer_nodes.len(), 3);
            assert_serial_part_chain(nodes, file.acquire, &file.transfer_nodes, file.verify);
            // No part may depend on a part belonging to another file.
            let own: std::collections::HashSet<usize> =
                file.transfer_nodes.iter().copied().collect();
            let other: std::collections::HashSet<usize> = built
                .files
                .iter()
                .filter(|f| f.key != file.key)
                .flat_map(|f| f.transfer_nodes.iter().copied())
                .collect();
            for &part in &file.transfer_nodes {
                for &dep in &nodes[part].depends_on {
                    assert!(
                        !other.contains(&dep),
                        "part of {} must not depend on another file's part",
                        file.key
                    );
                    assert!(
                        dep == file.acquire || own.contains(&dep),
                        "part deps must stay within this file's acquire/chain"
                    );
                }
            }
            assert_p0_06_part_accounting(nodes, &file.transfer_nodes, file_size, 3, chunk, 0);
        }

        // Files remain independent roots after discover: no edge from one
        // file's acquire/transfer into the other file's sub-DAG.
        assert_ne!(built.files[0].acquire, built.files[1].acquire);
        assert!(nodes[built.files[0].acquire]
            .depends_on
            .iter()
            .all(|&d| d != built.files[1].acquire));
        assert!(nodes[built.files[1].acquire]
            .depends_on
            .iter()
            .all(|&d| d != built.files[0].acquire));
    }

    #[test]
    fn p0_07_sync_shaped_cap1_per_file_serial_no_cross_file_deps() {
        let chunk = 8 * 1024 * 1024;
        let file_size = 24 * 1024 * 1024;
        let caps = caps_multipart_serial(chunk);
        let items = vec![
            SyncDagItem::with_size("a.bin", SyncDagAction::Upload, file_size),
            SyncDagItem::with_size("b.bin", SyncDagAction::Upload, file_size),
        ];
        let built = TransferDagBuilder::from_sync_plan_shaped(&items, &caps);
        assert_eq!(built.files.len(), 2);
        let nodes = built.dag.nodes();

        // Global compare prefix is shared; per-file chains hang below it.
        for file in &built.files {
            assert_eq!(nodes[file.acquire].depends_on, vec![built.compare]);
            assert_eq!(file.transfer_nodes.len(), 3);
            assert_serial_part_chain(nodes, file.acquire, &file.transfer_nodes, file.verify);
            let other: std::collections::HashSet<usize> = built
                .files
                .iter()
                .filter(|f| f.key != file.key)
                .flat_map(|f| f.transfer_nodes.iter().copied())
                .collect();
            for &part in &file.transfer_nodes {
                for &dep in &nodes[part].depends_on {
                    assert!(
                        !other.contains(&dep),
                        "sync part of {} must not depend on another file's part",
                        file.key
                    );
                }
            }
            assert_p0_06_part_accounting(nodes, &file.transfer_nodes, file_size, 3, chunk, 0);
        }
    }

    #[test]
    fn p0_07_all_shaped_surfaces_cap_gt1_fan_out() {
        let chunk = 1024 * 1024;
        let file_size = 4 * chunk;
        let caps = caps_multipart(chunk); // max_chunk_slots = 4

        let single = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, file_size);
        assert_eq!(single.profile.max_chunk_slots, 4);
        assert_fanout_parts(single.dag.nodes(), single.acquire, &single.transfer);

        let batch = TransferDagBuilder::from_batch_shaped(
            &[
                BatchDagItem::with_size("a.bin", TransferDirection::Upload, file_size),
                BatchDagItem::with_size("b.bin", TransferDirection::Upload, file_size),
            ],
            &caps,
        );
        for file in &batch.files {
            assert_fanout_parts(batch.dag.nodes(), file.acquire, &file.transfer_nodes);
        }

        let sync = TransferDagBuilder::from_sync_plan_shaped(
            &[
                SyncDagItem::with_size("a.bin", SyncDagAction::Upload, file_size),
                SyncDagItem::with_size("b.bin", SyncDagAction::Upload, file_size),
            ],
            &caps,
        );
        for file in &sync.files {
            assert_fanout_parts(sync.dag.nodes(), file.acquire, &file.transfer_nodes);
        }
    }

    #[test]
    fn p0_07_missing_max_chunk_slots_follows_serial_topology() {
        let chunk = 1024 * 1024;
        let file_size = 3 * chunk;
        let caps = caps_multipart_missing_slots(chunk);

        let single = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, file_size);
        assert_eq!(
            single.profile.max_chunk_slots, 1,
            "None max_chunk_slots resolves to effective cap=1"
        );
        assert_serial_part_chain(
            single.dag.nodes(),
            single.acquire,
            &single.transfer,
            single.verify,
        );

        let batch = TransferDagBuilder::from_batch_shaped(
            &[BatchDagItem::with_size(
                "a.bin",
                TransferDirection::Upload,
                file_size,
            )],
            &caps,
        );
        let f = &batch.files[0];
        assert_serial_part_chain(batch.dag.nodes(), f.acquire, &f.transfer_nodes, f.verify);

        let sync = TransferDagBuilder::from_sync_plan_shaped(
            &[SyncDagItem::with_size(
                "a.bin",
                SyncDagAction::Upload,
                file_size,
            )],
            &caps,
        );
        let s = &sync.files[0];
        assert_serial_part_chain(sync.dag.nodes(), s.acquire, &s.transfer_nodes, s.verify);
    }

    #[test]
    fn p0_07_shaped_surfaces_preserve_p0_06_buffer_and_api_slots() {
        let chunk = 8 * 1024 * 1024;
        let file_size = 25 * 1024 * 1024; // 4 parts, short tail on last
        let mut caps = caps_multipart(chunk);
        caps.rate_limited_api = Capability::Supported;
        let expected_api = 1u16;
        let parts = 4usize;

        let single = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, file_size);
        assert_eq!(single.profile.api_slots, expected_api);
        assert_p0_06_part_accounting(
            single.dag.nodes(),
            &single.transfer,
            file_size,
            parts,
            chunk,
            expected_api,
        );

        let batch = TransferDagBuilder::from_batch_shaped(
            &[BatchDagItem::with_size(
                "big.bin",
                TransferDirection::Upload,
                file_size,
            )],
            &caps,
        );
        assert_p0_06_part_accounting(
            batch.dag.nodes(),
            &batch.files[0].transfer_nodes,
            file_size,
            parts,
            chunk,
            expected_api,
        );

        let sync = TransferDagBuilder::from_sync_plan_shaped(
            &[SyncDagItem::with_size(
                "big.bin",
                SyncDagAction::Upload,
                file_size,
            )],
            &caps,
        );
        assert_p0_06_part_accounting(
            sync.dag.nodes(),
            &sync.files[0].transfer_nodes,
            file_size,
            parts,
            chunk,
            expected_api,
        );
    }

    #[tokio::test]
    async fn p0_07_executor_cap1_observes_monotone_part_order() {
        let chunk = 1024 * 1024;
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart_serial(chunk),
            4 * chunk,
        );
        // Map part node id → part index in transfer_nodes order (opaque ids).
        let part_order: std::collections::HashMap<usize, usize> = built
            .transfer
            .iter()
            .enumerate()
            .map(|(idx, &id)| (id, idx))
            .collect();
        let observed: Arc<std::sync::Mutex<Vec<usize>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_run = Arc::clone(&observed);
        let part_order_run = part_order.clone();
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let observed = Arc::clone(&observed_run);
            let part_order = part_order_run.clone();
            Box::pin(async move {
                if let Some(&idx) = part_order.get(&node.id) {
                    // Hold briefly so a racey fan-out would reorder; serial
                    // deps force monotone completion order regardless.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    observed.lock().expect("lock").push(idx);
                }
                NodeOutcome::Completed
            })
        });
        let manager = TransferResourceManager::new(TransferBudget {
            chunk_slots: 4, // budget allows overlap; topology must still serialise
            ..TransferBudget::from_file_slots(1)
        });

        let summary = execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("serial multipart must run cleanly");
        assert_eq!(summary.nodes_failed, 0);

        let order = observed.lock().expect("lock").clone();
        assert_eq!(
            order,
            vec![0, 1, 2, 3],
            "cap=1 must dispatch parts in order"
        );
    }

    #[tokio::test]
    async fn p0_07_executor_cap_gt1_can_overlap_parts() {
        let chunk = 1024 * 1024;
        let built = TransferDagBuilder::shaped_file(
            TransferDirection::Upload,
            &caps_multipart(chunk), // max_chunk_slots = 4
            4 * chunk,
        );
        let part_ids: std::collections::HashSet<usize> = built.transfer.iter().copied().collect();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let in_flight_run = Arc::clone(&in_flight);
        let peak_run = Arc::clone(&peak);
        let part_ids_run = part_ids.clone();
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let in_flight = Arc::clone(&in_flight_run);
            let peak = Arc::clone(&peak_run);
            let part_ids = part_ids_run.clone();
            Box::pin(async move {
                if part_ids.contains(&node.id) {
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                }
                NodeOutcome::Completed
            })
        });
        // chunk_slots + disk_read_slots must both allow overlap: UploadPart
        // reserves one of each, and from_file_slots(1) defaults disk_read=1.
        let manager = TransferResourceManager::new(TransferBudget {
            chunk_slots: 4,
            disk_read_slots: 4,
            buffer_bytes: 64 * 1024 * 1024,
            ..TransferBudget::from_file_slots(1)
        });

        let summary = execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("fan-out multipart must run cleanly");
        assert_eq!(summary.nodes_failed, 0);
        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "cap>1 fan-out must allow overlapping parts (peak={})",
            peak.load(Ordering::SeqCst)
        );
        assert!(
            peak.load(Ordering::SeqCst) <= 4,
            "overlap remains bounded by chunk/disk budgets (peak={})",
            peak.load(Ordering::SeqCst)
        );
    }
}
