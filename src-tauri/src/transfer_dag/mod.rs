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
pub mod error;
pub mod executor;
pub mod graph;
pub mod metrics;
pub mod observer;
pub mod planner;
pub mod probe;
pub mod resources;
pub mod session_pool;

pub use adaptive::{
    congestion_from_error, AdaptiveClass, AimdClassOverrides, AimdClassWindow, AimdConfig,
    AimdController, CongestionEvent,
};
pub use aimd_hints::AimdHint;
pub use builder::{
    BatchDag, BatchDagItem, BatchFileDag, CopyDag, ShapedFileDag, ShapedRangesDag, SingleFileDag,
    SyncDag, SyncDagAction, SyncDagItem, SyncFileDag, TransferDagBuilder, TransferDirection,
    TransferGraphProfile,
};
pub use capabilities::{Capability, TransferCapabilities};
pub use error::{FailureScope, RetryDirective, TransferError, TransferErrorKind};
pub use executor::{
    execute_dag, execute_dag_with_dispatch_window, DagExecutionError, DagExecutionSummary,
    DagNodeRunner, NodeFuture, NodeOutcome, DEFAULT_DISPATCH_WINDOW,
};
pub use observer::{
    DagObserver, NoopDagObserver, ObservedOutcome, OrderedDagObserver, SyncJournalDagObserver,
    SyncJournalTerminal,
};
pub use probe::SessionProbeCache;
pub use resources::{ResourceKind, ResourceRequest, TransferBudget, TransferResourceManager};
pub use session_pool::{
    FtpPoolSessionLease, FtpSessionPoolAdapter, SessionLeaseId, SessionLeaseInfo, SessionLeaseKind,
    SessionPoolCapacity, SessionPoolError, SingleSessionPool, TransferSessionLease,
    TransferSessionPoolHandle,
};
