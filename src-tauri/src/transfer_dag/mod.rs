// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Core DAG transfer engine primitives.
//!
//! This module is intentionally additive in the first production slice: the
//! existing GUI/CLI transfer paths can keep their public behavior while the
//! scheduler, capability model, and resource leases become shared vocabulary.

pub mod adaptive;
pub mod aimd_hints;
pub mod builder;
pub mod capabilities;
pub mod checkpoint;
pub mod error;
pub mod executor;
pub mod governor;
pub mod graph;
pub mod metrics;
pub mod observer;
pub mod planner;
pub mod probe;
pub mod resources;
pub mod session_pool;

pub use adaptive::{
    congestion_from_error, global_profile_registry, AdaptiveClass, AdaptiveClock,
    AdaptiveProfileConfig, AdaptiveProfileKey, AdaptiveProfileRegistry, AdaptiveProfileSnapshot,
    AdaptiveWorkload, AimdClassOverrides, AimdClassWindow, AimdConfig, AimdController,
    CongestionEvent, ManualClock, SystemClock,
};
pub use aimd_hints::AimdHint;
pub use builder::{
    BatchDag, BatchDagItem, BatchFileDag, CopyDag, ShapedFileDag, ShapedRangesDag, SingleFileDag,
    SyncDag, SyncDagAction, SyncDagItem, SyncFileDag, TransferDagBuilder, TransferDirection,
    TransferGraphProfile,
};
pub use capabilities::{Capability, TransferCapabilities};
pub use checkpoint::{
    CheckpointDestinationIdentity, CheckpointLayout, CheckpointOpen, CheckpointPartReceipt,
    CheckpointSourceIdentity, CheckpointStatus, MultipartCheckpoint, ObservedSource,
    TransferCheckpointStore, CHECKPOINT_SCHEMA_VERSION,
};
pub use error::{FailureScope, RetryDirective, TransferError, TransferErrorKind};
pub use executor::{
    execute_dag, execute_dag_with_dispatch_window, execute_dag_with_options, DagExecuteOptions,
    DagExecutionError, DagExecutionSummary, DagNodeRunner, NodeFuture, NodeOutcome,
    DEFAULT_DISPATCH_WINDOW, FAIL_FAST_ABORT_GRACE,
};
pub use governor::{
    global as global_governor, init as init_governor, BandwidthBucket, DiskDeviceGovernor,
    DiskDeviceIdentity, DiskDirection, DiskLeaseRequest, DiskOpLease, EndpointGovernor,
    EndpointIdentity, EndpointOpLease, GlobalTransferGovernor, GovernorConfig, GovernorJobLease,
    MemoryPool, TransferPriority, DEFAULT_DISK_DEVICE_SLOTS, DEFAULT_ENDPOINT_MAX_SLOTS,
    DISK_DEVICE_SLOTS_ENV, ENDPOINT_MAX_SLOTS_ENV, GLOBAL_BANDWIDTH_ENV,
};
pub use metrics::TransferDagMetrics;
pub use observer::{
    DagObserver, NoopDagObserver, ObservedOutcome, OrderedDagObserver, SyncJournalDagObserver,
    SyncJournalTerminal,
};
pub use probe::SessionProbeCache;
pub use resources::{
    buffer_bytes_to_quanta, multipart_part_byte_len, request_exceeds_budget,
    resolve_buffer_budget_bytes, ResourceKind, ResourceRequest, TransferBudget,
    TransferResourceManager, BUFFER_BUDGET_ENV, BUFFER_QUANTUM_BYTES, DEFAULT_BUFFER_BUDGET_BYTES,
    MAX_BUFFER_BUDGET_BYTES, MIN_BUFFER_BUDGET_BYTES,
};
pub use session_pool::{
    FtpPoolSessionLease, FtpSessionPoolAdapter, SessionLeaseId, SessionLeaseInfo, SessionLeaseKind,
    SessionPoolCapacity, SessionPoolError, SingleSessionPool, TransferSessionLease,
    TransferSessionPoolHandle,
};
