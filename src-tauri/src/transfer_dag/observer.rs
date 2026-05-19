// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `DagObserver`: an `AppHandle`-free observability sink for the transfer
//! engine.
//!
//! Two consumers route through this single abstraction:
//!
//! 1. The ready-frontier executor reports node lifecycle and an accumulated
//!    [`TransferDagMetrics`] snapshot. This is the channel a later slice uses
//!    to expose adaptive concurrency, and the one `PD-BENCH-1` reads for its
//!    report fields.
//! 2. The parallel remote scan emits periodic progress through
//!    [`DagObserver::on_scan_progress`]. The clone-pool scan path has no
//!    `tauri::AppHandle`, so it previously emitted progress only once at the
//!    end (the counter jumped straight to the final value). The observer is
//!    held by the caller (which does have an `AppHandle`); the scan module
//!    only ever sees `&dyn DagObserver`, so no `AppHandle` is threaded into
//!    it. This restores periodic scan progress without that dependency.
//!
//! Every method defaults to a no-op, mirroring the [`crate::transfer_event_sink`]
//! precedent: the CLI / headless / test paths inherit a silent observer for
//! free, and each adapter overrides only the events it cares about. The 1:1
//! adapter property (same event channel, same payload shape) is the
//! non-regression contract for the GUI.

use super::graph::TransferNodeKind;
use super::metrics::TransferDagMetrics;

/// Outcome category reported to [`DagObserver::on_node_complete`]. Mirrors the
/// executor's `NodeOutcome` without coupling the observer to its `String`
/// failure payload (kept observer-object-safe and allocation-free here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedOutcome {
    Completed,
    Fallback,
    Failed,
}

/// Object-safe, `AppHandle`-free observer. Held as `Arc<dyn DagObserver>` and
/// shared across the executor and the scan; every method is a no-op by
/// default so non-GUI callers need not implement any of them.
pub trait DagObserver: Send + Sync {
    /// A node was dispatched onto the ready frontier (its dependencies were
    /// satisfied; resource acquisition happens immediately after). Default:
    /// no-op.
    fn on_node_start(&self, _node_id: usize, _kind: TransferNodeKind) {}

    /// A dispatched node finished with the given outcome. Default: no-op.
    fn on_node_complete(&self, _node_id: usize, _outcome: ObservedOutcome) {}

    /// Periodic remote-scan progress: `scanned` entries collected so far,
    /// `in_flight` directory tasks still running. The GUI adapter maps this
    /// 1:1 onto the existing `sync_scan_progress` event. Default: no-op.
    fn on_scan_progress(&self, _scanned: usize, _in_flight: usize) {}

    /// Final accumulated metrics for a graph run. Default: no-op.
    fn on_metrics(&self, _metrics: &TransferDagMetrics) {}
}

/// Discards every event. The default observer for CLI / headless / tests.
pub struct NoopDagObserver;

impl DagObserver for NoopDagObserver {}

/// Aggregates the final metrics snapshot for later inspection. Used by tests
/// today and by `PD-BENCH-1` to read run totals without an `AppHandle`.
#[derive(Default)]
pub struct CollectingDagObserver {
    metrics: std::sync::Mutex<TransferDagMetrics>,
    scan_progress_calls: std::sync::atomic::AtomicU32,
    last_scanned: std::sync::atomic::AtomicUsize,
}

impl CollectingDagObserver {
    pub fn metrics(&self) -> TransferDagMetrics {
        self.metrics.lock().expect("metrics mutex poisoned").clone()
    }

    pub fn scan_progress_calls(&self) -> u32 {
        self.scan_progress_calls
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn last_scanned(&self) -> usize {
        self.last_scanned.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl DagObserver for CollectingDagObserver {
    fn on_scan_progress(&self, scanned: usize, _in_flight: usize) {
        self.scan_progress_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.last_scanned
            .store(scanned, std::sync::atomic::Ordering::SeqCst);
    }

    fn on_metrics(&self, metrics: &TransferDagMetrics) {
        *self.metrics.lock().expect("metrics mutex poisoned") = metrics.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn noop_observer_is_object_safe_and_silent() {
        let obs: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);
        obs.on_node_start(0, TransferNodeKind::PlanTransfer);
        obs.on_node_complete(0, ObservedOutcome::Completed);
        obs.on_scan_progress(10, 2);
        obs.on_metrics(&TransferDagMetrics::default());
        let cloned = Arc::clone(&obs);
        cloned.on_scan_progress(20, 0);
    }

    #[test]
    fn collecting_observer_records_scan_progress_and_metrics() {
        let obs = Arc::new(CollectingDagObserver::default());
        let dyn_obs: Arc<dyn DagObserver> = Arc::clone(&obs) as Arc<dyn DagObserver>;
        dyn_obs.on_scan_progress(5, 3);
        dyn_obs.on_scan_progress(40, 1);
        dyn_obs.on_metrics(&TransferDagMetrics {
            range_fallbacks: 2,
            ..TransferDagMetrics::default()
        });

        assert_eq!(obs.scan_progress_calls(), 2);
        assert_eq!(obs.last_scanned(), 40);
        assert_eq!(obs.metrics().range_fallbacks, 2);
    }
}
