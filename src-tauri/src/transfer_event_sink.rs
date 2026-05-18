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
}
