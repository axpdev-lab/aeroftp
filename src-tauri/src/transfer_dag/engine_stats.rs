// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-P2-07 (block E): process-global "most recent engine job" snapshot.
//!
//! The CLI and GUI read a completed job's [`EngineTransferStats`] in-band (the
//! CLI from the batch result, the GUI from the job-end event), so they always
//! attribute stats to the exact job they ran. The MCP server has no such
//! in-band channel: its own `aeroftp_transfer`/`transfer_tree` tools go through
//! `cross_profile_transfer` and the `RemoteBackend` trait, NOT the DAG engine,
//! so they never produce a [`TransferDagMetrics`]. This tiny store is the honest
//! bridge for that surface: every DAG-engine batch/sync job publishes its final
//! snapshot here, and the MCP `aeroftp_transfer_stats` accessor reads the latest.
//!
//! It is bounded to exactly one slot (the last job wins), holds no history, and
//! carries only the same additive read-model the other surfaces expose. When no
//! DAG-engine job has run in this process it reports `None`, never a fabricated
//! zero job.

use std::sync::{Mutex, OnceLock};

use super::metrics::EngineTransferStats;

fn slot() -> &'static Mutex<Option<EngineTransferStats>> {
    static LATEST: OnceLock<Mutex<Option<EngineTransferStats>>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(None))
}

/// Publish the most recent completed DAG-engine job's stats. Overwrites the
/// single slot; the previous snapshot is dropped.
pub fn publish(stats: EngineTransferStats) {
    let mut guard = slot().lock().expect("engine-stats slot poisoned");
    *guard = Some(stats);
}

/// The most recent published DAG-engine job stats, or `None` when no such job
/// has run in this process. A clone so the caller never holds the lock.
pub fn latest() -> Option<EngineTransferStats> {
    slot().lock().expect("engine-stats slot poisoned").clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::TransferDagMetrics;

    #[test]
    fn publish_makes_a_snapshot_available_to_latest() {
        let metrics = TransferDagMetrics {
            bytes_transferred: 4242,
            slot_peak: 3,
            ..Default::default()
        };
        publish(EngineTransferStats::from_job(metrics, 1234, None));
        // The slot is process-global and sibling tests run on parallel threads,
        // so another writer may overwrite it before this read. Assert only the
        // race-free contract: once any job has published, `latest` is `Some`
        // and never regresses to `None`.
        assert!(latest().is_some());
    }
}
