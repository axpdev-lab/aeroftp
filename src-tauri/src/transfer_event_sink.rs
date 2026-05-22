// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! TransferEventSink: decouples transfer event emission from `tauri::AppHandle`.
//!
//! `provider_transfer_executor` historically held a `tauri::AppHandle` and
//! called `app.emit("transfer_event", TransferEvent { .. })` directly at every
//! file_start / progress / file_complete / file_error site. That hard
//! dependency on `AppHandle` blocked the CLI file-level batch path
//! (PD-CLI-CONV) from reusing the shared orchestrator executor.
//!
//! This trait moves the emission point behind an abstraction. The GUI
//! implements it as a 1:1 adapter over `app.emit` (byte-identical events,
//! zero frontend regression). The CLI implements a no-op sink: its progress
//! UI is driven by the `indicatif` progress callback wired into
//! `provider.download` / `provider.upload`, not by Tauri events.
//!
//! Mirrors the [`crate::ai_core::EventSink`] precedent already used to make
//! the AI streaming path sink-agnostic for CLI/MCP.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::transfer_dag::graph::TransferNodeKind;
use crate::transfer_dag::{DagObserver, ObservedOutcome};
use crate::transfer_domain::{BatchProgressSnapshot, TransferBatchResult};
use crate::TransferEvent;

/// Abstraction over `transfer_event` emission: Tauri GUI or CLI / headless.
///
/// Every implementor receives the exact [`TransferEvent`] the executor would
/// otherwise have passed to `app.emit("transfer_event", _)`. Implementors
/// MUST NOT reshape or drop fields: the GUI frontend consumes this payload
/// verbatim and the byte-shape is a non-regression contract.
///
/// PD-CLI-CONV-B extends the same single abstraction with the three
/// **batch-lifecycle** events the orchestrator (`execute_batch`) used to
/// emit directly via its own `AppHandle`. They default to no-op so the CLI
/// `NoopTransferSink` and the existing tests inherit them for free; the GUI
/// `AppHandleSink` overrides them as a 1:1 adapter (byte-identical payload,
/// same channel names), which is the non-regression proof.
pub trait TransferEventSink: Send + Sync {
    /// Emit one `transfer_event`.
    ///
    /// - GUI: `app.emit("transfer_event", event)` (fire-and-forget).
    /// - CLI: no-op (progress surfaced via the indicatif progress callback).
    fn emit_transfer_event(&self, event: TransferEvent);

    /// Batch started. GUI: `app.emit("transfer_batch_started", payload)`.
    /// The orchestrator builds `payload` identically regardless of sink, so
    /// the GUI byte-shape is unchanged. Default: no-op (CLI/headless).
    fn emit_batch_started(&self, _payload: serde_json::Value) {}

    /// Per-file batch progress snapshot. GUI:
    /// `app.emit("transfer_batch_progress", snapshot)`. Default: no-op.
    fn emit_batch_progress(&self, _snapshot: &BatchProgressSnapshot) {}

    /// Batch completed. GUI: `app.emit("transfer_batch_completed", result)`.
    /// Default: no-op.
    fn emit_batch_completed(&self, _result: &TransferBatchResult) {}
}

/// GUI sink: 1:1 adapter over [`tauri::AppHandle::emit`]. Produces exactly the
/// same `("transfer_event", TransferEvent)` the executor emitted before the
/// sink abstraction, with the same fire-and-forget error semantics
/// (`let _ = ...`). This 1:1 property is what guarantees the GUI
/// non-regression gate.
pub struct AppHandleSink {
    app: tauri::AppHandle,
}

impl AppHandleSink {
    pub fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }
}

impl TransferEventSink for AppHandleSink {
    fn emit_transfer_event(&self, event: TransferEvent) {
        use tauri::Emitter;
        let _ = self.app.emit("transfer_event", event);
    }

    fn emit_batch_started(&self, payload: serde_json::Value) {
        use tauri::Emitter;
        let _ = self.app.emit("transfer_batch_started", payload);
    }

    fn emit_batch_progress(&self, snapshot: &BatchProgressSnapshot) {
        use tauri::Emitter;
        let _ = self.app.emit("transfer_batch_progress", snapshot);
    }

    fn emit_batch_completed(&self, result: &TransferBatchResult) {
        use tauri::Emitter;
        let _ = self.app.emit("transfer_batch_completed", result);
    }
}

/// CLI / headless sink: discards transfer events. The CLI batch path renders
/// progress through the indicatif callback passed into `provider.download` /
/// `provider.upload`, so Tauri-shaped events have no consumer there. Keeping
/// emission as a no-op (rather than removing it from the executor) preserves a
/// single shared executor code path.
pub struct NoopTransferSink;

impl TransferEventSink for NoopTransferSink {
    fn emit_transfer_event(&self, _event: TransferEvent) {}
}

/// Format a byte count the way the legacy single-file `transfer_event`
/// completion message does: one decimal, MB above 1 MiB, KB below. Kept
/// character-for-character identical so the GUI completion text is unchanged
/// whether the legacy path or the DAG path produced it.
fn format_complete_size(bytes: u64) -> String {
    if bytes > 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

/// [`DagObserver`] adapter that emits the GUI single-file **completion**
/// event when a single-file transfer graph reaches its terminal node.
///
/// ## Why only the completion event
///
/// The DAG-ENGINE phase-1 routing keeps the single-file `transfer_event`
/// stream byte-identical with the legacy path:
///
/// - `"start"` is emitted by the shared pre-DAG legacy code (it must precede
///   the delta attempt), so the observer must NOT emit it again.
/// - `"progress"` is emitted by the same per-byte callback handed straight to
///   `provider.download` / `provider.upload`, so it never routes through the
///   observer either.
/// - `"error"` carries the failure message, but [`DagObserver::on_node_complete`]
///   intentionally drops the failure payload (it is observer-object-safe and
///   allocation-free). The routing layer holds the typed [`crate::providers::ProviderError`]
///   and emits `"error"` itself, exactly as it owns the `"cancelled"` decision.
/// - `"complete"` is the one event with no upstream emitter on the DAG path:
///   the terminal `EmitProgress` node completing is precisely "the transfer
///   finished successfully". That is what this observer maps.
///
/// The completion event is built character-for-character like the legacy one
/// (`"({size} in 0s)"`, MB/KB via [`format_complete_size`], the original
/// `fallback_reason`), so a frontend cannot tell the two paths apart.
pub struct GuiDagObserver {
    sink: Arc<dyn TransferEventSink>,
    transfer_id: String,
    filename: String,
    /// `"download"` or `"upload"`.
    direction: String,
    /// Id of the terminal `EmitProgress` node in the single-file graph
    /// (`SingleFileDag::emit_progress`). Its completion is the success signal.
    emit_progress_node_id: usize,
    /// Size to report in the completion message. The download transfer node
    /// stores the real on-disk size here; the upload caller seeds it with the
    /// known local size.
    report_size: Arc<AtomicU64>,
    /// Carried verbatim into the completion event: a non-`None` value means an
    /// earlier delta attempt fell back silently to the classic transfer.
    fallback_reason: Option<String>,
}

impl GuiDagObserver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sink: Arc<dyn TransferEventSink>,
        transfer_id: String,
        filename: String,
        direction: String,
        emit_progress_node_id: usize,
        report_size: Arc<AtomicU64>,
        fallback_reason: Option<String>,
    ) -> Self {
        Self {
            sink,
            transfer_id,
            filename,
            direction,
            emit_progress_node_id,
            report_size,
            fallback_reason,
        }
    }
}

impl DagObserver for GuiDagObserver {
    fn on_node_complete(&self, node_id: usize, outcome: ObservedOutcome) {
        // Only the terminal node completing means the whole single-file graph
        // succeeded: every upstream node is its transitive dependency.
        if node_id != self.emit_progress_node_id {
            return;
        }
        if !matches!(
            outcome,
            ObservedOutcome::Completed | ObservedOutcome::Fallback
        ) {
            return;
        }
        let size = self.report_size.load(Ordering::SeqCst);
        self.sink.emit_transfer_event(TransferEvent {
            event_type: "complete".to_string(),
            transfer_id: self.transfer_id.clone(),
            filename: self.filename.clone(),
            direction: self.direction.clone(),
            message: Some(format!("({} in 0s)", format_complete_size(size))),
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: self.fallback_reason.clone(),
        });
    }

    fn on_node_start(&self, _node_id: usize, _kind: TransferNodeKind) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sample_event() -> TransferEvent {
        TransferEvent {
            event_type: "file_start".to_string(),
            transfer_id: "t1".to_string(),
            filename: "f.bin".to_string(),
            direction: "download".to_string(),
            message: None,
            progress: None,
            path: Some("/remote/f.bin".to_string()),
            delta_stats: None,
            fallback_reason: None,
        }
    }

    #[test]
    fn noop_sink_is_object_safe_and_does_not_panic() {
        // The executor holds `Arc<dyn TransferEventSink>`; the trait must be
        // object-safe and the CLI no-op sink must silently absorb events.
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        sink.emit_transfer_event(sample_event());
        let cloned = Arc::clone(&sink);
        cloned.emit_transfer_event(sample_event());
    }

    /// Test sink: records every emitted `transfer_event` for assertions.
    #[derive(Default)]
    struct RecordingSink {
        events: std::sync::Mutex<Vec<TransferEvent>>,
    }

    impl RecordingSink {
        fn events(&self) -> Vec<TransferEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl TransferEventSink for RecordingSink {
        fn emit_transfer_event(&self, event: TransferEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn gui_observer(
        sink: Arc<RecordingSink>,
        direction: &str,
        emit_progress_node_id: usize,
        report_size: Arc<AtomicU64>,
        fallback_reason: Option<String>,
    ) -> GuiDagObserver {
        GuiDagObserver::new(
            sink as Arc<dyn TransferEventSink>,
            "pdl-123".to_string(),
            "f.bin".to_string(),
            direction.to_string(),
            emit_progress_node_id,
            report_size,
            fallback_reason,
        )
    }

    #[test]
    fn gui_observer_emits_complete_only_on_the_terminal_node() {
        let sink = Arc::new(RecordingSink::default());
        let obs = gui_observer(
            Arc::clone(&sink),
            "download",
            6,
            Arc::new(AtomicU64::new(2_097_152)),
            None,
        );

        // Non-terminal completions emit nothing.
        for node_id in 0..6 {
            obs.on_node_complete(node_id, ObservedOutcome::Completed);
        }
        assert!(sink.events().is_empty(), "only the terminal node emits");

        // The terminal node completing emits exactly one "complete" event.
        obs.on_node_complete(6, ObservedOutcome::Completed);
        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "complete");
        assert_eq!(events[0].transfer_id, "pdl-123");
        assert_eq!(events[0].direction, "download");
        // 2 MiB formats as MB with one decimal, "in 0s" suffix.
        assert_eq!(events[0].message.as_deref(), Some("(2.0 MB in 0s)"));
        assert_eq!(events[0].fallback_reason, None);
    }

    #[test]
    fn gui_observer_complete_message_uses_kb_below_one_mib() {
        let sink = Arc::new(RecordingSink::default());
        let obs = gui_observer(
            Arc::clone(&sink),
            "upload",
            6,
            Arc::new(AtomicU64::new(512_000)),
            None,
        );
        obs.on_node_complete(6, ObservedOutcome::Completed);
        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].direction, "upload");
        assert_eq!(events[0].message.as_deref(), Some("(500.0 KB in 0s)"));
    }

    #[test]
    fn gui_observer_carries_the_delta_fallback_reason() {
        let sink = Arc::new(RecordingSink::default());
        let obs = gui_observer(
            Arc::clone(&sink),
            "download",
            6,
            Arc::new(AtomicU64::new(1024)),
            Some("rsync binary not found on remote".to_string()),
        );
        obs.on_node_complete(6, ObservedOutcome::Completed);
        let events = sink.events();
        assert_eq!(
            events[0].fallback_reason.as_deref(),
            Some("rsync binary not found on remote"),
        );
    }

    #[test]
    fn gui_observer_stays_silent_on_a_failed_terminal_outcome() {
        // A failed terminal node must not produce a "complete" event; the
        // routing layer owns the "error" event because it holds the message.
        let sink = Arc::new(RecordingSink::default());
        let obs = gui_observer(
            Arc::clone(&sink),
            "download",
            6,
            Arc::new(AtomicU64::new(1024)),
            None,
        );
        obs.on_node_complete(6, ObservedOutcome::Failed);
        assert!(sink.events().is_empty());
    }

    #[test]
    fn gui_observer_start_is_a_silent_noop() {
        // "start" is emitted by the shared legacy code before the DAG runs;
        // the observer must not duplicate it.
        let sink = Arc::new(RecordingSink::default());
        let obs = gui_observer(
            Arc::clone(&sink),
            "download",
            6,
            Arc::new(AtomicU64::new(1024)),
            None,
        );
        obs.on_node_start(0, TransferNodeKind::DiscoverRemote);
        obs.on_node_start(6, TransferNodeKind::EmitProgress);
        assert!(sink.events().is_empty());
    }
}
