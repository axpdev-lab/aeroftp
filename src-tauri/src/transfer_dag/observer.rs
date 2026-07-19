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
use crate::sync::{JournalEntryStatus, SyncJournal};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Outcome category reported to [`DagObserver::on_node_complete`]. Mirrors the
/// executor's `NodeOutcome` without coupling the observer to the typed
/// [`super::error::TransferError`] failure payload (kept observer-object-safe
/// and allocation-free here).
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

/// Calls child observers in the order they were provided.
///
/// Fase 2 uses this to enforce "durable journal first, visible completion
/// second": put `SyncJournalDagObserver` before GUI/progress observers.
pub struct OrderedDagObserver {
    observers: Vec<Arc<dyn DagObserver>>,
}

impl OrderedDagObserver {
    pub fn new(observers: Vec<Arc<dyn DagObserver>>) -> Self {
        Self { observers }
    }
}

impl DagObserver for OrderedDagObserver {
    fn on_node_start(&self, node_id: usize, kind: TransferNodeKind) {
        for observer in &self.observers {
            observer.on_node_start(node_id, kind);
        }
    }

    fn on_node_complete(&self, node_id: usize, outcome: ObservedOutcome) {
        for observer in &self.observers {
            observer.on_node_complete(node_id, outcome);
        }
    }

    fn on_scan_progress(&self, scanned: usize, in_flight: usize) {
        for observer in &self.observers {
            observer.on_scan_progress(scanned, in_flight);
        }
    }

    fn on_metrics(&self, metrics: &TransferDagMetrics) {
        for observer in &self.observers {
            observer.on_metrics(metrics);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncJournalTerminal {
    pub node_id: usize,
    pub entry_index: usize,
    pub bytes_transferred: Option<u64>,
    pub verified: Option<bool>,
}

impl SyncJournalTerminal {
    pub fn new(node_id: usize, entry_index: usize) -> Self {
        Self {
            node_id,
            entry_index,
            bytes_transferred: None,
            verified: Some(true),
        }
    }

    pub fn with_result(
        node_id: usize,
        entry_index: usize,
        bytes_transferred: u64,
        verified: bool,
    ) -> Self {
        Self {
            node_id,
            entry_index,
            bytes_transferred: Some(bytes_transferred),
            verified: Some(verified),
        }
    }
}

type PersistSyncJournal = dyn Fn(&SyncJournal) -> Result<(), String> + Send + Sync;

/// Updates and persists the legacy sync journal when a per-file terminal DAG
/// node completes.
///
/// The observer intentionally keys off the terminal per-file node
/// (`EmitProgress` in the Fase 2 builders), not `UploadFile` / `DownloadFile`,
/// so durable resume state advances only after verify/metadata/commit nodes
/// have completed.
pub struct SyncJournalDagObserver {
    journal: Arc<Mutex<SyncJournal>>,
    terminals: HashMap<usize, SyncJournalTerminal>,
    persist: Arc<PersistSyncJournal>,
    last_error: Mutex<Option<String>>,
}

impl SyncJournalDagObserver {
    pub fn new(journal: Arc<Mutex<SyncJournal>>, terminals: Vec<SyncJournalTerminal>) -> Self {
        Self::with_persist(journal, terminals, Arc::new(crate::sync::save_sync_journal))
    }

    pub fn with_persist(
        journal: Arc<Mutex<SyncJournal>>,
        terminals: Vec<SyncJournalTerminal>,
        persist: Arc<PersistSyncJournal>,
    ) -> Self {
        Self {
            journal,
            terminals: terminals
                .into_iter()
                .map(|terminal| (terminal.node_id, terminal))
                .collect(),
            persist,
            last_error: Mutex::new(None),
        }
    }

    pub fn journal(&self) -> Arc<Mutex<SyncJournal>> {
        Arc::clone(&self.journal)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .expect("sync journal observer error mutex poisoned")
            .clone()
    }
}

impl DagObserver for SyncJournalDagObserver {
    fn on_node_complete(&self, node_id: usize, outcome: ObservedOutcome) {
        if outcome != ObservedOutcome::Completed {
            return;
        }

        let Some(terminal) = self.terminals.get(&node_id) else {
            return;
        };

        let persist_result = {
            let mut journal = self
                .journal
                .lock()
                .expect("sync journal observer journal mutex poisoned");
            let Some(entry) = journal.entries.get_mut(terminal.entry_index) else {
                let error = format!(
                    "Sync journal terminal node {} references missing entry {}",
                    node_id, terminal.entry_index
                );
                *self
                    .last_error
                    .lock()
                    .expect("sync journal observer error mutex poisoned") = Some(error);
                return;
            };

            entry.status = JournalEntryStatus::Completed;
            if let Some(bytes) = terminal.bytes_transferred {
                entry.bytes_transferred = bytes;
            }
            if let Some(verified) = terminal.verified {
                entry.verified = Some(verified);
            }
            journal.completed = journal.entries.iter().all(|entry| {
                matches!(
                    entry.status,
                    JournalEntryStatus::Completed | JournalEntryStatus::Skipped
                )
            });

            (self.persist)(&journal)
        };

        match persist_result {
            Ok(()) => {
                *self
                    .last_error
                    .lock()
                    .expect("sync journal observer error mutex poisoned") = None;
            }
            Err(error) => {
                *self
                    .last_error
                    .lock()
                    .expect("sync journal observer error mutex poisoned") = Some(error);
            }
        }
    }
}

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
    use crate::sync::{CompareDirection, RetryPolicy, SyncJournalEntry, VerifyPolicy};

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

    fn test_journal() -> SyncJournal {
        let mut journal = SyncJournal::new(
            "/local".to_string(),
            "/remote".to_string(),
            CompareDirection::Bidirectional,
            RetryPolicy::default(),
            VerifyPolicy::SizeOnly,
        );
        journal.entries.push(SyncJournalEntry {
            relative_path: "a.txt".to_string(),
            action: "upload".to_string(),
            status: JournalEntryStatus::Pending,
            attempts: 1,
            last_error: None,
            verified: None,
            bytes_transferred: 0,
            ec_status: None,
        });
        journal.entries.push(SyncJournalEntry {
            relative_path: "b.txt".to_string(),
            action: "download".to_string(),
            status: JournalEntryStatus::Pending,
            attempts: 1,
            last_error: None,
            verified: None,
            bytes_transferred: 0,
            ec_status: None,
        });
        journal
    }

    #[test]
    fn sync_journal_observer_persists_terminal_completion() {
        let journal = Arc::new(Mutex::new(test_journal()));
        let persisted = Arc::new(Mutex::new(Vec::new()));
        let persist: Arc<PersistSyncJournal> = {
            let persisted = Arc::clone(&persisted);
            Arc::new(move |journal| {
                persisted
                    .lock()
                    .expect("persisted mutex poisoned")
                    .push(journal.clone());
                Ok(())
            })
        };
        let observer = SyncJournalDagObserver::with_persist(
            Arc::clone(&journal),
            vec![SyncJournalTerminal::with_result(42, 0, 123, true)],
            persist,
        );

        observer.on_node_complete(42, ObservedOutcome::Completed);

        let journal = journal.lock().expect("journal mutex poisoned");
        assert_eq!(journal.entries[0].status, JournalEntryStatus::Completed);
        assert_eq!(journal.entries[0].bytes_transferred, 123);
        assert_eq!(journal.entries[0].verified, Some(true));
        assert!(!journal.completed, "second entry is still pending");

        let snapshots = persisted.lock().expect("persisted mutex poisoned");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0].entries[0].status,
            JournalEntryStatus::Completed
        );
    }

    #[test]
    fn sync_journal_observer_ignores_non_terminal_or_non_completed_nodes() {
        let journal = Arc::new(Mutex::new(test_journal()));
        let persist_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let persist: Arc<PersistSyncJournal> = {
            let persist_calls = Arc::clone(&persist_calls);
            Arc::new(move |_| {
                persist_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        };
        let observer = SyncJournalDagObserver::with_persist(
            Arc::clone(&journal),
            vec![SyncJournalTerminal::new(42, 0)],
            persist,
        );

        observer.on_node_complete(41, ObservedOutcome::Completed);
        observer.on_node_complete(42, ObservedOutcome::Failed);

        assert_eq!(
            journal.lock().expect("journal mutex poisoned").entries[0].status,
            JournalEntryStatus::Pending
        );
        assert_eq!(persist_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn sync_journal_observer_records_persist_error() {
        let journal = Arc::new(Mutex::new(test_journal()));
        let observer = SyncJournalDagObserver::with_persist(
            journal,
            vec![SyncJournalTerminal::new(42, 0)],
            Arc::new(|_| Err("disk full".to_string())),
        );

        observer.on_node_complete(42, ObservedOutcome::Completed);

        assert_eq!(observer.last_error(), Some("disk full".to_string()));
    }

    struct TraceObserver {
        trace: Arc<Mutex<Vec<&'static str>>>,
        label: &'static str,
    }

    impl DagObserver for TraceObserver {
        fn on_node_complete(&self, _node_id: usize, _outcome: ObservedOutcome) {
            self.trace
                .lock()
                .expect("trace mutex poisoned")
                .push(self.label);
        }
    }

    #[test]
    fn ordered_observer_preserves_journal_before_visible_completion() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let journal = Arc::new(Mutex::new(test_journal()));
        let persist: Arc<PersistSyncJournal> = {
            let trace = Arc::clone(&trace);
            Arc::new(move |_| {
                trace.lock().expect("trace mutex poisoned").push("journal");
                Ok(())
            })
        };
        let journal_observer: Arc<dyn DagObserver> =
            Arc::new(SyncJournalDagObserver::with_persist(
                journal,
                vec![SyncJournalTerminal::new(42, 0)],
                persist,
            ));
        let visible_observer: Arc<dyn DagObserver> = Arc::new(TraceObserver {
            trace: Arc::clone(&trace),
            label: "visible",
        });
        let ordered = OrderedDagObserver::new(vec![journal_observer, visible_observer]);

        ordered.on_node_complete(42, ObservedOutcome::Completed);

        assert_eq!(
            trace.lock().expect("trace mutex poisoned").as_slice(),
            ["journal", "visible"]
        );
    }
}
