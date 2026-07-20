// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferDagMetrics {
    /// User-visible bytes represented by the logical operation.
    ///
    /// A server-side copy reports the source size here even though no payload
    /// crosses the local client.
    #[serde(default)]
    pub logical_bytes: u64,
    /// Payload bytes carried over the local client's provider data path.
    ///
    /// Native server-side copy is zero. Download-then-upload copy is twice
    /// `logical_bytes` after both legs complete.
    #[serde(default)]
    pub wire_bytes: u64,
    /// Payload bytes materialized locally while executing the operation.
    ///
    /// This stays zero for native copy and equals the temporary file size for
    /// download-then-upload copy.
    #[serde(default)]
    pub local_payload_bytes: u64,
    pub bytes_transferred: u64,
    pub retries: u32,
    pub backpressure_events: u32,
    pub range_fallbacks: u32,
    /// Total time dispatched nodes spent waiting before their runner started
    /// (adaptive-permit plus resource-lease acquisition), summed over nodes.
    #[serde(default)]
    pub wait_nanos_total: u64,
    /// Total runner execution time summed over dispatched nodes. Concurrent
    /// nodes each contribute their full duration, so this can exceed wall
    /// clock; divide by node count for a mean, never treat it as elapsed.
    #[serde(default)]
    pub run_nanos_total: u64,
    /// High-water mark of concurrently dispatched node tasks observed by the
    /// executor scheduling loop for this run.
    #[serde(default)]
    pub slot_peak: u32,
    /// Sum of time-to-first-byte samples measured at provider call
    /// boundaries where a first-byte moment is well defined (single GET/PUT,
    /// multipart part, ranged segment). Divide by `ttfb_samples` for a mean.
    /// Paths that cannot expose a first-byte moment honestly record NOTHING
    /// here; their operation-level latency is already covered by
    /// `run_nanos_total` and is never relabeled as TTFB.
    #[serde(default)]
    pub ttfb_nanos_total: u64,
    /// Number of samples folded into `ttfb_nanos_total`.
    #[serde(default)]
    pub ttfb_samples: u32,
    /// Native copy decisions that degraded to an observed download-upload
    /// graph, including capability-unavailable shaping.
    #[serde(default)]
    pub copy_fallbacks: u32,
}

impl TransferDagMetrics {
    /// Fold another run's metrics into this one. Additive counters sum;
    /// `slot_peak` is a high-water mark, so it keeps the max. Used where a
    /// job-level total aggregates per-subgraph runs (the batch/sync
    /// streaming frontier) so bytes, retries, and timing accumulate across
    /// the whole job rather than reflecting only the last subgraph.
    pub fn absorb(&mut self, other: &TransferDagMetrics) {
        self.logical_bytes = self.logical_bytes.saturating_add(other.logical_bytes);
        self.wire_bytes = self.wire_bytes.saturating_add(other.wire_bytes);
        self.local_payload_bytes = self
            .local_payload_bytes
            .saturating_add(other.local_payload_bytes);
        self.bytes_transferred = self
            .bytes_transferred
            .saturating_add(other.bytes_transferred);
        self.retries = self.retries.saturating_add(other.retries);
        self.backpressure_events = self
            .backpressure_events
            .saturating_add(other.backpressure_events);
        self.range_fallbacks = self.range_fallbacks.saturating_add(other.range_fallbacks);
        self.wait_nanos_total = self.wait_nanos_total.saturating_add(other.wait_nanos_total);
        self.run_nanos_total = self.run_nanos_total.saturating_add(other.run_nanos_total);
        self.slot_peak = self.slot_peak.max(other.slot_peak);
        self.ttfb_nanos_total = self.ttfb_nanos_total.saturating_add(other.ttfb_nanos_total);
        self.ttfb_samples = self.ttfb_samples.saturating_add(other.ttfb_samples);
        self.copy_fallbacks = self.copy_fallbacks.saturating_add(other.copy_fallbacks);
    }
}
