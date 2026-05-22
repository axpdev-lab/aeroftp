// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Core DAG transfer engine primitives.
//!
//! This module is intentionally additive in the first production slice: the
//! existing GUI/CLI transfer paths can keep their public behavior while the
//! scheduler, capability model, and resource leases become shared vocabulary.

pub mod adaptive;
pub mod builder;
pub mod capabilities;
pub mod executor;
pub mod graph;
pub mod metrics;
pub mod observer;
pub mod planner;
pub mod resources;
pub mod session_pool;

pub use adaptive::{
    congestion_from_error, AdaptiveClass, AimdConfig, AimdController, CongestionEvent,
};
pub use builder::{SingleFileDag, TransferDagBuilder, TransferDirection};
pub use capabilities::{Capability, TransferCapabilities};
pub use observer::{DagObserver, NoopDagObserver, ObservedOutcome};
pub use resources::{ResourceKind, ResourceRequest, TransferBudget, TransferResourceManager};
pub use session_pool::{
    FtpPoolSessionLease, FtpSessionPoolAdapter, SessionLeaseId, SessionLeaseInfo, SessionLeaseKind,
    SessionPoolCapacity, SessionPoolError, SingleSessionPool, TransferSessionLease,
    TransferSessionPoolHandle,
};
